// Platform backend modules re-export their public items at the crate root;
// the modules themselves are pub(crate) to keep them internal.
#![allow(
    unreachable_pub,
    reason = "pub items inside pub(crate) modules are re-exported at crate root"
)]
//! Cross-platform screen and camera capture with zero-copy GPU buffer support.
//!
//! Provides [`ScreenCapturer`] and [`CameraCapturer`] that implement
//! [`rusty_codecs::traits::VideoSource`], producing [`VideoFrame`](rusty_codecs::format::VideoFrame)
//! values ready for encoding or rendering.
//!
//! # Platform Backends
//!
//! | Platform | Screen | Camera | Zero-copy |
//! |----------|--------|--------|-----------|
//! | Linux (PipeWire) | `pipewire` feature | `pipewire` feature | DMA-BUF (NV12) |
//! | Linux (X11) | `x11` feature | — | No (MIT-SHM CPU) |
//! | Linux (V4L2) | — | `v4l2` feature | DMA-BUF via EXPBUF |
//! | macOS | `screen-apple` feature | `camera-apple` feature | IOSurface (planned) |
//! | iOS | — | `camera-apple` feature | IOSurface (planned) |
//!
//! # Feature Flags
//!
//! High-level capability features (recommended):
//! - **`camera`** — Camera capture on all platforms.
//! - **`screen`** — Screen capture on all platforms.
//!
//! Platform bundles:
//! - **`camera-linux`** — `pipewire` + `v4l2`.
//! - **`camera-apple`** — AVFoundation camera on macOS/iOS.
//! - **`screen-linux`** — `pipewire` (x11 not included by default).
//! - **`screen-apple`** — ScreenCaptureKit screen capture on macOS 12.3+.
//!
//! Low-level backend features:
//! - **`pipewire`** — PipeWire screen + camera capture on Linux (Wayland + libcamera).
//!   Requires `libpipewire-0.3-dev` at build time.
//! - **`v4l2`** — V4L2 camera capture on Linux.
//! - **`x11`** — X11 screen capture via MIT-SHM. Not included in defaults.

pub mod types;

#[allow(
    clippy::mod_module_files,
    reason = "platform/ tree uses mod.rs for nested backends"
)]
mod platform;

// Re-export core types from rusty-codecs that capture backends produce.
pub use rusty_codecs::format::{PixelFormat, VideoFormat, VideoFrame};
pub use rusty_codecs::traits::VideoSource;

pub use types::*;

// Re-export platform-specific capturers at the crate root.

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub use platform::linux::pipewire::{PipeWireCameraCapturer, PipeWireScreenCapturer};

#[cfg(all(target_os = "linux", feature = "v4l2"))]
pub use platform::linux::v4l2::V4l2CameraCapturer;

#[cfg(all(target_os = "linux", feature = "x11"))]
pub use platform::linux::x11::X11ScreenCapturer;

#[cfg(all(target_os = "macos", feature = "screen-apple"))]
pub use platform::apple::screen::MacScreenCapturer;

#[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
pub use platform::apple::camera::AppleCameraCapturer;

// ── PipeWire runtime detection ──────────────────────────────────────

#[cfg(all(target_os = "linux", feature = "pipewire"))]
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

#[cfg(all(target_os = "linux", feature = "pipewire"))]
fn pipewire_monitor_placeholder() -> MonitorInfo {
    MonitorInfo {
        id: "pipewire-portal".into(),
        name: "PipeWire Screen Share".into(),
        position: [0, 0],
        dimensions: [0, 0],
        scale_factor: 1.0,
        refresh_rate_hz: None,
        is_primary: true,
    }
}

#[cfg(all(target_os = "linux", feature = "pipewire"))]
fn pipewire_camera_placeholder() -> CameraInfo {
    CameraInfo {
        id: "pipewire-portal".into(),
        name: "PipeWire Camera".into(),
        supported_formats: vec![],
    }
}

// ── Backend dispatch ────────────────────────────────────────────────

#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn list_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        return Ok(vec![pipewire_monitor_placeholder()]);
    }
    #[cfg(all(target_os = "linux", feature = "x11"))]
    {
        return platform::linux::x11::monitors();
    }
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    {
        return platform::apple::screen::monitors();
    }
    anyhow::bail!(
        "no screen capture backend available (enable `screen` or a platform-specific feature)"
    )
}

#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn list_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        return Ok(vec![pipewire_camera_placeholder()]);
    }
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    {
        return platform::linux::v4l2::cameras();
    }
    #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
    {
        return platform::apple::camera::cameras();
    }
    anyhow::bail!(
        "no camera capture backend available (enable `camera` or a platform-specific feature)"
    )
}

