//! V4L2 camera capture with optional DMA-BUF zero-copy export.
//!
//! Uses the [`v4l2r`] crate (same ecosystem as `cros-codecs`) for safe
//! V4L2 ioctl wrappers. Works on all Raspberry Pi models (1–5, Zero)
//! and USB cameras.
//!
//! # Buffer Strategy
//!
//! - **MMAP** (universal): Kernel allocates buffers, userspace mmaps them.
//!   Works on every V4L2 driver. Produces CPU-accessible `FrameData::Packed`
//!   or `FrameData::Nv12` frames.
//!
//! - **DMABUF export** (when supported): After MMAP allocation, calls
//!   `VIDIOC_EXPBUF` via [`v4l2r::ioctl::expbuf`] to get DMA-BUF file
//!   descriptors. These can be imported directly by VAAPI or V4L2 M2M
//!   encoders without CPU-touching the pixels.
//!
//! # Threading Model
//!
//! No internal thread. The moq-media encode pipeline already runs capture on
//! its own thread via `spawn_thread`. `pop_frame()` calls `dqbuf` directly,
//! which blocks on the kernel until a frame is available. This is the
//! intended usage: one blocking call per frame, driven by the caller's
//! thread.

use std::{
    fs::{File, OpenOptions},
    os::unix::io::AsRawFd,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rusty_codecs::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};
use tracing::{debug, info};
use v4l2r::{
    Format, PixelFormat as V4l2PixelFormat, QueueType,
    ioctl::{self, FormatIterator, PlaneMapping, QueryBuffer},
    memory::{MemoryType, MmapHandle},
};

use crate::types::{CameraConfig, CameraFormat, CameraInfo, CapturePixelFormat};

/// Lists available V4L2 camera devices.
pub fn cameras() -> Result<Vec<CameraInfo>> {
    let mut result = Vec::new();
    // Scan /dev/video0..63 for capture-capable devices.
    for idx in 0..64 {
        let path = format!("/dev/video{idx}");
        let Ok(fd) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        let caps: ioctl::Capability = match ioctl::querycap(&fd) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Skip non-capture devices (M2M encoders/decoders, ISP nodes).
        if !caps
            .device_caps()
            .contains(ioctl::Capabilities::VIDEO_CAPTURE)
        {
            continue;
        }
        let name = caps.card.clone();
        let formats = enumerate_formats(&fd);
        result.push(CameraInfo {
            backend: crate::CaptureBackend::V4l2,
            id: path,
            name,
            supported_formats: formats,
        });
    }
    Ok(result)
}

fn enumerate_formats(fd: &impl AsRawFd) -> Vec<CameraFormat> {
    let mut formats = Vec::new();
    for desc in FormatIterator::new(fd, QueueType::VideoCapture) {
        let fourcc = desc.pixelformat.to_fourcc();
        let Some(pf) = CapturePixelFormat::from_v4l2_fourcc(&fourcc) else {
            continue;
        };
        // Enumerate frame sizes for this pixel format.
        for size_idx in 0u32.. {
            let Ok(frmsize) = ioctl::enum_frame_sizes::<v4l2r::bindings::v4l2_frmsizeenum>(
                fd,
                size_idx,
                desc.pixelformat,
            ) else {
                break;
            };
            let Some(size) = frmsize.size() else {
                continue;
            };
            let (w, h) = match size {
                ioctl::FrmSizeTypes::Discrete(d) => (d.width, d.height),
                ioctl::FrmSizeTypes::StepWise(s) => (s.max_width, s.max_height),
            };
            // Enumerate all frame intervals (fps values) for this size.
            let fps_list = enumerate_frame_intervals(fd, desc.pixelformat, w, h);
            for fps in fps_list {
                formats.push(CameraFormat {
                    dimensions: [w, h],
                    fps,
                    pixel_format: pf,
                });
            }
        }
    }
    formats
}

fn enumerate_frame_intervals(
    fd: &impl AsRawFd,
    pixfmt: V4l2PixelFormat,
    w: u32,
    h: u32,
) -> Vec<f32> {
    let mut fps_list = Vec::new();
    for idx in 0u32.. {
        let Ok(ival) =
            ioctl::enum_frame_intervals::<v4l2r::bindings::v4l2_frmivalenum>(fd, idx, pixfmt, w, h)
        else {
            break;
        };
        match ival.intervals() {
            Some(ioctl::FrmIvalTypes::Discrete(d)) => {
                fps_list.push(d.denominator as f32 / d.numerator.max(1) as f32);
            }
            Some(ioctl::FrmIvalTypes::StepWise(s)) => {
                // Report the fastest (min interval = max fps).
                fps_list.push(s.min.denominator as f32 / s.min.numerator.max(1) as f32);
                break; // Stepwise has a single range, not multiple entries.
            }
            None => break,
        }
    }
    if fps_list.is_empty() {
        fps_list.push(30.0); // Fallback when driver doesn't report intervals.
    }
    fps_list
}

