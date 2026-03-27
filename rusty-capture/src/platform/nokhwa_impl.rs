//! Cross-platform webcam capture via the [`nokhwa`] crate.
//!
//! Works on Linux (V4L2), macOS (AVFoundation), and Windows (Media
//! Foundation). Produces CPU-backed RGBA frames — no zero-copy path.
//!
//! # Threading Model
//!
//! No internal thread. The `camera-sync-impl` nokhwa feature makes
//! `Camera` `Send`, so `pop_frame()` calls `camera.frame()` directly on
//! the caller's thread (moq-media's encode pipeline thread).

use std::time::Instant;

use anyhow::{Context, Result};
use nokhwa::{
    Camera,
    pixel_format::RgbAFormat,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType},
};
use rusty_codecs::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};
use tracing::{debug, warn};

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

/// Opens a camera briefly to query its supported formats, then releases it.
fn enumerate_formats(index: &CameraIndex) -> Result<Vec<CameraFormat>> {
    let format = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut cam =
        Camera::new(index.clone(), format).context("nokhwa: failed to open camera for query")?;
    let nokhwa_formats = cam
        .compatible_camera_formats()
        .context("nokhwa: failed to list compatible formats")?;

    // Explicitly stop any stream nokhwa may have opened during enumeration,
    // then drop the Camera so the device is fully released before anyone
    // else tries to open it (CAP14/CAP15).
    let _ = cam.stop_stream();
    drop(cam);

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

/// Camera capturer backed by [`nokhwa`].
///
/// No internal thread — `pop_frame()` calls `camera.frame()` directly on
/// the caller's thread. Requires the `camera-sync-impl` nokhwa feature.
pub struct NokhwaCameraCapturer {
    camera_name: String,
    dimensions: [u32; 2],
    camera: Camera,
    streaming: bool,
    start_time: Instant,
}

impl std::fmt::Debug for NokhwaCameraCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NokhwaCameraCapturer")
            .field("camera_name", &self.camera_name)
            .field("dimensions", &self.dimensions)
            .field("streaming", &self.streaming)
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

        let camera = Camera::new(index, req_format).context("nokhwa: failed to open camera")?;

        let cf = camera.camera_format();
        let dimensions = [cf.resolution().width(), cf.resolution().height()];
        let fps = cf.frame_rate();
        let camera_name = info.name.clone();

        debug!(
            name = %camera_name,
            width = dimensions[0],
            height = dimensions[1],
            fps,
            "nokhwa camera capturer created"
        );

        Ok(Self {
            camera_name,
            dimensions,
            camera,
            streaming: false,
            start_time: Instant::now(),
        })
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
        if !self.streaming {
            self.camera
                .open_stream()
                .map_err(|e| anyhow::anyhow!("nokhwa: open_stream failed: {e}"))?;
            self.streaming = true;
            self.start_time = Instant::now();
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.streaming {
            self.camera
                .stop_stream()
                .map_err(|e| anyhow::anyhow!("nokhwa: stop_stream failed: {e}"))?;
            self.streaming = false;
        }
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.streaming {
            return Ok(None);
        }

        let buffer = match self.camera.frame() {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "nokhwa: frame capture failed");
                return Ok(None);
            }
        };

        let timestamp = self.start_time.elapsed();
        let res = buffer.resolution();
        let w = res.width();
        let h = res.height();

        let rgba_image = match buffer.decode_image::<RgbAFormat>() {
            Ok(img) => img,
            Err(e) => {
                warn!(error = %e, "nokhwa: frame decode failed");
                return Ok(None);
            }
        };

        let data: bytes::Bytes = rgba_image.into_raw().into();
        Ok(Some(VideoFrame::new_rgba(data, w, h, timestamp)))
    }
}

impl Drop for NokhwaCameraCapturer {
    fn drop(&mut self) {
        if self.streaming {
            let _ = self.camera.stop_stream();
        }
    }
}
