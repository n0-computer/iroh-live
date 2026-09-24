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
//!
//! Nothing here is async. The camera bridge never blocks: a pushed frame
//! replaces one the encoder has not taken yet.
//!
//! # Example
//!
//! Publish frames a camera callback pushes, and stop the camera while nobody
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