#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn create_camera_backend(
    info: &CameraInfo,
    config: &CameraConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if info.id == "pipewire-portal" {
        return Ok(Box::new(PipeWireCameraCapturer::new()?));
    }
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    {
        return Ok(Box::new(V4l2CameraCapturer::new(info, config)?));
    }
    #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
    {
        return Ok(Box::new(AppleCameraCapturer::new(info, config)?));
    }
    let _ = (info, config);
    anyhow::bail!("no camera capture backend available on this platform")
}

#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn create_screen_backend(
    monitor: Option<&MonitorInfo>,
    config: &ScreenConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        return Ok(Box::new(PipeWireScreenCapturer::new(config)?));
    }
    #[cfg(all(target_os = "linux", feature = "x11"))]
    {
        let mon = match monitor {
            Some(m) => m.clone(),
            None => list_monitors()?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no monitors available"))?,
        };
        return Ok(Box::new(X11ScreenCapturer::new(&mon, config)?));
    }
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    {
        let mon = match monitor {
            Some(m) => m.clone(),
            None => list_monitors()?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no monitors available"))?,
        };
        return Ok(Box::new(MacScreenCapturer::new(&mon, config)?));
    }
    let _ = (monitor, config);
    anyhow::bail!("no screen capture backend available on this platform")
}

// ── High-level capturers ────────────────────────────────────────────

/// Camera capturer that auto-selects the best available backend.
///
/// Implements [`VideoSource`]. On Linux, prefers PipeWire (if running) over
/// V4L2. On macOS/iOS, uses AVFoundation.
#[derive(derive_more::Debug)]
pub struct CameraCapturer {
    #[debug(skip)]
    inner: Box<dyn VideoSource>,
}

impl CameraCapturer {
    /// Lists available cameras.
    pub fn list() -> anyhow::Result<Vec<CameraInfo>> {
        list_cameras()
    }

    /// Opens the default camera.
    ///
    /// Picks the last camera in the enumerated list, which on most systems is
    /// the primary built-in camera.
    pub fn new() -> anyhow::Result<Self> {
        let cams = list_cameras()?;
        if cams.is_empty() {
            anyhow::bail!("no cameras available");
        }
        let cam = cams.last().unwrap();
        tracing::debug!(name = %cam.name, "selected camera");
        Self::open(cam)
    }

    /// Opens a camera by numeric index (from [`CameraCapturer::list()`]).
    pub fn with_index(index: u32) -> anyhow::Result<Self> {
        let cams = list_cameras()?;
        let cam = cams
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("camera index {index} out of range"))?;
        Self::open(cam)
    }

    fn open(info: &CameraInfo) -> anyhow::Result<Self> {
        let config = CameraConfig {
            preferred_resolution: Some([1920, 1080]),
            ..Default::default()
        };
        let inner = create_camera_backend(info, &config)?;
        Ok(Self { inner })
    }
}

impl VideoSource for CameraCapturer {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn format(&self) -> VideoFormat {
        self.inner.format()
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.inner.start()
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        self.inner.stop()
    }
    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        self.inner.pop_frame()
    }
}

/// Screen capturer that auto-selects the best available backend.
///
/// Implements [`VideoSource`]. On Linux, prefers PipeWire (if running) over
/// X11. On macOS, uses ScreenCaptureKit.
#[derive(derive_more::Debug)]
pub struct ScreenCapturer {
    #[debug(skip)]
    inner: Box<dyn VideoSource>,
}

impl ScreenCapturer {
    /// Lists available monitors for screen capture.
    pub fn list() -> anyhow::Result<Vec<MonitorInfo>> {
        list_monitors()
    }

    /// Opens the default (primary) monitor for screen capture.
    pub fn new() -> anyhow::Result<Self> {
        let config = ScreenConfig::default();
        let inner = create_screen_backend(None, &config)?;
        Ok(Self { inner })
    }

    /// Opens a specific monitor for screen capture.
    pub fn with_monitor(monitor: &MonitorInfo) -> anyhow::Result<Self> {
        let config = ScreenConfig::default();
        let inner = create_screen_backend(Some(monitor), &config)?;
        Ok(Self { inner })
    }
}

impl VideoSource for ScreenCapturer {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn format(&self) -> VideoFormat {
        self.inner.format()
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.inner.start()
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        self.inner.stop()
    }
    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        self.inner.pop_frame()
    }
}
