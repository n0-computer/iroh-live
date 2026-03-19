//! Android MediaCodec H.264 hardware decoder with zero-copy output.
//!
//! Uses an `ImageReader` surface as the MediaCodec output target. Decoded
//! frames remain in GPU memory as HardwareBuffers — no NV12-to-RGBA
//! conversion or CPU pixel copy on the hot path. Each frame is wrapped in
//! an [`AndroidGpuFrame`] and flows through the standard `VideoFrame`
//! pipeline.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use ndk::{
    hardware_buffer::HardwareBufferUsage,
    media::{
        image_reader::{AcquireResult, ImageFormat, ImageReader},
        media_codec::{
            DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec,
            MediaCodecDirection,
        },
    },
};

use super::{
    format::{
        BUFFER_FLAG_CODEC_CONFIG, DEQUEUE_INPUT_TIMEOUT, DEQUEUE_OUTPUT_TIMEOUT, KEY_HEIGHT,
        KEY_WIDTH, decoder_format, extract_sps_pps_from_avcc,
    },
    gpu_frame::AndroidGpuFrame,
};
use crate::{
    codec::h264::annexb::length_prefixed_to_annex_b,
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, GpuFrame, MediaPacket, NalFormat, VideoFrame},
    traits::VideoDecoder,
};

/// Android MediaCodec H.264 decoder using ImageReader surface output.
///
/// Decoded frames stay in GPU memory as HardwareBuffers. The decoder
/// produces [`VideoFrame`] instances with `FrameData::Gpu` backing,
/// enabling zero-copy rendering on the consumer side.
#[derive(derive_more::Debug)]
pub struct AndroidHwDecoder {
    #[debug(skip)]
    codec: MediaCodec,
    #[debug(skip)]
    image_reader: ImageReader,
    /// NAL framing format of incoming packets.
    nal_format: NalFormat,
    /// Current decoded frame dimensions (updated on format change).
    decoded_width: u32,
    decoded_height: u32,
    /// Output buffer stride (may be > width due to alignment padding).
    stride: u32,
    /// Output buffer slice height (may be > height).
    slice_height: u32,
    /// Decoded frames waiting to be collected via `pop_frame`.
    #[debug(skip)]
    pending_frames: VecDeque<VideoFrame>,
}

// SAFETY: MediaCodec and ImageReader can be moved between threads. The NDK
// requires callers to serialize access (no concurrent calls), which is
// upheld because AndroidHwDecoder takes `&mut self` for all operations.
unsafe impl Send for AndroidHwDecoder {}

impl VideoDecoder for AndroidHwDecoder {
    fn name(&self) -> &str {
        "h264-android-hw"
    }

    fn new(config: &VideoConfig, _playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        let inline = match &config.codec {
            VideoCodec::H264(h264) => h264.inline,
            other => {
                bail!("unsupported codec {other} (only H.264 is supported by AndroidHwDecoder)")
            }
        };

        let nal_format = if inline || config.description.is_none() {
            NalFormat::AnnexB
        } else {
            NalFormat::Avcc
        };

        let width = config.coded_width.unwrap_or(1920);
        let height = config.coded_height.unwrap_or(1080);

        let (sps, pps) = config
            .description
            .as_ref()
            .and_then(|desc| extract_sps_pps_from_avcc(desc))
            .map(|(s, p)| (Some(s), Some(p)))
            .unwrap_or((None, None));

        let format = decoder_format(width, height, sps.as_deref(), pps.as_deref())?;

        // Create an ImageReader for decoded frame output. The pool must be
        // large enough to cover: decode DPB (typically 4 for H.264) + playout
        // buffer (burst_size frames ≈ 4) + a few in-flight to the consumer.
        // 8 images avoids MaxImagesAcquired stalls that starve the pipeline.
        // GPU_SAMPLED_IMAGE allows the consumer to import the HardwareBuffer
        // as a GL/Vulkan texture.
        let image_reader = ImageReader::new_with_usage(
            width as i32,
            height as i32,
            ImageFormat::YUV_420_888,
            HardwareBufferUsage::GPU_SAMPLED_IMAGE | HardwareBufferUsage::CPU_READ_OFTEN,
            8,
        )
        .map_err(|e| anyhow::anyhow!("ImageReader creation failed: {e:?}"))?;

        let surface = image_reader
            .window()
            .map_err(|e| anyhow::anyhow!("ImageReader::window() failed: {e:?}"))?;

        let codec = MediaCodec::from_decoder_type(super::format::MIME_AVC)
            .context("failed to create Android H.264 decoder")?;

        codec
            .configure(&format, Some(&surface), MediaCodecDirection::Decoder)
            .map_err(|e| anyhow::anyhow!("MediaCodec configure with surface failed: {e:?}"))?;

        codec
            .start()
            .map_err(|e| anyhow::anyhow!("MediaCodec start failed: {e:?}"))?;

        tracing::info!(
            width,
            height,
            "H.264 Android HW decoder ready (ImageReader surface output)"
        );

        Ok(Self {
            codec,
            image_reader,
            nal_format,
            decoded_width: width,
            decoded_height: height,
            stride: width,
            slice_height: height,
            pending_frames: VecDeque::new(),
        })
    }