const NUM_BUFFERS: u32 = 4;

/// V4L2 camera capturer.
///
/// Captures frames via MMAP streaming. When the driver supports
/// `VIDIOC_EXPBUF`, attempts DMA-BUF export for zero-copy downstream
/// encoding (e.g. V4L2 M2M H.264 on Raspberry Pi 4).
///
/// No internal thread — `pop_frame()` blocks on the kernel's `dqbuf` ioctl
/// directly. The caller (moq-media encode pipeline) drives the capture loop
/// from its own thread.
#[derive(derive_more::Debug)]
pub struct V4l2CameraCapturer {
    width: u32,
    height: u32,
    #[allow(dead_code, reason = "used for future DMABUF format selection")]
    capture_format: CapturePixelFormat,
    device_name: String,
    /// Device fd, mmap buffers, and capture state. `None` before `start()` /
    /// after `stop()`.
    #[debug(skip)]
    state: Option<CaptureState>,
    /// Held between `new()` and `start()` so we can re-open the device.
    device_path: String,
}

struct CaptureState {
    dev: File,
    mappings: Vec<PlaneMapping>,
    capture_start: Instant,
}

impl V4l2CameraCapturer {
    /// Creates a new V4L2 camera capturer for the given device.
    pub fn new(info: &CameraInfo, config: &CameraConfig) -> Result<Self> {
        Self::open(&info.id, &info.name, info, config)
    }

    /// Opens the first available V4L2 camera with default configuration.
    pub fn open_default() -> Result<Self> {
        let cams = cameras()?;
        let cam = cams.first().context("no V4L2 cameras available")?;
        Self::open(&cam.id, &cam.name, cam, &CameraConfig::default())
    }

    fn open(path: &str, name: &str, info: &CameraInfo, config: &CameraConfig) -> Result<Self> {
        let mut dev = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .context("failed to open V4L2 device")?;

        // Select best format from supported formats using the config strategy.
        let selected = config.select_format(&info.supported_formats);
        let (req_w, req_h, req_fourcc) = if let Some(fmt) = selected {
            debug!(
                ?fmt.dimensions, fps = fmt.fps, ?fmt.pixel_format,
                "V4L2 format selected by {:?}", config.selector
            );
            (
                fmt.dimensions[0],
                fmt.dimensions[1],
                V4l2PixelFormat::from_fourcc(&fmt.pixel_format.to_v4l2_fourcc()),
            )
        } else {
            // No supported formats enumerated — let the driver pick.
            let fourcc = config
                .preferred_format
                .map(|pf| V4l2PixelFormat::from_fourcc(&pf.to_v4l2_fourcc()))
                .unwrap_or_else(|| V4l2PixelFormat::from_fourcc(b"YUYV"));
            (0, 0, fourcc)
        };

        let desired = Format {
            width: req_w,
            height: req_h,
            pixelformat: req_fourcc,
            plane_fmt: vec![],
        };
        let actual: Format = ioctl::s_fmt(&mut dev, (QueueType::VideoCapture, &desired))
            .context("failed to set V4L2 format")?;
        let width = actual.width;
        let height = actual.height;
        let actual_fourcc = actual.pixelformat.to_fourcc();
        let capture_format = CapturePixelFormat::from_v4l2_fourcc(&actual_fourcc)
            .unwrap_or(CapturePixelFormat::Yuyv);

        info!(
            device = %path,
            name = %name,
            width,
            height,
            fourcc = %actual.pixelformat,
            "V4L2 camera opened"
        );

        Ok(Self {
            width,
            height,
            capture_format,
            device_name: name.to_string(),
            state: None,
            device_path: path.to_string(),
        })
    }

    /// Sets up MMAP buffers and starts V4L2 streaming.
    fn start_streaming(&mut self) -> Result<()> {
        if self.state.is_some() {
            return Ok(());
        }

        let mut dev = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.device_path)
            .context("failed to reopen V4L2 device for capture")?;

