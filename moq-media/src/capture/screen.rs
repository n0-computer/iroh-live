use anyhow::Result;
use tracing::debug;

use crate::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};

/// Screen capturer backed by rusty-capture.
#[derive(derive_more::Debug)]
pub struct ScreenCapturer {
    #[debug(skip)]
    inner: Box<dyn VideoSource>,
    width: u32,
    height: u32,
}

impl ScreenCapturer {
    /// Creates a new screen capturer using the default monitor.
    pub fn new() -> Result<Self> {
        debug!("initializing screen capturer (rusty-capture)");
        let config = rusty_capture::ScreenConfig::default();
        let inner = Self::create_backend(&config)?;
        let fmt = inner.format();
        let [w, h] = fmt.dimensions;
        Ok(Self {
            inner,
            width: w,
            height: h,
        })
    }

    #[cfg(all(target_os = "linux", feature = "capture-screen"))]
    fn create_backend(config: &rusty_capture::ScreenConfig) -> Result<Box<dyn VideoSource>> {
        // Try PipeWire first, fall back to X11.
        if pipewire_available() {
            let capturer = rusty_capture::PipeWireScreenCapturer::new(config)?;
            return Ok(Box::new(capturer));
        }
        let monitors = rusty_capture::monitors()?;
        let monitor = monitors
            .first()
            .ok_or_else(|| anyhow::anyhow!("No monitors available"))?;
        let capturer = rusty_capture::X11ScreenCapturer::new(monitor, config)?;
        Ok(Box::new(capturer))
    }

    #[cfg(all(target_os = "macos", feature = "capture-screen"))]
    fn create_backend(config: &rusty_capture::ScreenConfig) -> Result<Box<dyn VideoSource>> {
        let monitors = rusty_capture::monitors()?;
        let monitor = monitors
            .first()
            .ok_or_else(|| anyhow::anyhow!("No monitors available"))?;
        let capturer = rusty_capture::MacScreenCapturer::new(monitor, config)?;
        Ok(Box::new(capturer))
    }

    #[cfg(not(any(
        all(target_os = "linux", feature = "capture-screen"),
        all(target_os = "macos", feature = "capture-screen"),
    )))]
    fn create_backend(_config: &rusty_capture::ScreenConfig) -> Result<Box<dyn VideoSource>> {
        anyhow::bail!("No screen capture backend available on this platform")
    }
}

#[cfg(all(target_os = "linux", feature = "capture-screen"))]
fn pipewire_available() -> bool {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let socket = std::path::Path::new(&runtime_dir).join("pipewire-0");
        if socket.exists() {
            return true;
        }
    }
    std::process::Command::new("pidof")
        .arg("pipewire")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

impl VideoSource for ScreenCapturer {
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
