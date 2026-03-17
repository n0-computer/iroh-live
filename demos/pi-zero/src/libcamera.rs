/// Video source that captures via `rpicam-vid` (libcamera) subprocess.
///
/// On Raspberry Pi OS Bookworm, the CSI camera is only accessible through
/// the libcamera stack. Direct V4L2 access to `/dev/video0` gives raw Bayer
/// data from the Unicam sensor, which is unusable without the ISP pipeline.
///
/// This source spawns `rpicam-vid --codec yuv420` to get processed I420
/// frames via stdout, avoiding the need for libcamera Rust bindings.
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use rusty_codecs::format::{PixelFormat, VideoFormat, VideoFrame};
use rusty_codecs::traits::VideoSource;

pub(crate) struct LibcameraSource {
    width: u32,
    height: u32,
    framerate: u32,
    child: Option<Child>,
    frame_size: usize,
    frame_count: u64,
}

impl LibcameraSource {
    pub(crate) fn new(width: u32, height: u32, framerate: u32) -> Self {
        let frame_size = (width as usize) * (height as usize) * 3 / 2; // I420
        Self {
            width,
            height,
            framerate,
            child: None,
            frame_size,
            frame_count: 0,
        }
    }
}

impl VideoSource for LibcameraSource {
    fn name(&self) -> &str {
        "libcamera"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            // The encoder will receive I420 frames via FrameData::I420,
            // but VideoFormat describes the logical format. Since the encode
            // pipeline checks FrameData variant directly, this is fine.
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        tracing::info!(
            width = self.width,
            height = self.height,
            fps = self.framerate,
            "starting rpicam-vid capture"
        );

        let child = Command::new("rpicam-vid")
            .args([
                "--codec",
                "yuv420",
                "--width",
                &self.width.to_string(),
                "--height",
                &self.height.to_string(),
                "--framerate",
                &self.framerate.to_string(),
                "--timeout",
                "0", // run indefinitely
                "--nopreview",
                "-o",
                "-", // output to stdout
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn rpicam-vid — is libcamera installed?")?;

        tracing::info!(pid = child.id(), "rpicam-vid started");
        self.child = Some(child);
        self.frame_count = 0;
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let child = self.child.as_mut().context("rpicam-vid not started")?;
        let stdout = child
            .stdout
            .as_mut()
            .context("rpicam-vid stdout not available")?;

        // Read one complete I420 frame.
        let mut buf = vec![0u8; self.frame_size];
        stdout
            .read_exact(&mut buf)
            .context("rpicam-vid: failed to read frame (process exited?)")?;

        let w = self.width;
        let h = self.height;
        let y_size = (w * h) as usize;
        let uv_size = y_size / 4;

        let all = Bytes::from(buf);
        let y = all.slice(..y_size);
        let u = all.slice(y_size..y_size + uv_size);
        let v = all.slice(y_size + uv_size..y_size + 2 * uv_size);

        let pts = Duration::from_secs_f64(self.frame_count as f64 / self.framerate as f64);
        self.frame_count += 1;

        if self.frame_count == 1 {
            tracing::info!(
                frame_size = self.frame_size,
                "first frame received from rpicam-vid"
            );
        }
        if self.frame_count.is_multiple_of(self.framerate as u64 * 5) {
            tracing::debug!(frames = self.frame_count, "libcamera capture running");
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

impl Drop for LibcameraSource {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
