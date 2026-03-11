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
//! | macOS | `apple-screen` feature | `apple` feature | IOSurface (planned) |
//! | iOS | — | `apple` feature | IOSurface (planned) |
//! | Windows | — (stub) | — (stub) | D3D11 (planned) |
//! | Android | — (stub) | — (stub) | AHardwareBuffer (planned) |
//!
//! # Feature Flags
//!
//! - **`pipewire`** — PipeWire screen + camera capture on Linux (Wayland + libcamera).
//!   Requires `libpipewire-0.3-dev` at build time.
//! - **`v4l2`** — V4L2 camera capture on Linux. Works on all Raspberry Pi models
//!   (Pi 1–5, Zero) and USB cameras. Zero-copy DMA-BUF export when the driver supports it.
//! - **`x11`** — X11 screen capture via MIT-SHM. CPU-only fallback for non-Wayland systems.
//! - **`apple`** — AVFoundation camera capture on macOS and iOS.
//! - **`apple-screen`** — ScreenCaptureKit screen capture on macOS 12.3+.

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

#[cfg(all(target_os = "macos", feature = "apple-screen"))]
pub use platform::apple::screen::MacScreenCapturer;

#[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "apple"))]
pub use platform::apple::camera::AppleCameraCapturer;

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

#[cfg(all(target_os = "macos", feature = "apple-screen"))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    platform::apple::screen::monitors()
}

#[cfg(not(any(
    all(target_os = "linux", feature = "x11"),
    all(target_os = "linux", feature = "pipewire"),
    all(target_os = "macos", feature = "apple-screen"),
)))]
fn cfg_dispatch_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    anyhow::bail!(
        "no screen capture backend available (enable `x11`, `pipewire`, or `apple-screen` feature)"
    )
}

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

#[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "apple",))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    platform::apple::camera::cameras()
}

#[cfg(not(any(
    all(target_os = "linux", feature = "v4l2"),
    all(target_os = "linux", feature = "pipewire"),
    all(any(target_os = "macos", target_os = "ios"), feature = "apple"),
)))]
fn cfg_dispatch_cameras() -> anyhow::Result<Vec<CameraInfo>> {
    anyhow::bail!(
        "no camera capture backend available (enable `v4l2`, `pipewire`, or `apple` feature)"
    )
}

// ── PipeWire runtime detection ──────────────────────────────────────

#[cfg(all(
    target_os = "linux",
    feature = "pipewire",
    any(feature = "x11", feature = "v4l2")
))]
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
