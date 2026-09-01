//! Android integration for moq-media.
//!
//! Provides reusable building blocks for Android apps that use moq-media:
//!
//! - [`camera`] bridges Android's push-model camera callbacks to the pull-model
//!   [`VideoSource`](moq_media::publish::VideoSource) a publish task reads
//! - [`egl`] — safe wrappers around EGL/GLES extension functions for
//!   HardwareBuffer → EGLImage → GL texture rendering
//! - [`handle`] — `Arc<Mutex<T>>` ↔ `i64` conversion for JNI handles

pub mod camera;
#[cfg(target_os = "android")]
pub mod egl;
pub mod handle;
#[cfg(target_os = "android")]
pub mod renderer;
