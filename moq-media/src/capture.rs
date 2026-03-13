//! Camera and screen capture, backed by [`rusty_capture`].
//!
//! Re-exports the high-level [`CameraCapturer`] and [`ScreenCapturer`] types
//! that auto-select the best platform backend. Platform-specific capturers
//! are also available for direct use.

pub use rusty_capture::*;
