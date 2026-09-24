//! Android integration for iroh-live-media.
//!
//! Provides reusable building blocks for Android apps that use iroh-live-media:
//!
//! - [`camera`] bridges Android's push-model camera callbacks to the
//!   [`VideoSource`](iroh_live_media::VideoSource) a
//!   [`LocalBroadcast`](iroh_live_media::LocalBroadcast) encodes
//! - `egl` provides safe wrappers around the EGL and GLES extension functions for
//!   the HardwareBuffer to EGLImage to GL texture path
//! - [`handle`]: `Arc<Mutex<T>>` <-> `i64` conversion for JNI handles

pub mod camera;
#[cfg(target_os = "android")]
pub mod egl;
pub mod handle;
#[cfg(target_os = "android")]
pub mod renderer;
