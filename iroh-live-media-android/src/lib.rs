//! Android integration for iroh-live-media.
//!
//! Hardware H.264 through MediaCodec lives in `moq-video`. This crate holds the
//! other pieces an Android app needs:
//!
//! - [`camera`] feeds frames from Android camera callbacks into a
//!   [`VideoSource`](iroh_live_media::VideoSource).
//! - `renderer` draws frames to an Android `Surface` with EGL and GLES2.
//! - `egl` wraps the EGL and GLES extension functions the renderer needs.
//! - [`handle`] passes an `Arc<Mutex<T>>` across JNI as an `i64`.
//!
//! `renderer` and `egl` only exist on Android. Nothing here is async, and the
//! camera bridge never blocks: a pushed frame replaces one the encoder has not
//! taken yet.
//!
//! # Example
//!
//! This publishes frames from a camera callback and checks whether anyone
//! watches:
//!
//! ```no_run
//! use iroh_live_media::{LocalBroadcast, VideoEncoding, VideoRendition};
//! use iroh_live_media_android::camera;
//! use moq_video::{Rate, Size};
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let (sink, source) = camera::camera(Size::new(1280, 720), Rate::new(30, 1)?);
//! let broadcast = LocalBroadcast::new();
//! broadcast.set_video(
//!     source,
//!     VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
//! )?;
//!
//! // From the camera callback, on whichever thread it runs:
//! # let rgba = vec![0u8; 1280 * 720 * 4];
//! sink.push_rgba(&rgba, moq_net::Timestamp::now())?;
//!
//! // Whether anything encodes the frames right now.
//! let demand = sink.demand();
//! # drop(demand);
//! # Ok(())
//! # }
//! ```

pub mod camera;
#[cfg(target_os = "android")]
pub mod egl;
pub mod handle;
#[cfg(target_os = "android")]
pub mod renderer;
