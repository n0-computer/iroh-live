//! Cross-platform webcam capture via the [`nokhwa`] crate.
//!
//! Works on Linux (V4L2), macOS (AVFoundation), and Windows (Media
//! Foundation). Produces CPU-backed RGBA frames — no zero-copy path.
//!
//! Because `nokhwa::Camera` is not `Send`, capture runs on a dedicated
//! OS thread. The camera is created, opened, and driven entirely within
//! that thread; the main thread communicates via bounded channels.

use std::sync::mpsc;
use std::time::Instant;

use anyhow::{Context, Result};
use nokhwa::Camera;
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType};
use tracing::{debug, warn};

use rusty_codecs::format::{PixelFormat, VideoFormat, VideoFrame};
use rusty_codecs::traits::VideoSource;

use crate::types::{CameraConfig, CameraFormat, CameraInfo, CaptureBackend, CapturePixelFormat};

/// Converts a nokhwa [`FrameFormat`](nokhwa::utils::FrameFormat) to a
/// [`CapturePixelFormat`].
fn convert_frame_format(ff: nokhwa::utils::FrameFormat) -> Option<CapturePixelFormat> {
    match ff {
        nokhwa::utils::FrameFormat::MJPEG => Some(CapturePixelFormat::Mjpeg),
        nokhwa::utils::FrameFormat::YUYV => Some(CapturePixelFormat::Yuyv),
        nokhwa::utils::FrameFormat::NV12 => Some(CapturePixelFormat::Nv12),
        nokhwa::utils::FrameFormat::GRAY => Some(CapturePixelFormat::Gray),
        nokhwa::utils::FrameFormat::RAWRGB => Some(CapturePixelFormat::Rgb),
        _ => None,
    }
}

/// Enumerates cameras visible to nokhwa.
pub fn cameras() -> Result<Vec<CameraInfo>> {
    let infos = nokhwa::query(ApiBackend::Auto).context("nokhwa: failed to query cameras")?;
    let mut result = Vec::with_capacity(infos.len());
    for info in &infos {
        let index = info.index().clone();
        let name = info.human_name();

        // Try to enumerate supported formats by briefly opening the camera.
        let supported_formats = enumerate_formats(&index).unwrap_or_else(|e| {
            warn!(camera = %name, error = %e, "nokhwa: failed to enumerate formats, reporting empty list");
            Vec::new()
        });

        let id = match &index {
            CameraIndex::Index(i) => format!("nokhwa-{i}"),
            CameraIndex::String(s) => format!("nokhwa-{s}"),
        };

        result.push(CameraInfo {
            backend: CaptureBackend::Nokhwa,
            id,
            name,
            supported_formats,
        });
    }
    Ok(result)
}

/// Opens a camera briefly to query its supported formats.
fn enumerate_formats(index: &CameraIndex) -> Result<Vec<CameraFormat>> {
    let format = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut cam =
        Camera::new(index.clone(), format).context("nokhwa: failed to open camera for query")?;
    let nokhwa_formats = cam
        .compatible_camera_formats()
        .context("nokhwa: failed to list compatible formats")?;
    let mut formats = Vec::new();
    for cf in nokhwa_formats {
        let Some(pf) = convert_frame_format(cf.format()) else {
            continue;
        };
        formats.push(CameraFormat {
            dimensions: [cf.resolution().width(), cf.resolution().height()],
            fps: cf.frame_rate() as f32,
            pixel_format: pf,
        });
    }
    Ok(formats)
}

/// Parses a camera index from our id format ("nokhwa-0" or "nokhwa-some-string").
fn parse_camera_index(id: &str) -> CameraIndex {
    let suffix = id.strip_prefix("nokhwa-").unwrap_or(id);
    if let Ok(n) = suffix.parse::<u32>() {
        CameraIndex::Index(n)
    } else {
        CameraIndex::String(suffix.to_string())
    }
}

/// Command sent from the [`VideoSource`] methods to the capture thread.
enum CaptureCmd {
    Start,
    Stop,
    Shutdown,
}

/// Result sent back from the capture thread after initialization.
struct InitResult {
    dimensions: [u32; 2],
    fps: u32,
}

/// Camera capturer backed by [`nokhwa`].
///
/// Runs capture on a dedicated OS thread because `nokhwa::Camera` is not
/// `Send`. The camera is created and driven entirely on that thread;
/// frames flow back through a bounded channel.
pub struct NokhwaCameraCapturer {
    camera_name: String,
    dimensions: [u32; 2],
    frame_rx: mpsc::Receiver<VideoFrame>,
    cmd_tx: mpsc::SyncSender<CaptureCmd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for NokhwaCameraCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NokhwaCameraCapturer")
            .field("camera_name", &self.camera_name)
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

impl NokhwaCameraCapturer {
    /// Opens a nokhwa camera matching the given info and config.
    pub fn new(info: &CameraInfo, config: &CameraConfig) -> Result<Self> {
        let index = parse_camera_index(&info.id);

        // Build the nokhwa RequestedFormat from our CameraConfig.
        let requested = match &config.selector {
            crate::types::CameraSelector::HighestResolution => {
                RequestedFormatType::AbsoluteHighestResolution
            }
            crate::types::CameraSelector::HighestFramerate => {
                RequestedFormatType::AbsoluteHighestFrameRate
            }
            crate::types::CameraSelector::TargetResolution(w, h) => {
                let resolution = nokhwa::utils::Resolution::new(*w, *h);
                RequestedFormatType::HighestResolution(resolution)
            }
        };
        let req_format = RequestedFormat::new::<RgbAFormat>(requested);
        let camera_name = info.name.clone();

        // Bounded channel: 2 frames of buffering keeps latency low.
        let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(2);
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<CaptureCmd>(4);
        // One-shot channel for the camera init result.
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<InitResult>>(1);

        let thread_name = format!("nokhwa-{camera_name}");
        let thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || capture_thread(index, req_format, init_tx, cmd_rx, frame_tx))
            .context("nokhwa: failed to spawn capture thread")?;

