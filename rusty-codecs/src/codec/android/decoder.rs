//! Android MediaCodec H.264 decoder.
//!
//! Uses the NDK `AMediaCodec` API in synchronous ByteBuffer mode. Encoded
//! H.264 packets are fed in, and NV12 frames are produced as output. The
//! decoder handles `INFO_OUTPUT_FORMAT_CHANGED` for resolution changes.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};

use super::format::{
    BUFFER_FLAG_CODEC_CONFIG, DEQUEUE_TIMEOUT, KEY_HEIGHT, KEY_WIDTH, decoder_format,
    extract_sps_pps_from_avcc,
};
use crate::{
    codec::h264::annexb::length_prefixed_to_annex_b,
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, FrameData, MediaPacket, NalFormat, PixelFormat, VideoFrame},
    processing::{
        convert::{nv12_to_bgra_data, nv12_to_rgba_data},
        scale::{Scaler, fit_within},
    },
    traits::VideoDecoder,
};

/// Android MediaCodec H.264 decoder using synchronous ByteBuffer mode.
///
/// Accepts encoded H.264 packets and produces decoded [`VideoFrame`] output.
/// The decoder outputs NV12 data from MediaCodec and converts it to RGBA or
/// BGRA based on the configured [`PixelFormat`].
#[derive(derive_more::Debug)]
pub struct AndroidDecoder {
    #[debug(skip)]
    codec: MediaCodec,
    /// NAL framing format of incoming packets.
    nal_format: NalFormat,
    pixel_format: PixelFormat,
    scaler: Scaler,
    viewport_changed: Option<(u32, u32)>,
    last_timestamp: Option<Duration>,
    /// Current decoded frame dimensions (updated on format change).
    decoded_width: u32,
    decoded_height: u32,
    /// Output buffer stride (may be > width due to alignment padding).
    stride: u32,
    /// Output buffer slice height (may be > height).
    slice_height: u32,
    /// Decoded frame waiting to be collected via `pop_frame`.
    #[debug(skip)]
    pending_frame: Option<VideoFrame>,
}

// Safety: MediaCodec instances are thread-safe in the NDK.
unsafe impl Send for AndroidDecoder {}

impl VideoDecoder for AndroidDecoder {
    fn name(&self) -> &str {
        "h264-android"
    }

    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        let inline = match &config.codec {
            VideoCodec::H264(h264) => h264.inline,
            other => bail!("unsupported codec {other} (only H.264 is supported by AndroidDecoder)"),
        };

        // Determine NAL format: inline (avc3) -> Annex B, non-inline (avc1) -> length-prefixed.
        let nal_format = if inline || config.description.is_none() {
            NalFormat::AnnexB
        } else {
            NalFormat::Avcc
        };

        let width = config.coded_width.unwrap_or(1920);
        let height = config.coded_height.unwrap_or(1080);

        // Extract SPS/PPS from avcC description if available.
        let (sps, pps) = config
            .description
            .as_ref()
            .and_then(|desc| extract_sps_pps_from_avcc(desc))
            .map(|(s, p)| (Some(s), Some(p)))
            .unwrap_or((None, None));

        let format = decoder_format(width, height, sps.as_deref(), pps.as_deref())?;

        let codec = MediaCodec::from_decoder_type(super::format::MIME_AVC)
            .context("failed to create Android H.264 decoder")?;

        codec
            .configure(&format, None, MediaCodecDirection::Decoder)
            .map_err(|e| anyhow::anyhow!("MediaCodec decoder configure failed: {e:?}"))?;

        codec
            .start()
            .map_err(|e| anyhow::anyhow!("MediaCodec decoder start failed: {e:?}"))?;

        tracing::info!(width, height, "H.264 Android MediaCodec decoder ready");

