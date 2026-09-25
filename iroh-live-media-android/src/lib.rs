//! Android support for iroh-live-media: an EGL renderer and JNI handle helpers.
//!
//! Hardware H.264 through MediaCodec lives in `moq-video`. This crate holds the
//! other pieces an Android app needs:
//!
//! - `renderer` draws frames to an Android `Surface` with EGL and GLES2.
//! - `egl` wraps the EGL and GLES extension functions the renderer needs.
//! - [`handle`] passes an `Arc<Mutex<T>>` across JNI as an `i64`.
//!
//! `renderer` and `egl` only exist on Android. Camera frames reach a broadcast
//! through `iroh_live_media::VideoSource::push`.

#[cfg(target_os = "android")]
pub mod egl;
pub mod handle;
#[cfg(target_os = "android")]
pub mod renderer;
