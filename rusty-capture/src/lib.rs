// Platform backend modules re-export their public items at the crate root;
// the modules themselves are pub(crate) to keep them internal.
#![allow(
    unreachable_pub,
    reason = "pub items inside pub(crate) modules are re-exported at crate root"
)]
#![allow(
    clippy::mod_module_files,
    reason = "platform/ tree uses mod.rs for clarity"
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
//! | Windows | — (stub) | — (stub) | D3D11 (planned) |
//! | Android | — (stub) | — (stub) | AHardwareBuffer (planned) |
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
//! - **`screen-linux`** — `pipewire` + `x11`.
//! - **`screen-apple`** — ScreenCaptureKit screen capture on macOS 12.3+.
//!
//! Low-level backend features:
//! - **`pipewire`** — PipeWire screen + camera capture on Linux (Wayland + libcamera).
//!   Requires `libpipewire-0.3-dev` at build time.
//! - **`v4l2`** — V4L2 camera capture on Linux.
//! - **`x11`** — X11 screen capture via MIT-SHM.

pub mod types;

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

// ── Monitor listing ─────────────────────────────────────────────────

/// Lists available monitors for screen capture.
///
/// Returns monitors from the best available backend for the current platform.
/// When multiple backends are enabled on Linux, prefers PipeWire (if the
/// PipeWire daemon is running) over X11.
pub fn monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    cfg_dispatch_monitors()
}

#[cfg(all(target_os = "linux", feature = "pipewire", feature = "x11"))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    if pipewire_available() {
        Ok(vec![pipewire_monitor_placeholder()])
    } else {
        platform::linux::x11::monitors()
    }
}

#[cfg(all(target_os = "linux", feature = "pipewire", not(feature = "x11")))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    Ok(vec![pipewire_monitor_placeholder()])
}

#[cfg(all(target_os = "linux", feature = "x11", not(feature = "pipewire")))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    platform::linux::x11::monitors()
}

#[cfg(all(target_os = "macos", feature = "screen-apple"))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    platform::apple::screen::monitors()
}

#[cfg(not(any(
    all(target_os = "linux", feature = "x11"),
    all(target_os = "linux", feature = "pipewire"),
    all(target_os = "macos", feature = "screen-apple"),
)))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    anyhow::bail!(
        "no screen capture backend available (enable `screen` or a platform-specific feature)"
    )
}

// ── Camera listing ──────────────────────────────────────────────────

/// Lists available cameras for capture.
///
/// Returns cameras from the best available backend for the current platform.
/// When multiple backends are enabled on Linux, prefers PipeWire (if the
/// PipeWire daemon is running) over V4L2.
pub fn cameras() -> anyhow::Result<Vec<CameraInfo>> {
    cfg_dispatch_cameras()
}

#[cfg(all(target_os = "linux", feature = "pipewire", feature = "v4l2"))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    if pipewire_available() {
        Ok(vec![pipewire_camera_placeholder()])
    } else {
        platform::linux::v4l2::cameras()
    }
}

#[cfg(all(target_os = "linux", feature = "pipewire", not(feature = "v4l2")))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    Ok(vec![pipewire_camera_placeholder()])
}

#[cfg(all(target_os = "linux", feature = "v4l2", not(feature = "pipewire")))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    platform::linux::v4l2::cameras()
}

#[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple",))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    platform::apple::camera::cameras()
}

#[cfg(not(any(
    all(target_os = "linux", feature = "v4l2"),
    all(target_os = "linux", feature = "pipewire"),
    all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"),
)))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    anyhow::bail!(
        "no camera capture backend available (enable `camera` or a platform-specific feature)"
    )
}

// ── PipeWire runtime detection ──────────────────────────────────────

#[cfg(all(target_os = "linux", feature = "pipewire"))]
fn pipewire_available() -> bool {
    // Check if the PipeWire daemon is running by looking for its socket.
    // The runtime directory is typically $XDG_RUNTIME_DIR/pipewire-0.
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let socket = std::path::Path::new(&runtime_dir).join("pipewire-0");
        if socket.exists() {
            return true;
        }
    }
    // Fallback: check if `pipewire` process is running.
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

// ── High-level capturers ────────────────────────────────────────────

/// Simplified camera device listing with numeric indices.
#[derive(Debug, Clone)]
pub struct SimpleCameraInfo {
    /// Numeric index of this camera.
    pub index: u32,
    /// Human-readable device name.
    pub name: String,
}