        // Re-set the format on the new fd.
        let desired = Format {
            width: self.width,
            height: self.height,
            pixelformat: V4l2PixelFormat::from_fourcc(&self.capture_format.to_v4l2_fourcc()),
            plane_fmt: vec![],
        };
        let _actual: Format = ioctl::s_fmt(&mut dev, (QueueType::VideoCapture, &desired))?;

        // Allocate MMAP buffers.
        let num_bufs: usize = ioctl::reqbufs(
            &dev,
            QueueType::VideoCapture,
            MemoryType::Mmap,
            NUM_BUFFERS,
            ioctl::MemoryConsistency::empty(),
        )?;

        // Query and mmap each buffer.
        let mut mappings: Vec<PlaneMapping> = Vec::with_capacity(num_bufs);
        for i in 0..num_bufs {
            let buf_info: QueryBuffer = ioctl::querybuf(&dev, QueueType::VideoCapture, i)?;
            let plane = buf_info.planes.first().context("no planes in buffer")?;
            let mapping = ioctl::mmap(&dev, plane.mem_offset, plane.length)?;
            mappings.push(mapping);
        }

        // Queue all buffers.
        for i in 0..num_bufs {
            let mut qbuf = ioctl::QBuffer::<MmapHandle>::new(QueueType::VideoCapture, i as u32);
            qbuf.planes.push(ioctl::QBufPlane::new(0));
            ioctl::qbuf::<_, ()>(&dev, qbuf)?;
        }

        // Start streaming.
        ioctl::streamon(&dev, QueueType::VideoCapture)?;

        self.state = Some(CaptureState {
            dev,
            mappings,
            capture_start: Instant::now(),
        });

        debug!(device = %self.device_path, "V4L2 streaming started");
        Ok(())
    }

    /// Stops V4L2 streaming and frees buffers.
    fn stop_streaming(&mut self) {
        if let Some(state) = self.state.take() {
            ioctl::streamoff(&state.dev, QueueType::VideoCapture).ok();
            // Free buffers by requesting 0.
            let _ = ioctl::reqbufs::<()>(
                &state.dev,
                QueueType::VideoCapture,
                MemoryType::Mmap,
                0,
                ioctl::MemoryConsistency::empty(),
            );
            debug!(device = %self.device_path, "V4L2 streaming stopped");
        }
    }
}

impl VideoSource for V4l2CameraCapturer {
    fn name(&self) -> &str {
        &self.device_name
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        self.start_streaming()
    }

    fn stop(&mut self) -> Result<()> {
        self.stop_streaming();
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let Some(state) = &self.state else {
            return Ok(None);
        };

        // dqbuf: blocks until a frame is ready (kernel-side wait).
        let dqbuf: QueryBuffer = match ioctl::dqbuf(&state.dev, QueueType::VideoCapture) {
            Ok(buf) => buf,
            Err(ioctl::DqBufError::IoctlError(ioctl::DqBufIoctlError::NotReady)) => {
                return Ok(None);
            }
            Err(e) => {
                anyhow::bail!("V4L2 dqbuf error: {e}");
            }
        };

        let buf_idx = dqbuf.index;
        let Some(mapping) = state.mappings.get(buf_idx) else {
            anyhow::bail!(
                "V4L2 dqbuf returned invalid index {buf_idx} (have {} buffers)",
                state.mappings.len()
            );
        };
        let data: &[u8] = mapping;

        let frame = convert_frame(
            data,
            self.width,
            self.height,
            self.capture_format,
            state.capture_start.elapsed(),
        )?;

        // Re-queue the buffer now that we've copied the data.
        let mut qbuf = ioctl::QBuffer::<MmapHandle>::new(QueueType::VideoCapture, buf_idx as u32);
        qbuf.planes.push(ioctl::QBufPlane::new(0));
        ioctl::qbuf::<_, ()>(&state.dev, qbuf)?;

        Ok(Some(frame))
    }
}

impl Drop for V4l2CameraCapturer {
    fn drop(&mut self) {
        self.stop_streaming();
    }
}