        Ok(Self {
            codec,
            nal_format,
            pixel_format: playback_config.pixel_format,
            scaler: Scaler::new(None),
            viewport_changed: None,
            last_timestamp: None,
            decoded_width: width,
            decoded_height: height,
            stride: width,
            slice_height: height,
            pending_frame: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        // Convert to Annex B if needed — MediaCodec expects start-code framing.
        let annex_b_buf;
        let annex_b: &[u8] = match self.nal_format {
            NalFormat::AnnexB => &payload,
            NalFormat::Avcc => {
                annex_b_buf = length_prefixed_to_annex_b(&payload);
                &annex_b_buf
            }
        };

        // Submit the encoded data to the decoder.
        self.submit_input(annex_b, packet.timestamp)?;

        // Drain any available output.
        self.drain_output()?;

        self.last_timestamp = Some(packet.timestamp);
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        // Try one more drain before returning.
        let _ = self.drain_output();

        let Some(mut frame) = self.pending_frame.take() else {
            return Ok(None);
        };

        // Apply viewport scaling after decode.
        let [src_w, src_h] = frame.dimensions;
        if let Some((max_w, max_h)) = self.viewport_changed.take() {
            let (tw, th) = fit_within(src_w, src_h, max_w, max_h);
            self.scaler.set_target_dimensions(tw, th);
        }

        if let FrameData::Packed { data, .. } = &frame.data {
            if let Some((scaled, sw, sh)) = self.scaler.scale_rgba(data, src_w, src_h)? {
                frame = VideoFrame::new_cpu_with_format(
                    scaled,
                    sw,
                    sh,
                    frame.timestamp,
                    self.pixel_format,
                );
            }
        }

        Ok(Some(frame))
    }

    fn burst_size(&self) -> usize {
        // Hardware decoders may buffer several frames in the DPB.
        4
    }
}

impl AndroidDecoder {
    /// Submits encoded data to the next available input buffer.
    fn submit_input(&self, data: &[u8], timestamp: Duration) -> Result<()> {
        match self
            .codec
            .dequeue_input_buffer(DEQUEUE_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("decoder dequeue input failed: {e:?}"))?
        {
            DequeuedInputBufferResult::Buffer(mut input_buf) => {
                let buf = input_buf.buffer_mut();
                if data.len() > buf.len() {
                    anyhow::bail!(
                        "encoded packet ({} bytes) exceeds MediaCodec input buffer ({} bytes)",
                        data.len(),
                        buf.len()
                    );
                }
                let copy_len = data.len();
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr(),
                        buf.as_mut_ptr() as *mut u8,
                        copy_len,
                    );
                }
                self.codec
                    .queue_input_buffer(input_buf, 0, copy_len, timestamp.as_micros() as u64, 0)
                    .map_err(|e| anyhow::anyhow!("decoder queue input failed: {e:?}"))?;
            }
            DequeuedInputBufferResult::TryAgainLater => {
                tracing::trace!("decoder input buffer not available, dropping packet");
            }
        }
        Ok(())
    }

    /// Drains all available output buffers, converting NV12 to the target
    /// pixel format and storing the latest frame in `pending_frame`.
    fn drain_output(&mut self) -> Result<()> {
        loop {
            match self
                .codec
                .dequeue_output_buffer(DEQUEUE_TIMEOUT)
                .map_err(|e| anyhow::anyhow!("decoder dequeue output failed: {e:?}"))?
            {
                DequeuedOutputBufferInfoResult::Buffer(output_buf) => {
                    let info = *output_buf.info();
                    let flags = info.flags();

                    // Skip codec-config buffers (SPS/PPS echo).
                    if (flags & BUFFER_FLAG_CODEC_CONFIG) != 0 {
                        self.codec
                            .release_output_buffer(output_buf, false)
                            .map_err(|e| anyhow::anyhow!("decoder release output failed: {e:?}"))?;
                        continue;
                    }

                    let raw = output_buf.buffer().to_vec();
                    let presentation_time_us = info.presentation_time_us().max(0) as u64;

                    self.codec
                        .release_output_buffer(output_buf, false)
                        .map_err(|e| anyhow::anyhow!("decoder release output failed: {e:?}"))?;

                    // Convert NV12 output to the requested pixel format.
                    // MediaCodec may pad rows to alignment boundaries, so
                    // stride and slice_height can be larger than width/height.
                    let w = self.decoded_width;
                    let h = self.decoded_height;
                    let stride = self.stride;
                    let slice_h = self.slice_height;
                    let y_plane_size = (stride * slice_h) as usize;
                    let uv_plane_size = (stride * slice_h / 2) as usize;

                    if raw.len() < y_plane_size + uv_plane_size {
                        tracing::warn!(
                            len = raw.len(),
                            expected = y_plane_size + uv_plane_size,
                            "decoder output buffer too small for NV12"
                        );
                        continue;
                    }

                    let y_data = &raw[..y_plane_size];
                    let uv_data = &raw[y_plane_size..y_plane_size + uv_plane_size];

                    let pixels = match self.pixel_format {
                        PixelFormat::Rgba => {
                            nv12_to_rgba_data(y_data, stride, uv_data, stride, w, h)?
                        }
                        PixelFormat::Bgra => {
                            nv12_to_bgra_data(y_data, stride, uv_data, stride, w, h)?
                        }
                    };

                    let timestamp = Duration::from_micros(presentation_time_us);
                    self.pending_frame = Some(VideoFrame::new_cpu_with_format(
                        pixels,
                        w,
                        h,
                        timestamp,
                        self.pixel_format,
                    ));
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    let format = self.codec.output_format();
                    if let Some(w) = format.i32(KEY_WIDTH) {
                        self.decoded_width = w as u32;
                    }
                    if let Some(h) = format.i32(KEY_HEIGHT) {
                        self.decoded_height = h as u32;
                    }
                    // Stride/slice-height may differ from width/height due to
                    // alignment padding. These are needed to correctly extract
                    // NV12 planes from the output buffer.
                    if let Some(s) = format.i32("stride") {
                        self.stride = s as u32;
                    } else {
                        self.stride = self.decoded_width;
                    }
                    if let Some(sh) = format.i32("slice-height") {
                        self.slice_height = sh as u32;
                    } else {
                        self.slice_height = self.decoded_height;
                    }
                    tracing::debug!(
                        width = self.decoded_width,
                        height = self.decoded_height,
                        stride = self.stride,
                        slice_height = self.slice_height,
                        "decoder output format changed"
                    );
                }
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => {
                    continue;
                }
                DequeuedOutputBufferInfoResult::TryAgainLater => {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl Drop for AndroidDecoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}
