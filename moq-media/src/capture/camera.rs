use anyhow::Result;
use tracing::debug;

use crate::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};

/// Information about an available camera.
#[derive(Debug, Clone)]
pub struct CameraInfo {
    /// Numeric index of this camera.
    pub index: u32,
    /// Human-readable name of this camera.
    pub name: String,
}

/// List all available cameras.
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    let cameras = rusty_capture::cameras()?;
    Ok(cameras
        .into_iter()
        .enumerate()
        .map(|(i, info)| CameraInfo {
            index: i as u32,
            name: info.name,
        })
        .collect())
}

/// Camera capturer backed by rusty-capture.
#[derive(derive_more::Debug)]
pub struct CameraCapturer {
    #[debug(skip)]
    inner: Box<dyn VideoSource>,
    width: u32,
    height: u32,
}

impl CameraCapturer {
    /// Creates a new camera capturer using the default camera.
    pub fn new() -> Result<Self> {
        debug!("initializing camera capturer (rusty-capture)");
        let cameras = rusty_capture::cameras()?;
        if cameras.is_empty() {
            anyhow::bail!("No cameras available");
        }
        // Pick the last camera (primary is often last, matching old nokhwa behavior).
        let camera = cameras.last().unwrap();
        debug!(name = %camera.name, "selected camera");
        Self::open(camera)
    }

    /// Creates a new camera capturer for a specific camera index.
    pub fn with_index(index: u32) -> Result<Self> {
        debug!("initializing camera capturer (rusty-capture) with index {index}");
        let cameras = rusty_capture::cameras()?;
        let camera = cameras
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("Camera index {index} out of range"))?;
        Self::open(camera)
    }

    fn open(info: &rusty_capture::CameraInfo) -> Result<Self> {
        let config = rusty_capture::CameraConfig {
            preferred_resolution: Some([1920, 1080]),
            ..Default::default()
        };
        let inner = Self::create_backend(info, &config)?;
        let fmt = inner.format();
        let [w, h] = fmt.dimensions;
        Ok(Self {
            inner,
            width: w,
            height: h,
        })
    }

    #[cfg(all(target_os = "linux", feature = "capture-camera"))]
    fn create_backend(
        info: &rusty_capture::CameraInfo,
        config: &rusty_capture::CameraConfig,
    ) -> Result<Box<dyn VideoSource>> {
        // Try PipeWire first, fall back to V4L2.
        if info.id == "pipewire-portal" {
            let capturer = rusty_capture::PipeWireCameraCapturer::new()?;
            return Ok(Box::new(capturer));
        }
        let capturer = rusty_capture::V4l2CameraCapturer::new(info, config)?;
        Ok(Box::new(capturer))
    }

    #[cfg(all(
        any(target_os = "macos", target_os = "ios"),
        feature = "capture-camera"
    ))]
    fn create_backend(
        info: &rusty_capture::CameraInfo,
        config: &rusty_capture::CameraConfig,
    ) -> Result<Box<dyn VideoSource>> {
        let capturer = rusty_capture::AppleCameraCapturer::new(info, config)?;
        Ok(Box::new(capturer))
    }

    #[cfg(not(any(
        all(target_os = "linux", feature = "capture-camera"),
        all(
            any(target_os = "macos", target_os = "ios"),
            feature = "capture-camera"
        ),
    )))]
    fn create_backend(
        _info: &rusty_capture::CameraInfo,
        _config: &rusty_capture::CameraConfig,
    ) -> Result<Box<dyn VideoSource>> {
        anyhow::bail!("No camera capture backend available on this platform")
    }
}

impl VideoSource for CameraCapturer {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> Result<()> {
        self.inner.stop()
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        self.inner.pop_frame()
    }
}
