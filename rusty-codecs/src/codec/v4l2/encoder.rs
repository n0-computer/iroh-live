use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::Duration,
};

use anyhow::{Context as _, Result};

use crate::{
    codec::h264::annexb::{annex_b_to_length_prefixed, build_avcc, extract_sps_pps, parse_annex_b},
    config::{H264, VideoCodec, VideoConfig},
    format::{EncodedFrame, NalFormat, ScaleMode, VideoEncoderConfig, VideoFrame},
    processing::convert::pixel_format_to_nv12,
    processing::scale::Scaler,
    traits::{VideoEncoder, VideoEncoderFactory},
};

/// V4L2 hardware-accelerated H.264 encoder for Linux.
///
/// Uses the `v4l2r` crate's stateful encoder API, targeting devices like the
/// Raspberry Pi's `bcm2835-codec` (`/dev/video11`). Runs the encoder in a
/// dedicated thread to contain v4l2r's unnameable type-state generics.
#[derive(derive_more::Debug)]
pub struct V4l2Encoder {
    #[debug(skip)]
    input_tx: SyncSender<EncoderCmd>,
    #[debug(skip)]
    output_rx: Receiver<EncodedOutput>,
    #[debug(skip)]
    thread: Option<JoinHandle<()>>,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    nal_format: NalFormat,
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    avcc: Option<Vec<u8>>,
    packet_buf: VecDeque<EncodedFrame>,
}

enum EncoderCmd {
    Encode { nv12: Vec<u8>, timestamp_us: u64 },
}

struct EncodedOutput {
    data: Vec<u8>,
    timestamp_us: u64,
}

impl V4l2Encoder {
    /// Bits-per-pixel factor for H.264 default bitrate calculation.
    const H264_BPP: f32 = 0.07;

    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(Self::H264_BPP);
        let keyframe_interval = config.keyframe_interval_or_default();
        let nal_format = config.nal_format;

        let device_path = super::encoder_device_path()
            .context("no V4L2 encoder device found (set V4L2_ENC_DEVICE env var)")?;

        let (input_tx, input_rx) = mpsc::sync_channel::<EncoderCmd>(4);
        let (output_tx, output_rx) = mpsc::sync_channel::<EncodedOutput>(8);
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<()>>(1);

        let device_path_owned = device_path.clone();
        let thread = std::thread::Builder::new()
            .name("v4l2-encoder".into())
            .spawn(move || {
                encoder_thread(
                    device_path_owned,
                    width,
                    height,
                    bitrate,
                    keyframe_interval,
                    input_rx,
                    output_tx,
                    init_tx,
                );
            })
            .context("failed to spawn V4L2 encoder thread")?;

        // Wait for initialization.
        let init_result = init_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("V4L2 encoder thread exited during init"))?;
        init_result?;

        Ok(Self {
            input_tx,
            output_rx,
            thread: Some(thread),
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            nal_format,
            scale_mode: config.scale_mode,
            scaler: Scaler::new(Some((width, height))),
            avcc: None,
            packet_buf: VecDeque::new(),
        })
    }

    fn drain_output(&mut self) {
        while let Ok(out) = self.output_rx.try_recv() {
            if let Some(frame) = self.process_output(out) {
                self.packet_buf.push_back(frame);
            }
        }
    }

    fn process_output(&mut self, out: EncodedOutput) -> Option<EncodedFrame> {
        // Strip trailing zero-padding from the coded buffer.
        let end = out.data.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
        let annex_b = &out.data[..end];
        if annex_b.is_empty() {
            return None;
        }

        let nals = parse_annex_b(annex_b);
        let is_keyframe = nals
            .iter()
            .any(|nal| !nal.is_empty() && (nal[0] & 0x1F) == 5);

        // Extract avcC from first keyframe if needed.
        if self.nal_format == NalFormat::Avcc
            && is_keyframe
            && self.avcc.is_none()
            && let Some((sps, pps)) = extract_sps_pps(&nals)
        {
            self.avcc = Some(build_avcc(&sps, &pps));
        }

        let payload: bytes::Bytes = match self.nal_format {
            NalFormat::AnnexB => annex_b.to_vec().into(),
            NalFormat::Avcc => annex_b_to_length_prefixed(annex_b).into(),
        };

        Some(EncodedFrame {
            is_keyframe,
            timestamp: Duration::from_micros(out.timestamp_us),
            payload,
        })
    }
}

