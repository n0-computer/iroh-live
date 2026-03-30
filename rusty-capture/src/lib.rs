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
//! | Linux (libcamera) | — | `libcamera` feature | No (subprocess pipe) |
//! | Cross-platform | `xcap` feature | `nokhwa` feature | No (CPU only) |
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
//! Cross-platform backends (default, CPU-only fallback):
//! - **`xcap`** — Screen capture via the `xcap` crate. Linux (X11/Wayland), macOS, Windows.
//! - **`nokhwa`** — Camera capture via the `nokhwa` crate. Linux, macOS, Windows.
//!
//! Low-level backend features:
//! - **`pipewire`** — PipeWire screen + camera capture on Linux (Wayland + libcamera).
//!   Requires `libpipewire-0.3-dev` at build time.
//! - **`v4l2`** — V4L2 camera capture on Linux.
//! - **`libcamera`** — Raspberry Pi CSI camera via rpicam-vid subprocess.
//!   No build-time dependencies; requires `rpicam-vid` at runtime.
//! - **`x11`** — X11 screen capture via MIT-SHM. Not included in defaults.

pub mod types;

#[allow(
    clippy::mod_module_files,
    reason = "platform/ tree uses mod.rs for nested backends"
)]
mod platform;

// Re-export core types from rusty-codecs that capture backends produce.
#[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
pub use platform::apple::camera::AppleCameraCapturer;
#[cfg(all(target_os = "macos", feature = "screen-apple"))]
pub use platform::apple::screen::MacScreenCapturer;
// Re-export platform-specific capturers at the crate root.
#[cfg(all(target_os = "linux", feature = "libcamera"))]
pub use platform::linux::libcamera::{LibcameraCapturer, LibcameraConfig};
#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub use platform::linux::pipewire::{PipeWireCameraCapturer, PipeWireScreenCapturer};
#[cfg(all(target_os = "linux", feature = "v4l2"))]
pub use platform::linux::v4l2::V4l2CameraCapturer;
#[cfg(all(target_os = "linux", feature = "x11"))]
pub use platform::linux::x11::X11ScreenCapturer;
#[cfg(feature = "nokhwa")]
pub use platform::nokhwa_impl::NokhwaCameraCapturer;
#[cfg(feature = "xcap")]
pub use platform::xcap_impl::XcapScreenCapturer;
pub use rusty_codecs::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};
pub use types::*;

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
        backend: CaptureBackend::PipeWire,
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
        backend: CaptureBackend::PipeWire,
        id: "pipewire-portal".into(),
        name: "PipeWire Camera".into(),
        supported_formats: vec![],
    }
}

// ── Backend dispatch ────────────────────────────────────────────────

/// Lists monitors from the preferred backend (priority cascade).
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
    #[cfg(feature = "xcap")]
    {
        return platform::xcap_impl::monitors();
    }
    anyhow::bail!(
        "no screen capture backend available (enable `screen` or a platform-specific feature)"
    )
}

/// Lists monitors from all compiled-in backends.
#[allow(unused_mut, reason = "push count depends on compiled backends")]
fn list_all_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    let mut all = Vec::new();
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        all.push(pipewire_monitor_placeholder());
    }
    #[cfg(all(target_os = "linux", feature = "x11"))]
    if let Ok(monitors) = platform::linux::x11::monitors() {
        all.extend(monitors);
    }
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    if let Ok(monitors) = platform::apple::screen::monitors() {
        all.extend(monitors);
    }
    #[cfg(feature = "xcap")]
    if let Ok(monitors) = platform::xcap_impl::monitors() {
        all.extend(monitors);
    }
    Ok(all)
}