    fn set_viewport(&mut self, _w: u32, _h: u32) {
        // No viewport scaling in HW decoder — frames are full resolution.
        // The consumer handles display scaling.
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        let annex_b_buf;
        let annex_b: &[u8] = match self.nal_format {
            NalFormat::AnnexB => &payload,
            NalFormat::Avcc => {
                annex_b_buf = length_prefixed_to_annex_b(&payload);
                &annex_b_buf
            }
        };

        self.submit_input(annex_b, packet.timestamp)?;
        self.drain_output()?;

        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let _ = self.drain_output();
        Ok(self.pending_frames.pop_front())
    }

    fn burst_size(&self) -> usize {
        4
    }
}

impl AndroidHwDecoder {
    /// Submits encoded data to the next available input buffer.
    fn submit_input(&self, data: &[u8], timestamp: Duration) -> Result<()> {
        match self
            .codec
            .dequeue_input_buffer(DEQUEUE_INPUT_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("decoder dequeue input failed: {e:?}"))?
        {
            DequeuedInputBufferResult::Buffer(mut input_buf) => {
                let buf = input_buf.buffer_mut();
                if data.len() > buf.len() {
                    bail!(
                        "encoded packet ({} bytes) exceeds MediaCodec input buffer ({} bytes)",
                        data.len(),
                        buf.len()
                    );
                }
                let copy_len = data.len();
                // SAFETY: Both pointers are valid for copy_len bytes — data
                // is the encoded packet, buf is the MediaCodec input buffer.
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

    /// Drains all available output buffers. Each buffer is released with
    /// `render=true` to route it through the ImageReader surface, then the
    /// latest image is acquired as a HardwareBuffer-backed GPU frame.
    fn drain_output(&mut self) -> Result<()> {
        loop {
            match self
                .codec
                .dequeue_output_buffer(DEQUEUE_OUTPUT_TIMEOUT)
                .map_err(|e| anyhow::anyhow!("decoder dequeue output failed: {e:?}"))?
            {
                DequeuedOutputBufferInfoResult::Buffer(output_buf) => {
                    let info = *output_buf.info();
                    let flags = info.flags();

                    // Skip codec-config buffers (SPS/PPS echo).
                    if (flags & BUFFER_FLAG_CODEC_CONFIG) != 0 {
                        self.codec
                            .release_output_buffer(output_buf, false)
                            .map_err(|e| {
                                anyhow::anyhow!("decoder release config buffer failed: {e:?}")
                            })?;
                        continue;
                    }

                    let presentation_time_us = info.presentation_time_us().max(0) as u64;

                    // Release with render=true to route through the ImageReader surface.
                    self.codec
                        .release_output_buffer(output_buf, true)
                        .map_err(|e| {
                            anyhow::anyhow!("decoder release output buffer (render) failed: {e:?}")
                        })?;

                    // Acquire the rendered image from the ImageReader.
                    match self
                        .image_reader
                        .acquire_latest_image()
                        .map_err(|e| anyhow::anyhow!("acquire_latest_image failed: {e:?}"))?
                    {
                        AcquireResult::Image(image) => {
                            let w = self.decoded_width;
                            let h = self.decoded_height;
                            let y_stride = self.stride;
                            let uv_offset = self.stride * self.slice_height;
                            let uv_stride = self.stride;

                            let gpu_frame =
                                AndroidGpuFrame::new(image, w, h, y_stride, uv_offset, uv_stride);
                            let frame = VideoFrame::new_gpu(
                                GpuFrame::new(Arc::new(gpu_frame)),
                                Duration::from_micros(presentation_time_us),
                            );
                            self.pending_frames.push_back(frame);
                        }
                        AcquireResult::NoBufferAvailable => {
                            tracing::trace!("ImageReader: no buffer available after release");
                        }
                        AcquireResult::MaxImagesAcquired => {
                            tracing::warn!("ImageReader: max images acquired, frame dropped");
                        }
                    }
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    let format = self.codec.output_format();
                    if let Some(w) = format.i32(KEY_WIDTH) {
                        self.decoded_width = w as u32;
                    }
                    if let Some(h) = format.i32(KEY_HEIGHT) {
                        self.decoded_height = h as u32;
                    }
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
                        "hw decoder output format changed"
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

impl Drop for AndroidHwDecoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}
