//! Android integration for moq-media.
//!
//! Provides reusable building blocks for Android apps that use moq-media:
//!
//! - [`camera`] bridges Android's push-model camera callbacks to the pull-model
//!   [`VideoSource`](moq_media::publish::VideoSource) a publish task reads
//! - `egl` provides safe wrappers around the EGL and GLES extension functions for
//!   the HardwareBuffer to EGLImage to GL texture path
//! - [`handle`] — `Arc<Mutex<T>>` ↔ `i64` conversion for JNI handles

pub mod camera;
#[cfg(target_os = "android")]
pub mod egl;
pub mod handle;
#[cfg(target_os = "android")]
pub mod renderer;
