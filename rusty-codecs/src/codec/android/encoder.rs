//! Android MediaCodec H.264 encoder.
//!
//! Uses the NDK `AMediaCodec` API in synchronous ByteBuffer mode. Input
//! frames are converted to NV12 and copied into MediaCodec input buffers;
//! output is collected as Annex B NAL units.

use std::{collections::VecDeque, time::Duration};

use anyhow::{Context, Result};
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};

use super::format::{
    BUFFER_FLAG_CODEC_CONFIG, BUFFER_FLAG_KEY_FRAME, DEQUEUE_TIMEOUT, H264_BPP, encoder_format,
    extract_csd_from_format,
};
use crate::{
    codec::h264::annexb::{build_avcc, extract_sps_pps, parse_annex_b},
    config::{H264, VideoCodec, VideoConfig},
    format::{EncodedFrame, NalFormat, ScaleMode, VideoEncoderConfig, VideoFrame},
    processing::convert::pixel_format_to_nv12,
    processing::scale::Scaler,
    traits::{VideoEncoder, VideoEncoderFactory},
};

/// Maximum consecutive codec errors before attempting a reset.
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Android MediaCodec H.264 encoder using synchronous ByteBuffer mode.
///
/// Accepts [`VideoFrame`] input in any supported pixel format, converts to
/// NV12 internally, and produces Annex B encoded output. SPS/PPS are
/// extracted from `INFO_OUTPUT_FORMAT_CHANGED` or from the first keyframe's
/// codec-config buffer.
#[derive(derive_more::Debug)]
pub struct AndroidEncoder {
    #[debug(skip)]
    codec: MediaCodec,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    keyframe_interval_secs: f32,
    frame_count: u64,
    nal_format: NalFormat,
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    /// avcC description, populated when SPS/PPS are first seen.
    avcc: Option<Vec<u8>>,
    /// Encoded packets ready for collection.
    packet_buf: VecDeque<EncodedFrame>,
    /// Consecutive error counter for reset logic.
    consecutive_errors: u32,
}

// Safety: MediaCodec instances are thread-safe in the NDK. The synchronous
// ByteBuffer API can be called from any single thread.
unsafe impl Send for AndroidEncoder {}

impl AndroidEncoder {
    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(H264_BPP);
        let nal_format = config.nal_format;
        let keyframe_interval = config.keyframe_interval_or_default();
        let keyframe_interval_secs = keyframe_interval as f32 / framerate as f32;

        let codec = MediaCodec::from_encoder_type(super::format::MIME_AVC)
            .context("failed to create Android H.264 encoder")?;

        let format = encoder_format(width, height, bitrate, framerate, keyframe_interval_secs)?;

        codec
            .configure(&format, None, MediaCodecDirection::Encoder)
            .map_err(|e| anyhow::anyhow!("MediaCodec configure failed: {e:?}"))?;

        codec
            .start()
            .map_err(|e| anyhow::anyhow!("MediaCodec start failed: {e:?}"))?;

        tracing::info!(
            width,
            height,
            framerate,
            bitrate,
            "H.264 Android MediaCodec encoder ready"
        );

