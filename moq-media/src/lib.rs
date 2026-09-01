//! Publish and subscribe plumbing over `moq-video` and `moq-audio`.
//!
//! The media itself is upstream: `moq_video` captures, encodes, decodes, and
//! renders; `moq_audio` does the same for sound and owns the speaker. What
//! lives here is the layer iroh-live needs on top and moq has no counterpart
//! for:
//!
//! - [`publish`] fans one camera out to a simulcast ladder, because an upstream
//!   producer publishes one rendition and owns its device.
//! - [`subscribe`] chooses among those renditions as the downlink moves
//!   ([`adaptive`]), and keeps audio and video aligned across two independent
//!   decode paths ([`sync`], [`playout`]).
//! - [`catalog`] extends hang's catalog with the chat and identity sections
//!   iroh-live publishes.
//! - [`stats`] is the client-side instrumentation a UI draws.
//!
//! Nothing here depends on iroh: a broadcast arrives as a
//! [`moq_net::broadcast::Producer`] or `Consumer`, whatever carried it.

pub mod adaptive;
pub mod audio_file;
pub mod catalog;
pub mod frame_channel;
pub mod net;
#[cfg(feature = "playback")]
pub mod playback;
pub mod playout;
pub mod publish;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub mod rpicam;
pub mod stats;
pub mod subscribe;
pub mod sync;
#[cfg(any(test, feature = "test-source"))]
pub mod test_source;

/// The upstream video stack: capture, encode, decode, render, and the [`Frame`]
/// vocabulary every one of them speaks.
///
/// [`Frame`]: moq_video::Frame
pub use moq_video as video;

/// The upstream audio stack: capture, encode, decode, playback, and echo
/// cancellation.
pub use moq_audio as audio;
