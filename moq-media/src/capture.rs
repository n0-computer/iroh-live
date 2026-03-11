//! Camera and screen capture, backed by [`rusty_capture`].
//!
//! Re-exports the high-level [`CameraCapturer`] and [`ScreenCapturer`] types
//! that auto-select the best platform backend.

#[cfg(feature = "capture-camera")]
pub use rusty_capture::{CameraCapturer, CameraListEntry};

#[cfg(feature = "capture-screen")]
pub use rusty_capture::ScreenCapturer;