        Ok(Self {
            codec,
            width,
            height,
            framerate,
            bitrate,
            keyframe_interval_secs,
            frame_count: 0,
            nal_format,
            scale_mode: config.scale_mode,
            scaler: Scaler::new(Some((width, height))),
            avcc: None,
            packet_buf: VecDeque::new(),
            consecutive_errors: 0,
        })
    }

    /// Scales the frame to encoder dimensions if needed.
    fn scale_if_needed(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        let [fw, fh] = frame.dimensions;
        if fw == self.width && fh == self.height {
            return Ok(frame);
        }
        let (tw, th) = self.scale_mode.resolve((fw, fh), (self.width, self.height));
        if tw == fw && th == fh {
            return Ok(frame);
        }
        self.scaler.set_target_dimensions(tw, th);
        let img = frame.rgba_image();
        let scaled = if self.scale_mode == ScaleMode::Cover {
            self.scaler.scale_cover_rgba(img.as_raw(), fw, fh)?
        } else {
            self.scaler.scale_rgba(img.as_raw(), fw, fh)?
        };
        match scaled {
            Some((data, w, h)) => Ok(VideoFrame::new_rgba(data.into(), w, h, frame.timestamp)),
            None => Ok(frame),
        }
    }

    /// Submits NV12 data to the next available input buffer.
    fn submit_input(&self, nv12: &[u8], timestamp_us: u64) -> Result<()> {
        match self
            .codec
            .dequeue_input_buffer(DEQUEUE_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("dequeue input buffer failed: {e:?}"))?
        {
            DequeuedInputBufferResult::Buffer(mut input_buf) => {
                let buf = input_buf.buffer_mut();
                if nv12.len() > buf.len() {
                    anyhow::bail!(
                        "NV12 frame ({} bytes) exceeds MediaCodec input buffer ({} bytes)",
                        nv12.len(),
                        buf.len()
                    );
                }
                let copy_len = nv12.len();
                // Safety: writing valid NV12 bytes into MaybeUninit buffer.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        nv12.as_ptr(),
                        buf.as_mut_ptr() as *mut u8,
                        copy_len,
                    );
                }
                self.codec
                    .queue_input_buffer(input_buf, 0, copy_len, timestamp_us, 0)
                    .map_err(|e| anyhow::anyhow!("queue input buffer failed: {e:?}"))?;
                Ok(())
            }
            DequeuedInputBufferResult::TryAgainLater => {
                // In synchronous mode, this means the codec's input buffers
                // are all full. Drop the frame rather than blocking.
                tracing::trace!("MediaCodec input buffer not available, dropping frame");
                Ok(())
            }
        }
    }

    /// Drains all available output buffers into `packet_buf`.
    fn drain_output(&mut self) -> Result<()> {
        loop {
            match self
                .codec
                .dequeue_output_buffer(DEQUEUE_TIMEOUT)
                .map_err(|e| anyhow::anyhow!("dequeue output buffer failed: {e:?}"))?
            {
                DequeuedOutputBufferInfoResult::Buffer(output_buf) => {
                    let info = *output_buf.info();
                    let flags = info.flags();
                    let is_config = (flags & BUFFER_FLAG_CODEC_CONFIG) != 0;
                    let is_keyframe = (flags & BUFFER_FLAG_KEY_FRAME) != 0;

                    let data = output_buf.buffer().to_vec();
                    let presentation_time_us = info.presentation_time_us().max(0) as u64;

                    // Release the buffer back to the codec before processing.
                    self.codec
                        .release_output_buffer(output_buf, false)
                        .map_err(|e| anyhow::anyhow!("release output buffer failed: {e:?}"))?;

                    // Codec-config buffers contain SPS/PPS, not media data.
                    if is_config {
                        self.try_extract_sps_pps_from_data(&data);
                        continue;
                    }

                    // Extract SPS/PPS from first keyframe if not yet available.
                    if is_keyframe && self.avcc.is_none() {
                        self.try_extract_sps_pps_from_data(&data);
                    }

                    // Convert to the requested NAL format.
                    let payload: bytes::Bytes = match self.nal_format {
                        NalFormat::AnnexB => data.into(),
                        NalFormat::Avcc => {
                            crate::codec::h264::annexb::annex_b_to_length_prefixed(&data).into()
                        }
                    };

                    self.packet_buf.push_back(EncodedFrame {
                        is_keyframe,
                        timestamp: Duration::from_micros(presentation_time_us),
                        payload,
                    });

                    self.consecutive_errors = 0;
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    // New output format available — extract SPS/PPS from it.
                    let format = self.codec.output_format();
                    if let Some((sps, pps)) = extract_csd_from_format(&format) {
                        self.avcc = Some(build_avcc(&sps, &pps));
                    }
                }
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => {
                    // Deprecated signal, just continue draining.
                    continue;
                }
                DequeuedOutputBufferInfoResult::TryAgainLater => {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Attempts to extract SPS/PPS from Annex B data and build an avcC record.
    fn try_extract_sps_pps_from_data(&mut self, data: &[u8]) {
        let nals = parse_annex_b(data);
        if let Some((sps, pps)) = extract_sps_pps(&nals) {
            self.avcc = Some(build_avcc(&sps, &pps));
        }
    }

    /// Attempts to reset the codec after repeated errors.
    fn try_reset(&mut self) -> Result<()> {
        tracing::warn!("resetting Android MediaCodec encoder after consecutive errors");
        let _ = self.codec.stop();
        let format = encoder_format(
            self.width,
            self.height,
            self.bitrate,
            self.framerate,
            self.keyframe_interval_secs,
        )?;
        self.codec
            .configure(&format, None, MediaCodecDirection::Encoder)
            .map_err(|e| anyhow::anyhow!("MediaCodec reconfigure failed: {e:?}"))?;
        self.codec
            .start()
            .map_err(|e| anyhow::anyhow!("MediaCodec restart failed: {e:?}"))?;
        self.consecutive_errors = 0;
        Ok(())
    }
}

impl VideoEncoderFactory for AndroidEncoder {
    const ID: &str = "h264-android";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(H264_BPP);
        let inline = config.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline,
            }),
            description: None,
            coded_width: Some(config.width),
            coded_height: Some(config.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(bitrate),
            framerate: Some(config.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }
}

impl VideoEncoder for AndroidEncoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E, // Level 3.0
                inline,
            }),
            description: self.avcc.clone().map(Into::into),
            coded_width: Some(self.width),
            coded_height: Some(self.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(self.bitrate),
            framerate: Some(self.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        let frame = self.scale_if_needed(frame)?;
        let [w, h] = frame.dimensions;

        // Convert to NV12 for MediaCodec's COLOR_FormatYUV420SemiPlanar.
        let nv12_buf: Vec<u8> = match &frame.data {
            crate::format::FrameData::Packed { pixel_format, data } => {
                pixel_format_to_nv12(data, w, h, *pixel_format)?.into_contiguous()
            }
            crate::format::FrameData::Nv12(planes) => {
                // Already NV12 — concatenate Y and UV planes.
                let mut buf = Vec::with_capacity(planes.y_data.len() + planes.uv_data.len());
                buf.extend_from_slice(&planes.y_data);
                buf.extend_from_slice(&planes.uv_data);
                buf
            }
            _ => {
                // GPU or I420 frames: fall back through RGBA.
                let img = frame.rgba_image();
                pixel_format_to_nv12(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
                    .into_contiguous()
            }
        };

        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;

        if let Err(e) = self.submit_input(&nv12_buf, timestamp_us) {
            self.consecutive_errors += 1;
            if self.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                self.try_reset()?;
            }
            return Err(e);
        }
        self.frame_count += 1;

        // Drain any available output.
        if let Err(e) = self.drain_output() {
            self.consecutive_errors += 1;
            if self.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                self.try_reset()?;
            }
            return Err(e);
        }

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        // Try draining once more before returning.
        let _ = self.drain_output();
        Ok(self.packet_buf.pop_front())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        // AMediaCodec_setParameters requires API level 26+. Without the
        // ndk/api-level-26 feature we cannot call it at compile time, so
        // we store the new target and apply it on the next codec reset.
        self.bitrate = bitrate;
        tracing::debug!(
            bitrate,
            "encoder bitrate target updated (applied on next reset)"
        );
        Ok(())
    }
}

impl Drop for AndroidEncoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}