        // Wait for the camera to be created on the capture thread.
        let init = init_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("nokhwa: capture thread exited during init"))??;

        debug!(
            name = %camera_name,
            width = init.dimensions[0],
            height = init.dimensions[1],
            fps = init.fps,
            "nokhwa camera capturer created"
        );

        Ok(Self {
            camera_name,
            dimensions: init.dimensions,
            frame_rx,
            cmd_tx,
            thread: Some(thread),
        })
    }
}

/// Entry point for the capture thread. Creates the camera here so it
/// never crosses a thread boundary.
fn capture_thread(
    index: CameraIndex,
    req_format: RequestedFormat<'static>,
    init_tx: mpsc::SyncSender<Result<InitResult>>,
    cmd_rx: mpsc::Receiver<CaptureCmd>,
    frame_tx: mpsc::SyncSender<VideoFrame>,
) {
    let camera = match Camera::new(index, req_format) {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(anyhow::anyhow!("nokhwa: failed to open camera: {e}")));
            return;
        }
    };

    let cf = camera.camera_format();
    let dimensions = [cf.resolution().width(), cf.resolution().height()];
    let fps = cf.frame_rate();

    if init_tx.send(Ok(InitResult { dimensions, fps })).is_err() {
        return;
    }

    capture_loop(camera, cmd_rx, frame_tx);
}

/// Drives the camera, responding to commands and forwarding frames.
fn capture_loop(
    mut camera: Camera,
    cmd_rx: mpsc::Receiver<CaptureCmd>,
    frame_tx: mpsc::SyncSender<VideoFrame>,
) {
    let mut streaming = false;
    let mut start_time = Instant::now();

    loop {
        // Drain commands (non-blocking).
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                CaptureCmd::Start => {
                    if !streaming {
                        if let Err(e) = camera.open_stream() {
                            warn!(error = %e, "nokhwa: open_stream failed");
                        } else {
                            streaming = true;
                            start_time = Instant::now();
                        }
                    }
                }
                CaptureCmd::Stop => {
                    if streaming {
                        if let Err(e) = camera.stop_stream() {
                            warn!(error = %e, "nokhwa: stop_stream failed");
                        }
                        streaming = false;
                    }
                }
                CaptureCmd::Shutdown => {
                    if streaming {
                        let _ = camera.stop_stream();
                    }
                    return;
                }
            }
        }

        if !streaming {
            // Block until a command arrives instead of busy-looping.
            match cmd_rx.recv() {
                Ok(CaptureCmd::Start) => {
                    if let Err(e) = camera.open_stream() {
                        warn!(error = %e, "nokhwa: open_stream failed");
                    } else {
                        streaming = true;
                        start_time = Instant::now();
                    }
                }
                Ok(CaptureCmd::Shutdown) | Err(_) => return,
                Ok(CaptureCmd::Stop) => {}
            }
            continue;
        }

        // Capture a frame.
        let buffer = match camera.frame() {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "nokhwa: frame capture failed");
                continue;
            }
        };

        let timestamp = start_time.elapsed();
        let res = buffer.resolution();
        let w = res.width();
        let h = res.height();

        let rgba_image = match buffer.decode_image::<RgbAFormat>() {
            Ok(img) => img,
            Err(e) => {
                warn!(error = %e, "nokhwa: frame decode failed");
                continue;
            }
        };

        let data: bytes::Bytes = rgba_image.into_raw().into();
        let frame = VideoFrame::new_rgba(data, w, h, timestamp);

        // Non-blocking send: drop the frame if the consumer is too slow.
        if frame_tx.try_send(frame).is_err() {
            // Consumer is behind; drop the frame to keep latency low.
        }
    }
}

impl VideoSource for NokhwaCameraCapturer {
    fn name(&self) -> &str {
        &self.camera_name
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: self.dimensions,
        }
    }

    fn start(&mut self) -> Result<()> {
        self.cmd_tx
            .send(CaptureCmd::Start)
            .map_err(|_| anyhow::anyhow!("nokhwa: capture thread exited"))
    }

    fn stop(&mut self) -> Result<()> {
        self.cmd_tx
            .send(CaptureCmd::Stop)
            .map_err(|_| anyhow::anyhow!("nokhwa: capture thread exited"))
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        match self.frame_rx.try_recv() {
            Ok(frame) => Ok(Some(frame)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                anyhow::bail!("nokhwa: capture thread exited unexpectedly")
            }
        }
    }
}

impl Drop for NokhwaCameraCapturer {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(CaptureCmd::Shutdown);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}