/// Lists available cameras with simple numeric indices.
///
/// Wraps [`cameras()`] into a flat list suitable for UI selection.
pub fn list_cameras() -> anyhow::Result<Vec<SimpleCameraInfo>> {
    let cams = cameras()?;
    Ok(cams
        .into_iter()
        .enumerate()
        .map(|(i, info)| SimpleCameraInfo {
            index: i as u32,
            name: info.name,
        })
        .collect())
}

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
    /// Opens the default camera.
    ///
    /// Picks the last camera in the enumerated list, which on most systems is
    /// the primary built-in camera.
    pub fn new() -> anyhow::Result<Self> {
        let cams = cameras()?;
        if cams.is_empty() {
            anyhow::bail!("no cameras available");
        }
        let cam = cams.last().unwrap();
        tracing::debug!(name = %cam.name, "selected camera");
        Self::open(cam)
    }

    /// Opens a camera by numeric index (from [`list_cameras()`]).
    pub fn with_index(index: u32) -> anyhow::Result<Self> {
        let cams = cameras()?;
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
    /// Opens the default monitor for screen capture.
    pub fn new() -> anyhow::Result<Self> {
        let config = ScreenConfig::default();
        let inner = create_screen_backend(&config)?;
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

// ── Backend dispatch for high-level capturers ───────────────────────

#[cfg(all(target_os = "linux", any(feature = "pipewire", feature = "v4l2")))]
fn create_camera_backend(
    info: &CameraInfo,
    config: &CameraConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    #[cfg(feature = "pipewire")]
    if info.id == "pipewire-portal" {
        let capturer = PipeWireCameraCapturer::new()?;
        return Ok(Box::new(capturer));
    }
    #[cfg(feature = "v4l2")]
    {
        let capturer = V4l2CameraCapturer::new(info, config)?;
        Ok(Box::new(capturer))
    }
    #[cfg(not(feature = "v4l2"))]
    {
        let _ = (info, config);
        anyhow::bail!("no camera backend available (PipeWire camera not selected)")
    }
}

#[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple",))]
fn create_camera_backend(
    info: &CameraInfo,
    config: &CameraConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    let capturer = AppleCameraCapturer::new(info, config)?;
    Ok(Box::new(capturer))
}

#[cfg(not(any(
    all(target_os = "linux", any(feature = "pipewire", feature = "v4l2")),
    all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"),
)))]
fn create_camera_backend(
    _info: &CameraInfo,
    _config: &CameraConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    anyhow::bail!("no camera capture backend available on this platform")
}

#[cfg(all(target_os = "linux", any(feature = "pipewire", feature = "x11")))]
fn create_screen_backend(config: &ScreenConfig) -> anyhow::Result<Box<dyn VideoSource>> {
    #[cfg(feature = "pipewire")]
    if pipewire_available() {
        let capturer = PipeWireScreenCapturer::new(config)?;
        return Ok(Box::new(capturer));
    }
    #[cfg(feature = "x11")]
    {
        let mons = monitors()?;
        let mon = mons
            .first()
            .ok_or_else(|| anyhow::anyhow!("no monitors available"))?;
        let capturer = X11ScreenCapturer::new(mon, config)?;
        Ok(Box::new(capturer))
    }
    #[cfg(not(feature = "x11"))]
    {
        let _ = config;
        anyhow::bail!("no screen backend available (PipeWire not running and X11 disabled)")
    }
}

#[cfg(all(target_os = "macos", feature = "screen-apple"))]
fn create_screen_backend(config: &ScreenConfig) -> anyhow::Result<Box<dyn VideoSource>> {
    let mons = monitors()?;
    let mon = mons
        .first()
        .ok_or_else(|| anyhow::anyhow!("no monitors available"))?;
    let capturer = MacScreenCapturer::new(mon, config)?;
    Ok(Box::new(capturer))
}

#[cfg(not(any(
    all(target_os = "linux", any(feature = "pipewire", feature = "x11")),
    all(target_os = "macos", feature = "screen-apple"),
)))]
fn create_screen_backend(_config: &ScreenConfig) -> anyhow::Result<Box<dyn VideoSource>> {
    anyhow::bail!("no screen capture backend available on this platform")
}