impl V4l2Encoder {
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
}

impl VideoEncoderFactory for V4l2Encoder {
    const ID: &str = "h264-v4l2";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(Self::H264_BPP);
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

impl VideoEncoder for V4l2Encoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
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

        // Fast-path: use NV12 data directly when available, avoiding RGBA
        // round-trips. For GPU frames with DMA-BUF handles, `download_nv12()`
        // reads the NV12 planes without an intermediate RGBA conversion.
        //
        // TODO: true zero-copy via V4L2_MEMORY_DMABUF on the OUTPUT queue.
        // Requires allocating the queue in DMA-BUF mode at init and passing
        // the DMA-BUF FD from `NativeFrameHandle::DmaBuf` directly to V4L2.
        let nv12 = match &frame.data {
            crate::format::FrameData::Nv12(planes) => nv12_planes_to_contiguous(planes),
            crate::format::FrameData::Gpu(gpu) => {
                if let Some(Ok(planes)) = gpu.download_nv12() {
                    nv12_planes_to_contiguous(&planes)
                } else {
                    let img = frame.rgba_image();
                    pixel_format_to_nv12(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
                        .into_contiguous()
                }
            }
            crate::format::FrameData::Packed { pixel_format, data } => {
                pixel_format_to_nv12(data, w, h, *pixel_format)?.into_contiguous()
            }
            _ => {
                let img = frame.rgba_image();
                pixel_format_to_nv12(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
                    .into_contiguous()
            }
        };

        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
        self.frame_count += 1;

        self.input_tx
            .send(EncoderCmd::Encode { nv12, timestamp_us })
            .map_err(|_| anyhow::anyhow!("V4L2 encoder thread exited"))?;

        self.drain_output();
        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        self.drain_output();
        Ok(self.packet_buf.pop_front())
    }
}

impl Drop for V4l2Encoder {
    fn drop(&mut self) {
        // Close the input channel to signal shutdown.
        drop(std::mem::replace(
            &mut self.input_tx,
            mpsc::sync_channel(0).0,
        ));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs the V4L2 stateful encoder. All v4l2r type-state generics are local.
#[allow(clippy::too_many_arguments)]
fn encoder_thread(
    device_path: std::path::PathBuf,
    width: u32,
    height: u32,
    bitrate: u64,
    keyframe_interval: u32,
    input_rx: Receiver<EncoderCmd>,
    output_tx: SyncSender<EncodedOutput>,
    init_tx: SyncSender<Result<()>>,
) {
    use v4l2r::{
        device::queue::handles_provider::MmapProvider,
        encoder::{CompletedOutputBuffer, Encoder},
        memory::MmapHandle,
    };

    // Shared timestamp queue: input side pushes, output callback pops.
    let timestamps: Arc<Mutex<VecDeque<u64>>> = Arc::new(Mutex::new(VecDeque::new()));

    let result: Result<()> = (|| {
        // Set H.264 encoder controls via a separate FD (best-effort).
        // Must be done before the encoder's internal streaming starts.
        set_encoder_controls(&device_path, bitrate, keyframe_interval);

        let encoder = Encoder::open(&device_path)
            .map_err(|e| anyhow::anyhow!("failed to open V4L2 encoder {device_path:?}: {e}"))?;

        // Set capture format (H.264 encoded output).
        let encoder = encoder
            .set_capture_format(|f| {
                f.set_pixelformat(b"H264").apply::<v4l2r::Format>()?;
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("failed to set V4L2 encoder capture format: {e}"))?;

        // Set output format (NV12 raw frame input).
        let encoder = encoder
            .set_output_format(|f| {
                f.set_pixelformat(b"NV12")
                    .set_size(width as usize, height as usize)
                    .apply::<v4l2r::Format>()?;
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("failed to set V4L2 encoder output format: {e}"))?;

        // Get capture format for MMAP buffer allocation (available before output buffer alloc).
        let capture_format = encoder
            .get_capture_format()
            .map_err(|e| anyhow::anyhow!("failed to get capture format: {e}"))?;

        // Allocate output buffers (raw frame input, MMAP).
        let encoder = encoder
            .allocate_output_buffers::<Vec<MmapHandle>>(4)
            .map_err(|e| anyhow::anyhow!("failed to allocate V4L2 encoder output buffers: {e}"))?;

        // Allocate capture buffers (encoded output, MMAP).
        let encoder = encoder
            .allocate_capture_buffers(4, MmapProvider::new(&capture_format))
            .map_err(|e| anyhow::anyhow!("failed to allocate V4L2 encoder capture buffers: {e}"))?;

        let output_tx_cb = output_tx.clone();
        let ts_cb = timestamps.clone();

        let mut encoder = encoder
            .start(
                // Input done: no-op.
                |_: CompletedOutputBuffer<Vec<MmapHandle>>| {},
                // Encoded output ready: extract data and send to main thread.
                move |cap_dqbuf| {
                    let bytes_used = *cap_dqbuf.data.get_first_plane().bytesused as usize;
                    if bytes_used == 0 {
                        return;
                    }
                    let Some(mapping) = cap_dqbuf.get_plane_mapping(0) else {
                        return;
                    };
                    let data = mapping.as_ref()[..bytes_used].to_vec();
                    let timestamp_us = ts_cb.lock().unwrap().pop_front().unwrap_or(0);
                    let _ = output_tx_cb.try_send(EncodedOutput { data, timestamp_us });
                },
            )
            .map_err(|e| anyhow::anyhow!("failed to start V4L2 encoder: {e}"))?;

        // Signal successful init.
        init_tx
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("init channel closed"))?;

        // Process input commands until the channel closes.
        for cmd in &input_rx {
            match cmd {
                EncoderCmd::Encode { nv12, timestamp_us } => {
                    timestamps.lock().unwrap().push_back(timestamp_us);

                    let qbuf = match encoder.get_buffer() {
                        Ok(buf) => buf,
                        Err(e) => {
                            tracing::warn!("V4L2 encoder: no free buffer: {e}");
                            continue;
                        }
                    };

                    // Write NV12 data into the MMAP buffer.
                    {
                        let Some(mut mapping) = qbuf.get_plane_mapping(0) else {
                            tracing::warn!("V4L2 encoder: buffer map failed");
                            continue;
                        };
                        let len = nv12.len().min(mapping.len());
                        mapping[..len].copy_from_slice(&nv12[..len]);
                    }

                    if let Err(e) = qbuf.queue(&[nv12.len()]) {
                        tracing::warn!("V4L2 encoder: queue failed: {e}");
                    }
                }
            }
        }

        // Stop encoder (implicitly drains remaining frames).
        let _ = encoder.stop();
        Ok(())
    })();

    if let Err(e) = result {
        // If init hasn't been acknowledged, send the error.
        let _ = init_tx.send(Err(e));
    }
}

/// Tries to set H.264 encoder controls on the device (best-effort).
///
/// Opens the device separately for control setup. Not all controls are
/// supported by all hardware — errors are logged at debug level.
fn set_encoder_controls(device_path: &std::path::Path, bitrate: u64, keyframe_interval: u32) {
    use std::os::unix::io::AsRawFd;
    use v4l2r::controls::{ExtControlTrait as _, codec};

    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(device_path)
    else {
        return;
    };

    let fd = file.as_raw_fd();

    // V4L2_CID_MPEG_VIDEO_BITRATE
    if let Err(e) = v4l2r::ioctl::s_ctrl(&fd, codec::VideoBitrate::ID, bitrate as i32) {
        tracing::debug!("V4L2: could not set bitrate: {e}");
    }

    // V4L2_CID_MPEG_VIDEO_GOP_SIZE
    if let Err(e) = v4l2r::ioctl::s_ctrl(&fd, codec::VideoGopSize::ID, keyframe_interval as i32) {
        tracing::debug!("V4L2: could not set GOP size: {e}");
    }

    // V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR — inline SPS/PPS before each keyframe.
    if let Err(e) = v4l2r::ioctl::s_ctrl(&fd, codec::VideoPrependSpsPpsToIdr::ID, 1) {
        tracing::debug!("V4L2: could not set prepend SPS/PPS: {e}");
    }

    // V4L2_CID_MPEG_VIDEO_H264_PROFILE — Constrained Baseline for low-latency.
    if let Err(e) = v4l2r::ioctl::s_ctrl(
        &fd,
        codec::VideoH264Profile::ID,
        codec::VideoH264Profile::ConstrainedBaseline as i32,
    ) {
        tracing::debug!("V4L2: could not set H264 profile: {e}");
    }

    // V4L2_CID_MPEG_VIDEO_H264_LEVEL — Level 3.0.
    if let Err(e) = v4l2r::ioctl::s_ctrl(
        &fd,
        codec::VideoH264Level::ID,
        codec::VideoH264Level::L3_0 as i32,
    ) {
        tracing::debug!("V4L2: could not set H264 level: {e}");
    }
}

/// Packs [`Nv12Planes`] into a contiguous Y+UV buffer, stripping stride padding.
fn nv12_planes_to_contiguous(planes: &crate::format::Nv12Planes) -> Vec<u8> {
    let w = planes.width as usize;
    let h = planes.height as usize;
    let uv_h = (h + 1) / 2;
    let y_stride = planes.y_stride as usize;
    let uv_stride = planes.uv_stride as usize;

    let mut out = Vec::with_capacity(w * h + w * uv_h);

    // Y plane.
    if y_stride == w && planes.y_data.len() >= w * h {
        out.extend_from_slice(&planes.y_data[..w * h]);
    } else {
        for row in 0..h {
            let start = row * y_stride;
            let end = (start + w).min(planes.y_data.len());
            if start < planes.y_data.len() {
                out.extend_from_slice(&planes.y_data[start..end]);
            }
        }
    }

    // UV plane.
    if uv_stride == w && planes.uv_data.len() >= w * uv_h {
        out.extend_from_slice(&planes.uv_data[..w * uv_h]);
    } else {
        for row in 0..uv_h {
            let start = row * uv_stride;
            let end = (start + w).min(planes.uv_data.len());
            if start < planes.uv_data.len() {
                out.extend_from_slice(&planes.uv_data[start..end]);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::test_util::make_rgba_frame,
        format::VideoPreset,
        traits::{VideoEncoder, VideoEncoderFactory},
    };

    #[test]
    #[ignore = "requires V4L2 encoder hardware"]
    fn v4l2_encode_basic() {
        let mut enc = V4l2Encoder::with_preset(VideoPreset::P360).unwrap();
        let mut packet_count = 0;
        for _ in 0..30 {
            let frame = make_rgba_frame(640, 360, 255, 0, 0);
            enc.push_frame(frame).unwrap();
            while let Some(_pkt) = enc.pop_packet().unwrap() {
                packet_count += 1;
            }
        }
        assert!(
            packet_count > 0,
            "expected at least 1 packet, got {packet_count}"
        );
    }

    #[test]
    #[ignore = "requires V4L2 encoder hardware"]
    fn v4l2_encode_keyframe_interval() {
        let mut enc = V4l2Encoder::with_preset(VideoPreset::P360).unwrap();
        let mut keyframe_count = 0;
        for _ in 0..60 {
            let frame = make_rgba_frame(640, 360, 128, 128, 128);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                if pkt.is_keyframe {
                    keyframe_count += 1;
                }
            }
        }
        assert!(
            keyframe_count >= 2,
            "expected >= 2 keyframes in 60 frames, got {keyframe_count}"
        );
    }

    #[test]
    #[ignore = "requires V4L2 encoder hardware"]
    fn v4l2_config_fields() {
        let enc = V4l2Encoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(matches!(config.codec, VideoCodec::H264(_)));
        assert_eq!(config.coded_width, Some(640));
        assert_eq!(config.coded_height, Some(360));
        assert_eq!(config.framerate, Some(30.0));
    }
}
