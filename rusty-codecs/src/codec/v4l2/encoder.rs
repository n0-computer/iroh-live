use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver, SyncSender},
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
/// Uses raw V4L2 ioctls to drive the V4L2 M2M encoder, following the standard
/// V4L2 M2M startup sequence required by the bcm2835-codec driver. Validated
/// against ffmpeg's `h264_v4l2m2m` and the kernel documentation. Runs in a
/// internal poll loop for the CAPTURE queue.
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
                    framerate,
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
        let mut count = 0u32;
        while let Ok(out) = self.output_rx.try_recv() {
            count += 1;
            if let Some(frame) = self.process_output(out) {
                self.packet_buf.push_back(frame);
            }
        }
        if count > 0 {
            tracing::trace!(
                drained = count,
                buffered = self.packet_buf.len(),
                "V4L2 encoder: drain_output"
            );
        }
    }

    fn process_output(&mut self, out: EncodedOutput) -> Option<EncodedFrame> {
        // Strip trailing zero-padding from the coded buffer.
        let end = out.data.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
        let annex_b = &out.data[..end];
        if annex_b.is_empty() {
            tracing::debug!("V4L2 encoder: empty output buffer (all zeros)");
            return None;
        }
        tracing::trace!(
            raw_len = out.data.len(),
            trimmed_len = annex_b.len(),
            "V4L2 encoder: got encoded output"
        );

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

        let nv12 = match &frame.data {
            crate::format::FrameData::Nv12(planes) => nv12_planes_to_contiguous(planes),
            crate::format::FrameData::I420 { y, u, v } => {
                let y_size = (w * h) as usize;
                let uv_size = y_size / 2;
                let mut buf = Vec::with_capacity(y_size + uv_size);
                buf.extend_from_slice(y);
                let uv_len = (w as usize / 2) * (h as usize / 2);
                for i in 0..uv_len {
                    buf.push(u[i]);
                    buf.push(v[i]);
                }
                buf
            }
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
        };

        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
        self.frame_count += 1;

        if self.frame_count <= 3 || self.frame_count % 150 == 0 {
            tracing::debug!(
                frame = self.frame_count,
                nv12_len = nv12.len(),
                timestamp_us,
                "V4L2 encoder: sending frame"
            );
        }

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

// ---------------------------------------------------------------------------
// Raw V4L2 M2M encoder implementation using direct ioctls.
//
// Standard V4L2 M2M encoder sequence (per kernel docs + bcm2835-codec):
//   1. Open device
//   2. S_FMT OUTPUT (NV12) + CAPTURE (H264)
//   3. Set controls (bitrate, GOP, profile, level)
//   4. REQBUFS OUTPUT (4) + CAPTURE (8)
//   5. MMAP all buffers
//   6. QBUF one OUTPUT frame
//   7. STREAMON OUTPUT
//   8. QBUF all CAPTURE buffers
//   9. STREAMON CAPTURE
//  10. Poll loop: DQBUF CAPTURE → send encoded, DQBUF OUTPUT → reuse
//
// Also validated against ffmpeg's h264_v4l2m2m for driver compatibility.
// ---------------------------------------------------------------------------

#[allow(unreachable_pub, dead_code)]
mod raw_v4l2 {
    use std::collections::VecDeque;
    use std::os::unix::io::{AsRawFd, RawFd};

    /// V4L2 ioctl numbers (from linux/videodev2.h).
    #[allow(unreachable_pub)]
    mod ioctl_nr {
        use libc::c_ulong;
        // _IOWR('V', ...)
        pub const VIDIOC_S_FMT: c_ulong = 0xC0D05605;
        pub const VIDIOC_REQBUFS: c_ulong = 0xC0145608;
        pub const VIDIOC_QUERYBUF: c_ulong = 0xC0585609;
        pub const VIDIOC_QBUF: c_ulong = 0xC058560F;
        pub const VIDIOC_DQBUF: c_ulong = 0xC0585611;
        pub const VIDIOC_STREAMON: c_ulong = 0x40045612;
        pub const VIDIOC_STREAMOFF: c_ulong = 0x40045613;
        pub const VIDIOC_S_PARM: c_ulong = 0xC0CC5616;
        pub const VIDIOC_S_CTRL: c_ulong = 0xC008561C;
    }

    // V4L2 type constants (from linux/videodev2.h).
    const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
    const V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;
    const V4L2_MEMORY_MMAP: u32 = 1;

    // V4L2 control IDs (from linux/v4l2-controls.h).
    // V4L2_CID_CODEC_BASE = V4L2_CTRL_CLASS_CODEC | 0x900 = 0x00990900
    const V4L2_CID_MPEG_VIDEO_GOP_SIZE: u32 = 0x00990900 + 203; // CODEC_BASE+203
    const V4L2_CID_MPEG_VIDEO_BITRATE: u32 = 0x00990900 + 207; // CODEC_BASE+207
    const V4L2_CID_MPEG_VIDEO_H264_LEVEL: u32 = 0x00990900 + 359; // CODEC_BASE+359
    const V4L2_CID_MPEG_VIDEO_H264_PROFILE: u32 = 0x00990900 + 363; // CODEC_BASE+363
    const V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR: u32 = 0x00990900 + 644; // CODEC_BASE+644

    // H264 Baseline profile = 0, Level 3.0 = 9 (V4L2 enum values).
    const V4L2_MPEG_VIDEO_H264_PROFILE_CONSTRAINED_BASELINE: i32 = 1;
    const V4L2_MPEG_VIDEO_H264_LEVEL_3_0: i32 = 9;

    fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
        u32::from(a) | (u32::from(b) << 8) | (u32::from(c) << 16) | (u32::from(d) << 24)
    }

    /// v4l2_format (208 bytes on 64-bit).
    ///
    /// The union in the kernel struct contains pointer types (v4l2_window)
    /// requiring 8-byte alignment, so there are 4 bytes of padding between
    /// `type_` (u32) and the union. We use a flat [u8; 204] to represent the
    /// padding + union, and write fields at known offsets.
    #[repr(C)]
    struct V4l2Format {
        type_: u32,       // 0..4
        _rest: [u8; 204], // 4..208 (4 bytes padding + 200 bytes union)
    }

    /// v4l2_requestbuffers.
    #[repr(C)]
    struct V4l2Requestbuffers {
        count: u32,
        type_: u32,
        memory: u32,
        capabilities: u32,
        flags: u8,
        reserved: [u8; 3],
    }

    /// v4l2_plane (for MPLANE, 64-bit layout).
    ///
    /// The `m` union is 8 bytes on 64-bit (contains `unsigned long userptr`).
    /// Total size: 64 bytes.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct V4l2Plane {
        bytesused: u32,      // 0
        length: u32,         // 4
        m_offset: u64,       // 8  (union: mem_offset(u32) / userptr(u64) / fd(i32))
        data_offset: u32,    // 16
        reserved: [u32; 11], // 20..64
    }

    /// v4l2_buffer (MPLANE version, 64-bit layout).
    ///
    /// Matches the kernel's struct exactly: field at offset 16 is followed by
    /// 4 bytes of padding before timestamp (offset 24) due to timeval's
    /// 8-byte alignment requirement.
    #[repr(C)]
    struct V4l2Buffer {
        index: u32,          // 0
        type_: u32,          // 4
        bytesused: u32,      // 8
        flags: u32,          // 12
        field: u32,          // 16
        _pad: u32,           // 20 (padding for timeval alignment)
        timestamp_sec: i64,  // 24
        timestamp_usec: i64, // 32
        timecode: [u32; 4],  // 40
        sequence: u32,       // 56
        memory: u32,         // 60
        m_planes: u64,       // 64 (pointer to v4l2_plane array)
        length: u32,         // 72 (number of planes)
        reserved2: u32,      // 76
        _union: u32,         // 80
        reserved: u32,       // 84
    } // total: 88 = 0x58

    /// v4l2_control.
    #[repr(C)]
    struct V4l2Control {
        id: u32,
        value: i32,
    }

    /// v4l2_streamparm (subset).
    #[repr(C)]
    struct V4l2Streamparm {
        type_: u32,
        parm: [u8; 200],
    }

    struct MmapBuffer {
        ptr: *mut u8,
        length: usize,
    }

    // SAFETY: we only access MmapBuffer from the encoder thread.
    unsafe impl Send for MmapBuffer {}

    pub(super) struct RawEncoder {
        fd: std::fs::File,
        output_bufs: Vec<MmapBuffer>,  // raw NV12 input
        capture_bufs: Vec<MmapBuffer>, // encoded H264 output
        output_free: VecDeque<u32>,    // free OUTPUT buffer indices
    }

    impl Drop for RawEncoder {
        fn drop(&mut self) {
            for buf in self.output_bufs.drain(..) {
                unsafe {
                    libc::munmap(buf.ptr as *mut libc::c_void, buf.length);
                }
            }
            for buf in self.capture_bufs.drain(..) {
                unsafe {
                    libc::munmap(buf.ptr as *mut libc::c_void, buf.length);
                }
            }
            // STREAMOFF both queues.
            let mut type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
            unsafe {
                libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_STREAMOFF, &mut type_);
            }
            type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
            unsafe {
                libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_STREAMOFF, &mut type_);
            }
        }
    }

    impl RawEncoder {
        pub fn open(
            device_path: &std::path::Path,
            width: u32,
            height: u32,
            bitrate: u64,
            keyframe_interval: u32,
            framerate: u32,
        ) -> anyhow::Result<Self> {
            let fd = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(device_path)?;

            let raw_fd = fd.as_raw_fd();

            // 1. S_FMT OUTPUT (NV12).
            Self::s_fmt(
                raw_fd,
                V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
                fourcc(b'N', b'V', b'1', b'2'),
                width,
                height,
            )?;
            tracing::debug!("V4L2 raw: OUTPUT format set to NV12 {width}x{height}");

            // 2. S_FMT CAPTURE (H264).
            Self::s_fmt(
                raw_fd,
                V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                fourcc(b'H', b'2', b'6', b'4'),
                width,
                height,
            )?;
            tracing::debug!("V4L2 raw: CAPTURE format set to H264 {width}x{height}");

            // 3. Set controls (best-effort).
            Self::s_ctrl(raw_fd, V4L2_CID_MPEG_VIDEO_BITRATE, bitrate as i32);
            Self::s_ctrl(
                raw_fd,
                V4L2_CID_MPEG_VIDEO_GOP_SIZE,
                keyframe_interval as i32,
            );
            Self::s_ctrl(
                raw_fd,
                V4L2_CID_MPEG_VIDEO_H264_PROFILE,
                V4L2_MPEG_VIDEO_H264_PROFILE_CONSTRAINED_BASELINE,
            );
            Self::s_ctrl(
                raw_fd,
                V4L2_CID_MPEG_VIDEO_H264_LEVEL,
                V4L2_MPEG_VIDEO_H264_LEVEL_3_0,
            );
            Self::s_ctrl(raw_fd, V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR, 1);

            // 4. S_PARM (framerate).
            Self::s_parm(raw_fd, framerate);

            // 5. REQBUFS.
            let n_output = Self::reqbufs(raw_fd, V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 4)?;
            let n_capture = Self::reqbufs(raw_fd, V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 8)?;
            tracing::info!(n_output, n_capture, "V4L2 raw: buffers allocated");

            // 6. MMAP all buffers.
            let output_bufs =
                Self::mmap_buffers(raw_fd, V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, n_output)?;
            let capture_bufs =
                Self::mmap_buffers(raw_fd, V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, n_capture)?;

            let output_free: VecDeque<u32> = (0..n_output as u32).collect();

            Ok(Self {
                fd,
                output_bufs,
                capture_bufs,
                output_free,
            })
        }

        /// Queue a raw NV12 frame to the OUTPUT queue.
        /// Returns false if no free buffer is available (caller should drain CAPTURE first).
        pub fn queue_frame(&mut self, nv12: &[u8], timestamp_us: u64) -> bool {
            let Some(idx) = self.output_free.pop_front() else {
                return false;
            };
            let buf = &self.output_bufs[idx as usize];
            let len = nv12.len().min(buf.length);
            unsafe {
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), buf.ptr, len);
            }
            self.qbuf_output(idx, len as u32, timestamp_us);
            true
        }

        /// Dequeue one encoded buffer from CAPTURE. Returns (data, is_last).
        /// Returns None if no buffer is ready (non-blocking).
        pub fn dequeue_capture(&mut self) -> Option<(Vec<u8>, u64)> {
            let mut planes = [V4l2Plane::default(); 1];
            let mut buf = Self::make_buffer(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, V4L2_MEMORY_MMAP);
            buf.m_planes = planes.as_mut_ptr() as u64;
            buf.length = 1;

            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_DQBUF, &mut buf) };
            if ret < 0 {
                return None;
            }

            let bytes_used = planes[0].bytesused as usize;
            let idx = buf.index as usize;
            let timestamp_us = buf.timestamp_sec as u64 * 1_000_000 + buf.timestamp_usec as u64;

            let data = if bytes_used > 0 && idx < self.capture_bufs.len() {
                let cb = &self.capture_bufs[idx];
                let len = bytes_used.min(cb.length);
                unsafe { std::slice::from_raw_parts(cb.ptr, len) }.to_vec()
            } else {
                Vec::new()
            };

            // Re-queue the capture buffer.
            self.qbuf_capture(idx as u32);

            Some((data, timestamp_us))
        }

        /// Try to dequeue a completed OUTPUT buffer (non-blocking).
        pub fn dequeue_output(&mut self) {
            let mut planes = [V4l2Plane::default(); 1];
            let mut buf = Self::make_buffer(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, V4L2_MEMORY_MMAP);
            buf.m_planes = planes.as_mut_ptr() as u64;
            buf.length = 1;

            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_DQBUF, &mut buf) };
            if ret == 0 {
                self.output_free.push_back(buf.index);
            }
        }

        /// Start streaming: queue first frame, STREAMON OUTPUT, queue all CAPTURE, STREAMON CAPTURE.
        /// This ordering is required by the bcm2835-codec driver.
        pub fn start_streaming(
            &mut self,
            first_nv12: &[u8],
            timestamp_us: u64,
        ) -> anyhow::Result<()> {
            // Queue first OUTPUT buffer before STREAMON (driver requirement).
            self.queue_frame(first_nv12, timestamp_us);

            // STREAMON OUTPUT.
            let mut type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
            let ret =
                unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_STREAMON, &mut type_) };
            if ret < 0 {
                anyhow::bail!(
                    "STREAMON OUTPUT failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            tracing::debug!("V4L2 raw: STREAMON OUTPUT");

            // Queue all CAPTURE buffers.
            for i in 0..self.capture_bufs.len() as u32 {
                self.qbuf_capture(i);
            }

            // STREAMON CAPTURE.
            type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
            let ret =
                unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_STREAMON, &mut type_) };
            if ret < 0 {
                anyhow::bail!(
                    "STREAMON CAPTURE failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            tracing::debug!("V4L2 raw: STREAMON CAPTURE");

            Ok(())
        }

        /// Poll the device for readiness (CAPTURE or OUTPUT).
        pub fn poll(&self, timeout_ms: i32) -> (bool, bool) {
            let mut pollfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN | libc::POLLOUT,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut pollfd, 1, timeout_ms);
            }
            let capture_ready = pollfd.revents & libc::POLLIN != 0;
            let output_ready = pollfd.revents & libc::POLLOUT != 0;
            (capture_ready, output_ready)
        }

        // --- Private helpers ---

        /// G_FMT to get current format, then modify and S_FMT.
        ///
        /// Doing G_FMT first ensures all fields have valid defaults (colorspace,
        /// sizeimage, etc.) — S_FMT with a zeroed struct is rejected by
        /// bcm2835-codec.
        fn s_fmt(
            fd: RawFd,
            type_: u32,
            pixfmt: u32,
            width: u32,
            height: u32,
        ) -> anyhow::Result<()> {
            const VIDIOC_G_FMT: libc::c_ulong = 0xC0D05604; // _IOWR('V', 4, v4l2_format)

            let mut fmt: V4l2Format = unsafe { std::mem::zeroed() };
            fmt.type_ = type_;

            // G_FMT to get valid defaults.
            let ret = unsafe { libc::ioctl(fd, VIDIOC_G_FMT, &mut fmt) };
            if ret < 0 {
                anyhow::bail!(
                    "G_FMT type={type_} failed: {}",
                    std::io::Error::last_os_error()
                );
            }

            // Modify the fields we care about.
            // Offsets within the 208-byte struct:
            //   _rest[0..4] = padding (4 bytes for 8-byte union alignment)
            //   _rest[4..] = v4l2_pix_format_mplane union start
            //   pix_mp.width      at union offset 0   → _rest[4..8]
            //   pix_mp.height     at union offset 4   → _rest[8..12]
            //   pix_mp.pixelformat at union offset 8  → _rest[12..16]
            //   pix_mp.num_planes at union offset 180 → _rest[184]
            fmt._rest[4..8].copy_from_slice(&width.to_ne_bytes());
            fmt._rest[8..12].copy_from_slice(&height.to_ne_bytes());
            fmt._rest[12..16].copy_from_slice(&pixfmt.to_ne_bytes());
            fmt._rest[184] = 1; // num_planes

            let ret = unsafe { libc::ioctl(fd, ioctl_nr::VIDIOC_S_FMT, &mut fmt) };
            if ret < 0 {
                anyhow::bail!(
                    "S_FMT type={type_} failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(())
        }

        fn s_ctrl(fd: RawFd, id: u32, value: i32) {
            let mut ctrl = V4l2Control { id, value };
            let ret = unsafe { libc::ioctl(fd, ioctl_nr::VIDIOC_S_CTRL, &mut ctrl) };
            if ret < 0 {
                tracing::debug!(id, value, err = %std::io::Error::last_os_error(), "V4L2 raw: S_CTRL failed");
            }
        }

        fn s_parm(fd: RawFd, framerate: u32) {
            let mut parm: V4l2Streamparm = unsafe { std::mem::zeroed() };
            parm.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
            // v4l2_outputparm.timeperframe: numerator=1, denominator=fps
            // Offset in parm union for outputparm.timeperframe: bytes 4..12
            parm.parm[4..8].copy_from_slice(&1u32.to_ne_bytes());
            parm.parm[8..12].copy_from_slice(&framerate.to_ne_bytes());
            let ret = unsafe { libc::ioctl(fd, ioctl_nr::VIDIOC_S_PARM, &mut parm) };
            if ret < 0 {
                tracing::debug!(framerate, "V4L2 raw: S_PARM failed");
            }
        }

        fn reqbufs(fd: RawFd, type_: u32, count: u32) -> anyhow::Result<usize> {
            let mut req = V4l2Requestbuffers {
                count,
                type_,
                memory: V4L2_MEMORY_MMAP,
                capabilities: 0,
                flags: 0,
                reserved: [0; 3],
            };
            let ret = unsafe { libc::ioctl(fd, ioctl_nr::VIDIOC_REQBUFS, &mut req) };
            if ret < 0 {
                anyhow::bail!(
                    "REQBUFS type={type_} failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(req.count as usize)
        }

        fn mmap_buffers(fd: RawFd, type_: u32, count: usize) -> anyhow::Result<Vec<MmapBuffer>> {
            let mut bufs = Vec::with_capacity(count);
            for i in 0..count {
                let mut planes = [V4l2Plane::default(); 1];
                let mut buf = Self::make_buffer(type_, V4L2_MEMORY_MMAP);
                buf.index = i as u32;
                buf.m_planes = planes.as_mut_ptr() as u64;
                buf.length = 1;

                let ret = unsafe { libc::ioctl(fd, ioctl_nr::VIDIOC_QUERYBUF, &mut buf) };
                if ret < 0 {
                    anyhow::bail!(
                        "QUERYBUF type={type_} idx={i} failed: {}",
                        std::io::Error::last_os_error()
                    );
                }

                let offset = planes[0].m_offset;
                let length = planes[0].length as usize;

                let ptr = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        length,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        fd,
                        offset as i64,
                    )
                };
                if ptr == libc::MAP_FAILED {
                    anyhow::bail!(
                        "mmap type={type_} idx={i} failed: {}",
                        std::io::Error::last_os_error()
                    );
                }

                bufs.push(MmapBuffer {
                    ptr: ptr as *mut u8,
                    length,
                });
            }
            Ok(bufs)
        }

        fn qbuf_output(&self, index: u32, bytesused: u32, timestamp_us: u64) {
            let mut planes = [V4l2Plane {
                bytesused,
                length: self.output_bufs[index as usize].length as u32,
                m_offset: 0,
                data_offset: 0,
                reserved: [0; 11],
            }];
            let mut buf = Self::make_buffer(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, V4L2_MEMORY_MMAP);
            buf.index = index;
            buf.m_planes = planes.as_mut_ptr() as u64;
            buf.length = 1;
            buf.timestamp_sec = (timestamp_us / 1_000_000) as i64;
            buf.timestamp_usec = (timestamp_us % 1_000_000) as i64;

            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_QBUF, &mut buf) };
            if ret < 0 {
                tracing::warn!(
                    index,
                    "V4L2 raw: QBUF OUTPUT failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }

        fn qbuf_capture(&self, index: u32) {
            let mut planes = [V4l2Plane {
                bytesused: 0,
                length: self.capture_bufs[index as usize].length as u32,
                m_offset: 0,
                data_offset: 0,
                reserved: [0; 11],
            }];
            let mut buf = Self::make_buffer(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, V4L2_MEMORY_MMAP);
            buf.index = index;
            buf.m_planes = planes.as_mut_ptr() as u64;
            buf.length = 1;

            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_nr::VIDIOC_QBUF, &mut buf) };
            if ret < 0 {
                tracing::warn!(
                    index,
                    "V4L2 raw: QBUF CAPTURE failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }

        fn make_buffer(type_: u32, memory: u32) -> V4l2Buffer {
            V4l2Buffer {
                index: 0,
                type_,
                bytesused: 0,
                flags: 0,
                field: 0,
                _pad: 0,
                timestamp_sec: 0,
                timestamp_usec: 0,
                timecode: [0; 4],
                sequence: 0,
                memory,
                m_planes: 0,
                length: 0,
                reserved2: 0,
                _union: 0,
                reserved: 0,
            }
        }
    }
}

/// Runs the V4L2 M2M encoder using raw ioctls.
#[allow(clippy::too_many_arguments)]
fn encoder_thread(
    device_path: std::path::PathBuf,
    width: u32,
    height: u32,
    bitrate: u64,
    keyframe_interval: u32,
    framerate: u32,
    input_rx: Receiver<EncoderCmd>,
    output_tx: SyncSender<EncodedOutput>,
    init_tx: SyncSender<Result<()>>,
) {
    let result: Result<()> = (|| {
        tracing::info!(
            ?device_path,
            width,
            height,
            bitrate,
            "V4L2 encoder thread: starting init"
        );

        let mut enc = raw_v4l2::RawEncoder::open(
            &device_path,
            width,
            height,
            bitrate,
            keyframe_interval,
            framerate,
        )?;

        tracing::info!("V4L2 encoder: device opened and configured");

        // Signal successful init so the encode pipeline can start feeding frames.
        init_tx
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("init channel closed"))?;

        // Wait for first frame before starting the V4L2 streaming pipeline
        // (the driver requires a queued OUTPUT buffer before STREAMON).
        let first_cmd = input_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("input channel closed before first frame"))?;
        let EncoderCmd::Encode {
            nv12: first_nv12,
            timestamp_us: first_ts,
        } = first_cmd;

        enc.start_streaming(&first_nv12, first_ts)?;
        tracing::info!("V4L2 encoder: streaming started");

        let mut queued_count = 1u64;
        let mut encoded_count = 0u64;

        // Main encode loop. Each iteration:
        // 1. Poll device for any ready buffers (non-blocking)
        // 2. Dequeue completed OUTPUT and CAPTURE buffers
        // 3. Try to receive a new frame (short timeout)
        // 4. Queue the frame if we have a free OUTPUT buffer
        //
        // The short recv timeout ensures we keep draining the device even
        // when the input channel is temporarily empty.
        loop {
            // Always drain device first — dequeue completed OUTPUT + CAPTURE.
            enc.dequeue_output();
            while let Some((data, ts)) = enc.dequeue_capture() {
                if !data.is_empty() {
                    encoded_count += 1;
                    if encoded_count <= 5 || encoded_count % 30 == 0 {
                        tracing::debug!(
                            encoded_count,
                            len = data.len(),
                            "V4L2 encoder: encoded packet"
                        );
                    }
                    let _ = output_tx.try_send(EncodedOutput {
                        data,
                        timestamp_us: ts,
                    });
                }
            }

            // Wait for a frame with a short timeout so we keep polling the device.
            let cmd = match input_rx.recv_timeout(Duration::from_millis(5)) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::info!("V4L2 encoder: input channel closed, shutting down");
                    break;
                }
            };
            let EncoderCmd::Encode { nv12, timestamp_us } = cmd;

            // Queue the frame.
            if enc.queue_frame(&nv12, timestamp_us) {
                queued_count += 1;
                if queued_count <= 5 || queued_count % 150 == 0 {
                    tracing::debug!(queued_count, "V4L2 encoder: frame queued");
                }
            } else {
                // All OUTPUT buffers busy — poll until one frees up, then retry.
                enc.poll(50);
                enc.dequeue_output();
                if enc.queue_frame(&nv12, timestamp_us) {
                    queued_count += 1;
                } else {
                    tracing::debug!("V4L2 encoder: dropping frame, all buffers busy");
                }
            }
        }

        Ok(())
    })();

    if let Err(e) = &result {
        tracing::error!("V4L2 encoder thread error: {e:#}");
        let _ = init_tx.send(Err(anyhow::anyhow!("{e:#}")));
    }
}

/// Packs [`Nv12Planes`] into a contiguous Y+UV buffer, stripping stride padding.
fn nv12_planes_to_contiguous(planes: &crate::format::Nv12Planes) -> Vec<u8> {
    let w = planes.width as usize;
    let h = planes.height as usize;
    let uv_h = h.div_ceil(2);
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