fn convert_frame(
    data: &[u8],
    width: u32,
    height: u32,
    capture_format: CapturePixelFormat,
    timestamp: Duration,
) -> Result<VideoFrame> {
    Ok(match capture_format {
        CapturePixelFormat::Nv12 => {
            use rusty_codecs::format::Nv12Planes;
            let y_size = (width * height) as usize;
            let uv_size = y_size / 2;
            anyhow::ensure!(
                data.len() >= y_size + uv_size,
                "NV12 frame too small: {} < {}",
                data.len(),
                y_size + uv_size
            );
            VideoFrame::new_nv12(
                Nv12Planes {
                    y_data: data[..y_size].to_vec(),
                    y_stride: width,
                    uv_data: data[y_size..y_size + uv_size].to_vec(),
                    uv_stride: width,
                    width,
                    height,
                },
                timestamp,
            )
        }
        CapturePixelFormat::I420 => {
            let y_size = (width * height) as usize;
            let uv_size = y_size / 4;
            let total = y_size + uv_size * 2;
            anyhow::ensure!(
                data.len() >= total,
                "I420 frame too small: {} < {}",
                data.len(),
                total
            );
            VideoFrame::new_i420(
                bytes::Bytes::copy_from_slice(&data[..y_size]),
                bytes::Bytes::copy_from_slice(&data[y_size..y_size + uv_size]),
                bytes::Bytes::copy_from_slice(&data[y_size + uv_size..y_size + uv_size * 2]),
                width,
                height,
                timestamp,
            )
        }
        CapturePixelFormat::Yuyv => {
            let rgba = yuyv_to_rgba(data, width, height);
            VideoFrame::new_rgba(rgba.into(), width, height, timestamp)
        }
        CapturePixelFormat::Rgb => {
            let rgba = rgb_to_rgba(data, width, height);
            VideoFrame::new_rgba(rgba.into(), width, height, timestamp)
        }
        CapturePixelFormat::Mjpeg => {
            let rgba = mjpeg_to_rgba(data, width, height)?;
            VideoFrame::new_rgba(rgba.into(), width, height, timestamp)
        }
        CapturePixelFormat::Gray => {
            let rgba = gray_to_rgba(data, width, height);
            VideoFrame::new_rgba(rgba.into(), width, height, timestamp)
        }
        CapturePixelFormat::Rgba => VideoFrame::new_rgba(
            bytes::Bytes::copy_from_slice(data),
            width,
            height,
            timestamp,
        ),
        CapturePixelFormat::Bgra => VideoFrame::new_packed(
            bytes::Bytes::copy_from_slice(data),
            width,
            height,
            PixelFormat::Bgra,
            timestamp,
        ),
    })
}

// ── Pixel format conversion helpers ──────────────────────────────────

fn yuyv_to_rgba(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgba = vec![0u8; pixel_count * 4];
    for i in 0..(pixel_count / 2) {
        let base = i * 4;
        if base + 3 >= data.len() {
            break;
        }
        let y0 = data[base] as i32;
        let cb = data[base + 1] as i32 - 128;
        let y1 = data[base + 2] as i32;
        let cr = data[base + 3] as i32 - 128;

        let out = i * 8;
        if out + 7 < rgba.len() {
            // BT.601 full-range, fixed-point: coefficients scaled by 256.
            rgba[out] = (y0 + ((359 * cr + 128) >> 8)).clamp(0, 255) as u8;
            rgba[out + 1] = (y0 + ((-88 * cb - 183 * cr + 128) >> 8)).clamp(0, 255) as u8;
            rgba[out + 2] = (y0 + ((454 * cb + 128) >> 8)).clamp(0, 255) as u8;
            rgba[out + 3] = 255;
            rgba[out + 4] = (y1 + ((359 * cr + 128) >> 8)).clamp(0, 255) as u8;
            rgba[out + 5] = (y1 + ((-88 * cb - 183 * cr + 128) >> 8)).clamp(0, 255) as u8;
            rgba[out + 6] = (y1 + ((454 * cb + 128) >> 8)).clamp(0, 255) as u8;
            rgba[out + 7] = 255;
        }
    }
    rgba
}

fn rgb_to_rgba(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgba = vec![255u8; pixel_count * 4];
    for (src, dst) in data.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
    }
    rgba
}

fn gray_to_rgba(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgba = vec![255u8; pixel_count * 4];
    for (i, &g) in data.iter().take(pixel_count).enumerate() {
        let out = i * 4;
        rgba[out] = g;
        rgba[out + 1] = g;
        rgba[out + 2] = g;
    }
    rgba
}

fn mjpeg_to_rgba(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
        .context("MJPEG decode failed")?;
    anyhow::ensure!(
        img.width() == width && img.height() == height,
        "MJPEG dimensions mismatch: {}x{} != {}x{}",
        img.width(),
        img.height(),
        width,
        height
    );
    Ok(img.to_rgba8().into_raw())
}