/// Lists cameras from the preferred backend (priority cascade).
#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn list_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    #[cfg(all(target_os = "linux", feature = "libcamera"))]
    {
        let cams = platform::linux::libcamera::cameras()?;
        if !cams.is_empty() {
            return Ok(cams);
        }
    }
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    {
        return platform::linux::v4l2::cameras();
    }
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        return Ok(vec![pipewire_camera_placeholder()]);
    }
    #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
    {
        return platform::apple::camera::cameras();
    }
    #[cfg(feature = "nokhwa")]
    {
        return platform::nokhwa_impl::cameras();
    }
    anyhow::bail!(
        "no camera capture backend available (enable `camera` or a platform-specific feature)"
    )
}

/// Lists cameras from all compiled-in backends.
#[allow(unused_mut, reason = "push count depends on compiled backends")]
fn list_all_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    let mut all = Vec::new();
    #[cfg(all(target_os = "linux", feature = "libcamera"))]
    if let Ok(cameras) = platform::linux::libcamera::cameras() {
        all.extend(cameras);
    }
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    if let Ok(cameras) = platform::linux::v4l2::cameras() {
        all.extend(cameras);
    }
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        all.push(pipewire_camera_placeholder());
    }
    #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
    if let Ok(cameras) = platform::apple::camera::cameras() {
        all.extend(cameras);
    }
    #[cfg(feature = "nokhwa")]
    if let Ok(cameras) = platform::nokhwa_impl::cameras() {
        all.extend(cameras);
    }
    Ok(all)
}

#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn create_camera_backend(
    info: &CameraInfo,
    config: &CameraConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    match info.backend {
        #[cfg(all(target_os = "linux", feature = "libcamera"))]
        CaptureBackend::Libcamera => {
            let lc_config = platform::linux::libcamera::LibcameraConfig {
                width: match &config.selector {
                    CameraSelector::TargetResolution(w, _) => *w,
                    _ => 640,
                },
                height: match &config.selector {
                    CameraSelector::TargetResolution(_, h) => *h,
                    _ => 360,
                },
                framerate: 30,
            };
            Ok(Box::new(LibcameraCapturer::new(lc_config)))
        }
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        CaptureBackend::PipeWire => Ok(Box::new(PipeWireCameraCapturer::new(config)?)),
        #[cfg(all(target_os = "linux", feature = "v4l2"))]
        CaptureBackend::V4l2 => Ok(Box::new(V4l2CameraCapturer::new(info, config)?)),
        #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
        CaptureBackend::AVFoundation => Ok(Box::new(AppleCameraCapturer::new(info, config)?)),
        #[cfg(feature = "nokhwa")]
        CaptureBackend::Nokhwa => Ok(Box::new(NokhwaCameraCapturer::new(info, config)?)),
        _ => {
            let _ = (info, config);
            anyhow::bail!(
                "camera backend {} is not available on this platform",
                info.backend
            )
        }
    }
}

/// Creates a screen backend for a specific monitor.
///
/// When `monitor` is provided, dispatches based on `monitor.backend`.
/// When `None`, uses the preferred-backend cascade (same as `list_monitors`).
#[allow(
    unreachable_code,
    reason = "cfg-gated returns may make trailing code unreachable"
)]
fn create_screen_backend(
    monitor: Option<&MonitorInfo>,
    config: &ScreenConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    // When a specific monitor is given, dispatch on its backend directly.
    if let Some(mon) = monitor {
        return create_screen_for_backend(mon, config);
    }

    // No monitor specified — use preferred-backend cascade.
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    if pipewire_available() {
        return Ok(Box::new(PipeWireScreenCapturer::new(config)?));
    }
    #[cfg(all(target_os = "linux", feature = "x11"))]
    {
        let mon = list_monitors()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no monitors available"))?;
        return Ok(Box::new(X11ScreenCapturer::new(&mon, config)?));
    }
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    {
        let mon = list_monitors()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no monitors available"))?;
        return Ok(Box::new(MacScreenCapturer::new(&mon, config)?));
    }
    #[cfg(feature = "xcap")]
    {
        let mon = list_monitors()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no monitors available"))?;
        return Ok(Box::new(XcapScreenCapturer::new(&mon, config)?));
    }
    let _ = config;
    anyhow::bail!("no screen capture backend available on this platform")
}

