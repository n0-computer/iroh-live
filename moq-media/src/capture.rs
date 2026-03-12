//! Camera and screen capture, backed by [`rusty_capture`].
//!
//! Re-exports the high-level [`CameraCapturer`] and [`ScreenCapturer`] types
//! that auto-select the best platform backend. Platform-specific capturers
//! are also available for direct use.

#[cfg(feature = "capture-camera")]
pub use rusty_capture::CameraCapturer;

#[cfg(feature = "capture-screen")]
pub use rusty_capture::ScreenCapturer;

#[cfg(all(target_os = "linux", feature = "capture-camera"))]
pub use rusty_capture::PipeWireCameraCapturer;

#[cfg(all(target_os = "linux", feature = "capture-camera"))]
pub use rusty_capture::V4l2CameraCapturer;
