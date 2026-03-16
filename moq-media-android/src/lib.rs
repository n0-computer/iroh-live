//! Android integration for moq-media.
//!
//! Provides reusable building blocks for Android apps that use moq-media:
//!
//! - [`camera`] — push-based [`VideoSource`](moq_media::traits::VideoSource) for
//!   Android camera frames (NV12 or RGBA)
//! - [`egl`] — safe wrappers around EGL/GLES extension functions for
//!   HardwareBuffer → EGLImage → GL texture rendering
//! - [`handle`] — `Arc<Mutex<T>>` ↔ `i64` conversion for JNI handles

pub mod camera;
#[cfg(target_os = "android")]
pub mod egl;
pub mod handle;