/// Dispatches to the correct screen backend based on the monitor's backend field.
fn create_screen_for_backend(
    monitor: &MonitorInfo,
    config: &ScreenConfig,
) -> anyhow::Result<Box<dyn VideoSource>> {
    match monitor.backend {
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        CaptureBackend::PipeWire => Ok(Box::new(PipeWireScreenCapturer::new(config)?)),
        #[cfg(all(target_os = "linux", feature = "x11"))]
        CaptureBackend::X11 => Ok(Box::new(X11ScreenCapturer::new(monitor, config)?)),
        #[cfg(all(target_os = "macos", feature = "screen-apple"))]
        CaptureBackend::ScreenCaptureKit => Ok(Box::new(MacScreenCapturer::new(monitor, config)?)),
        #[cfg(feature = "xcap")]
        CaptureBackend::Xcap => Ok(Box::new(XcapScreenCapturer::new(monitor, config)?)),
        _ => anyhow::bail!(
            "screen backend {} is not available on this platform",
            monitor.backend
        ),
    }
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
    /// Returns the capture backends that are compiled in and available at runtime
    /// for camera capture.
    #[allow(
        unused_mut,
        clippy::vec_init_then_push,
        reason = "mut and push count depend on which platform backends are compiled in"
    )]
    pub fn list_backends() -> Vec<CaptureBackend> {
        // Order must match `list_all_cameras()` so that backend indexes from
        // `irl devices` resolve correctly via `BackendRef::Index`.
        let mut backends = Vec::new();
        #[cfg(all(target_os = "linux", feature = "libcamera"))]
        backends.push(CaptureBackend::Libcamera);
        #[cfg(all(target_os = "linux", feature = "v4l2"))]
        backends.push(CaptureBackend::V4l2);
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        if pipewire_available() {
            backends.push(CaptureBackend::PipeWire);
        }
        #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "camera-apple"))]
        backends.push(CaptureBackend::AVFoundation);
        #[cfg(feature = "nokhwa")]
        backends.push(CaptureBackend::Nokhwa);
        backends
    }

    /// Lists cameras from the default (preferred) backend.
    pub fn list() -> anyhow::Result<Vec<CameraInfo>> {
        list_cameras()
    }

    /// Lists cameras from all compiled-in backends.
    pub fn list_all() -> anyhow::Result<Vec<CameraInfo>> {
        list_all_cameras()
    }

    /// Opens the default camera with [`CameraSelector::HighestResolution`].
    ///
    /// Picks the first camera in the enumerated list, which on most systems is
    /// the primary built-in camera.
    pub fn new() -> anyhow::Result<Self> {
        Self::with_config(None, &CameraConfig::default())
    }

    /// Opens the default camera with a custom selector strategy.
    pub fn with_selector(selector: CameraSelector) -> anyhow::Result<Self> {
        Self::with_config(
            None,
            &CameraConfig {
                selector,
                ..Default::default()
            },
        )
    }

    /// Opens a camera by numeric index (from [`CameraCapturer::list()`]).
    pub fn with_index(index: u32) -> anyhow::Result<Self> {
        let cams = list_cameras()?;
        let cam = cams
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("camera index {index} out of range"))?;
        Self::with_config(Some(cam), &CameraConfig::default())
    }

    /// Opens a camera with a specific format from [`CameraInfo::supported_formats`].
    pub fn with_format(info: &CameraInfo, format: &CameraFormat) -> anyhow::Result<Self> {
        let config = CameraConfig {
            selector: CameraSelector::TargetResolution(format.dimensions[0], format.dimensions[1]),
            preferred_format: Some(format.pixel_format),
            ..Default::default()
        };
        let inner = create_camera_backend(info, &config)?;
        Ok(Self { inner })
    }

    /// Opens a camera matching an optional backend and/or device ID.
    ///
    /// When only `backend` is given, selects the first camera on that backend.
    /// When only `id` is given, searches all cameras by platform ID or name.
    /// When both are given, requires an exact match.
    /// When neither is given, delegates to [`CameraCapturer::new()`].
    ///
    /// On failure, the error message lists available cameras.
    pub fn open(
        backend: Option<CaptureBackend>,
        id: Option<&str>,
        config: &CameraConfig,
    ) -> anyhow::Result<Self> {
        match (backend, id) {
            (None, None) => Self::new(),
            (Some(b), None) => Self::with_backend(b, config),
            (None, Some(id)) => {
                let cameras = list_cameras()?;
                let info = cameras
                    .iter()
                    .find(|c| c.id == id || c.name == id)
                    .ok_or_else(|| format_not_found("camera", id, &cameras))?;
                Self::with_config(Some(info), config)
            }
            (Some(b), Some(id)) => {
                let cameras = list_cameras()?;
                let info = cameras
                    .iter()
                    .find(|c| c.backend == b && (c.id == id || c.name == id))
                    .ok_or_else(|| format_not_found("camera", id, &cameras))?;
                Self::with_config(Some(info), config)
            }
        }
    }

    /// Opens a camera using a specific backend with default config.
    ///
    /// Returns an error if the backend is not available or does not support camera capture.
    pub fn with_backend(backend: CaptureBackend, config: &CameraConfig) -> anyhow::Result<Self> {
        let cameras = list_cameras()?;
        match cameras.iter().find(|c| c.backend == backend) {
            Some(info) => {
                let inner = create_camera_backend(info, config)?;
                Ok(Self { inner })
            }
            None => anyhow::bail!("camera backend {backend} is not available"),
        }
    }

    /// Opens a camera with full configuration control.
    ///
    /// When `info` is `None`, selects the first camera from the enumerated list.
    pub fn with_config(info: Option<&CameraInfo>, config: &CameraConfig) -> anyhow::Result<Self> {
        let owned;
        let info = match info {
            Some(i) => i,
            None => {
                let cams = list_cameras()?;
                if cams.is_empty() {
                    anyhow::bail!("no cameras available");
                }
                owned = cams.into_iter().next().unwrap();
                &owned
            }
        };
        tracing::debug!(name = %info.name, ?config.selector, "opening camera");
        let inner = create_camera_backend(info, config)?;
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
    /// Returns the capture backends that are compiled in and available at runtime
    /// for screen capture.
    #[allow(
        unused_mut,
        clippy::vec_init_then_push,
        reason = "mut and push count depend on which platform backends are compiled in"
    )]
    pub fn list_backends() -> Vec<CaptureBackend> {
        let mut backends = Vec::new();
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        if pipewire_available() {
            backends.push(CaptureBackend::PipeWire);
        }
        #[cfg(all(target_os = "linux", feature = "x11"))]
        backends.push(CaptureBackend::X11);
        #[cfg(all(target_os = "macos", feature = "screen-apple"))]
        backends.push(CaptureBackend::ScreenCaptureKit);
        #[cfg(feature = "xcap")]
        backends.push(CaptureBackend::Xcap);
        backends
    }

    /// Lists monitors from the default (preferred) backend.
    pub fn list() -> anyhow::Result<Vec<MonitorInfo>> {
        list_monitors()
    }

    /// Lists monitors from all compiled-in backends.
    pub fn list_all() -> anyhow::Result<Vec<MonitorInfo>> {
        list_all_monitors()
    }

    /// Opens the default (primary) monitor for screen capture.
    pub fn new() -> anyhow::Result<Self> {
        let config = ScreenConfig::default();
        let inner = create_screen_backend(None, &config)?;
        Ok(Self { inner })
    }

    /// Opens a screen matching an optional backend and/or monitor ID.
    ///
    /// Same selection semantics as [`CameraCapturer::open()`].
    pub fn open(
        backend: Option<CaptureBackend>,
        id: Option<&str>,
        config: &ScreenConfig,
    ) -> anyhow::Result<Self> {
        match (backend, id) {
            (None, None) => Self::new(),
            (Some(b), None) => Self::with_backend(b, config),
            (None, Some(id)) => {
                let monitors = list_monitors()?;
                let info = monitors
                    .iter()
                    .find(|m| m.id == id || m.name == id)
                    .ok_or_else(|| format_screen_not_found(id, &monitors))?;
                let inner = create_screen_for_backend(info, config)?;
                Ok(Self { inner })
            }
            (Some(b), Some(id)) => {
                let monitors = list_monitors()?;
                let info = monitors
                    .iter()
                    .find(|m| m.backend == b && (m.id == id || m.name == id))
                    .ok_or_else(|| format_screen_not_found(id, &monitors))?;
                let inner = create_screen_for_backend(info, config)?;
                Ok(Self { inner })
            }
        }
    }

    /// Opens the default screen using a specific backend.
    ///
    /// Returns an error if the backend is not available or does not support screen capture.
    pub fn with_backend(backend: CaptureBackend, config: &ScreenConfig) -> anyhow::Result<Self> {
        let monitors = list_monitors()?;
        let mon = monitors.iter().find(|m| m.backend == backend);
        match (backend, mon) {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            (CaptureBackend::PipeWire, _) if pipewire_available() => {
                let inner = create_screen_backend(None, config)?;
                Ok(Self { inner })
            }
            (_, Some(mon)) if mon.backend == backend => {
                let inner = create_screen_backend(Some(mon), config)?;
                Ok(Self { inner })
            }
            _ => anyhow::bail!("screen backend {backend} is not available"),
        }
    }

    /// Opens a specific monitor for screen capture.
    pub fn with_monitor(monitor: &MonitorInfo) -> anyhow::Result<Self> {
        let config = ScreenConfig::default();
        let inner = create_screen_backend(Some(monitor), &config)?;
        Ok(Self { inner })
    }

    /// Lists available windows for capture (macOS only currently).
    pub fn list_windows() -> anyhow::Result<Vec<WindowInfo>> {
        #[allow(unused_mut, reason = "empty for some platforms")]
        let mut windows = Vec::new();
        #[cfg(all(target_os = "macos", feature = "screen-apple"))]
        {
            windows.extend(platform::apple::screen::windows()?);
        }
        Ok(windows)
    }

    /// Opens a specific window for capture by window ID.
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    pub fn with_window(window_id: u32, config: &ScreenConfig) -> anyhow::Result<Self> {
        let inner = Box::new(platform::apple::screen::MacScreenCapturer::new_window(
            window_id, config,
        )?);
        Ok(Self { inner })
    }

    /// Opens a specific window for capture by window ID.
    ///
    /// Currently only supported on macOS (ScreenCaptureKit).
    #[cfg(not(all(target_os = "macos", feature = "screen-apple")))]
    pub fn with_window(_window_id: u32, _config: &ScreenConfig) -> anyhow::Result<Self> {
        anyhow::bail!("window capture is not supported on this platform")
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

// ---------------------------------------------------------------------------
// Error formatting helpers
// ---------------------------------------------------------------------------

fn format_not_found(kind: &str, id: &str, cameras: &[CameraInfo]) -> anyhow::Error {
    let available: Vec<String> = cameras.iter().map(|c| c.summary()).collect();
    if available.is_empty() {
        anyhow::anyhow!("no {kind}s found")
    } else {
        anyhow::anyhow!(
            "{kind} '{id}' not found. Available:\n  {}",
            available.join("\n  ")
        )
    }
}

fn format_screen_not_found(id: &str, monitors: &[MonitorInfo]) -> anyhow::Error {
    let available: Vec<String> = monitors.iter().map(|m| m.summary()).collect();
    if available.is_empty() {
        anyhow::anyhow!("no screens found")
    } else {
        anyhow::anyhow!(
            "screen '{id}' not found. Available:\n  {}",
            available.join("\n  ")
        )
    }
}
