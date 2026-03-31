//! Raspberry Pi camera capture via `rpicam-vid` (libcamera subprocess).
//!
//! On Raspberry Pi OS Bookworm, the CSI camera is only accessible through
//! the libcamera stack. Direct V4L2 access to `/dev/video0` yields raw
//! Bayer data from the Unicam sensor, which is unusable without the ISP
//! pipeline that libcamera provides.
//!
//! This backend spawns `rpicam-vid --codec yuv420` and reads raw I420
//! frames from its stdout pipe. It produces [`VideoFrame`] values ready
//! for encoding by a separate encoder (openh264, V4L2 M2M, etc.).
//!
//! For the pre-encoded H.264 path (where rpicam-vid does the encoding
//! internally), see [`LibcameraH264Source`](super::libcamera_h264::LibcameraH264Source)
//! (requires the `libcamera-h264` feature).

use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};
use bytes::Bytes;
use rusty_codecs::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};

use crate::types::{CameraInfo, CaptureBackend};

/// Lists libcamera-accessible cameras by probing `rpicam-vid --list-cameras`.
///
/// Returns a single [`CameraInfo`] entry when a camera is detected, or an
/// empty list when rpicam-vid is not installed or no camera is connected.
pub fn cameras() -> Result<Vec<CameraInfo>> {
    let output = match Command::new("rpicam-vid")
        .args(["--list-cameras"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("rpicam-vid not found, skipping libcamera backend");
            return Ok(vec![]);
        }
        Err(e) => {
            tracing::debug!(error = %e, "rpicam-vid --list-cameras failed");
            return Ok(vec![]);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // rpicam-vid prints camera info to stderr, not stdout.
    let combined = format!("{stdout}{stderr}");

    // Parse camera count from output like "Available cameras: 1" or
    // individual camera entries starting with a number and colon.
    let has_camera = combined.contains(": /base/") || combined.contains("Available cameras");
    let no_cameras =
        combined.contains("No cameras available") || combined.contains("Available cameras\n0");

    if !has_camera || no_cameras {
        return Ok(vec![]);
    }

    // Extract camera name from lines like "0 : imx219 [3280x2464 10-bit RGGB]"
    let name = combined
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("0 :") || trimmed.starts_with("0:") {
                Some(
                    trimmed
                        .trim_start_matches("0 :")
                        .trim_start_matches("0:")
                        .trim()
                        .to_string(),
                )
            } else {
                None
            }
        })
        .unwrap_or_else(|| "libcamera CSI camera".to_string());

    Ok(vec![CameraInfo {
        backend: CaptureBackend::Libcamera,
        id: "libcamera:0".into(),
        name,
        // rpicam-vid does not expose per-format enumeration in a machine-
        // readable way. Resolution and framerate are configured at open time.
        supported_formats: vec![],
    }])
}

/// Configuration for the libcamera YUV capture backend.
#[derive(Debug, Clone)]
pub struct LibcameraConfig {
    /// Capture width in pixels.
    pub width: u32,
    /// Capture height in pixels.
    pub height: u32,
    /// Capture framerate.
    pub framerate: u32,
}

impl Default for LibcameraConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            framerate: 30,
        }
    }
}

/// Raw I420 video capture via `rpicam-vid --codec yuv420`.
///
/// Implements [`VideoSource`], producing uncompressed I420 frames suitable
/// for any encoder. Each instance owns an `rpicam-vid` subprocess.
///
/// # Threading
///
/// `pop_frame()` blocks on the pipe read until a complete frame arrives.
/// The moq-media encode pipeline drives capture from its own thread via
/// `spawn_thread`, so this blocking behavior is expected.
pub struct LibcameraCapturer {
    config: LibcameraConfig,
    child: Option<Child>,
    frame_size: usize,
    frame_count: u64,
}

impl std::fmt::Debug for LibcameraCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibcameraCapturer")
            .field("config", &self.config)
            .field("frame_count", &self.frame_count)
            .field("running", &self.child.is_some())
            .finish()
    }
}

impl LibcameraCapturer {
    /// Creates a capturer with the given configuration.
    pub fn new(config: LibcameraConfig) -> Self {
        let frame_size = (config.width as usize) * (config.height as usize) * 3 / 2;
        Self {
            config,
            child: None,
            frame_size,
            frame_count: 0,
        }
    }

    /// Creates a capturer with default settings (640x360 @ 30 fps).
    pub fn new_default() -> Self {
        Self::new(LibcameraConfig::default())
    }
}

impl VideoSource for LibcameraCapturer {
    fn name(&self) -> &str {
        "libcamera-yuv"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            // I420 frames are produced, but VideoFormat only has Rgba/Bgra.
            // Downstream code inspects the actual FrameData variant.
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.config.width, self.config.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        let c = &self.config;
        tracing::info!(
            width = c.width,
            height = c.height,
            fps = c.framerate,
            "starting rpicam-vid capture (YUV420)"
        );

        let child = Command::new("rpicam-vid")
            .args([
                "--codec",
                "yuv420",
                "--width",
                &c.width.to_string(),
                "--height",
                &c.height.to_string(),
                "--framerate",
                &c.framerate.to_string(),
                "--timeout",
                "0",
                "--nopreview",
                "-o",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn rpicam-vid — is libcamera installed?")?;

        tracing::info!(pid = child.id(), "rpicam-vid started (YUV420 mode)");
        self.child = Some(child);
        self.frame_count = 0;
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let child = self.child.as_mut().context("rpicam-vid not started")?;
        let stdout = child
            .stdout
            .as_mut()
            .context("rpicam-vid stdout unavailable")?;

        let mut buf = vec![0u8; self.frame_size];
        stdout
            .read_exact(&mut buf)
            .context("rpicam-vid: read failed (process exited?)")?;

        let w = self.config.width;
        let h = self.config.height;
        let y_size = (w * h) as usize;
        let uv_size = y_size / 4;

        let all = Bytes::from(buf);
        let y = all.slice(..y_size);
        let u = all.slice(y_size..y_size + uv_size);
        let v = all.slice(y_size + uv_size..y_size + 2 * uv_size);

        let pts = Duration::from_secs_f64(self.frame_count as f64 / self.config.framerate as f64);
        self.frame_count += 1;

        if self.frame_count == 1 {
            tracing::info!(
                frame_size = self.frame_size,
                "first YUV frame from rpicam-vid"
            );
        }
        if self
            .frame_count
            .is_multiple_of(self.config.framerate as u64 * 5)
        {
            tracing::debug!(frames = self.frame_count, "libcamera YUV capture running");
        }

        Ok(Some(VideoFrame::new_i420(y, u, v, w, h, pts)))
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            tracing::info!("stopping rpicam-vid");
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for LibcameraCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
