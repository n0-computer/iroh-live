//! A player over `moq_video` and `moq_audio`.
//!
//! It picks the rendition the link can carry and keeps audio and video in
//! step. Nothing here depends on iroh or on a publisher. A transport builds a
//! [`RemoteBroadcast`] from what it subscribed to, and attaches its view of the
//! link with [`RemoteBroadcast::set_network`]. A broadcast that lives in the
//! same process is read with [`RemoteBroadcast::from_consumer`].
//!
//! # Example
//!
//! ```no_run
//! # async fn example(consumer: moq_net::broadcast::Consumer) -> Result<(), moq_ext_player::Error> {
//! use moq_ext_player::{PlayerConfig, RemoteBroadcast};
//!
//! let player = RemoteBroadcast::from_consumer(consumer).play(PlayerConfig::default())?;
//! let mut frames = player.video_frames();
//! while let Some(frame) = frames.next().await {
//!     // draw `frame`
//! #   let _ = frame;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The main types are:
//!
//! - [`RemoteBroadcast`]: a subscribed broadcast and its [`Catalog`] (hang's).
//!   [`RemoteBroadcast::play`] starts a [`Player`].
//! - [`Player`]: decodes the chosen rendition and keeps audio and video in step.
//! - [`VideoFrames`]: the frame stream every renderer reads.
//! - [`AudioFrames`]: the decoded audio, for analysis or a custom output.
//! - [`RenditionMode`]: how the video rendition is chosen, re-exported from
//!   `moq_ext_adaptive`.
//!
//! A [`RemoteBroadcast`] that follows a route table looks for another route
//! when its broadcast ends. A player adapts only to the network signals a
//! transport attached. Without them, [`RenditionMode::Auto`] plays the best
//! rendition its limits allow.
//!
//! Dropping a [`Player`] aborts its tasks and returns at once. Dropping the
//! last clone of a [`RemoteBroadcast`] aborts the task that follows it.
//!
//! # Cancellation safety
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | [`VideoFrames::next`], [`AudioFrames::next`], [`RemoteBroadcast::closed`] | yes | loses nothing |
//! | [`RemoteBroadcast::catalog_within`] | yes | stops waiting |
//! | `RemoteBroadcast::mux_source` (feature `record`) | yes | stops waiting |

mod catalog;
mod clean_close;
mod network;
mod player;
mod remote;
mod stats;
// Helpers for this workspace's tests, not part of the public API.
#[cfg(feature = "test-util")]
#[doc(hidden)]
pub mod test_util;

pub use hang::catalog::Catalog;
use moq_ext_sources::frames;
pub use moq_ext_sources::{Error, SlotState, SourceClosed, VideoFrames};
pub use moq_net::track::Delay;

pub use self::{
    catalog::video_rendition,
    network::NetworkSample,
    player::{
        Adaptation, AudioFrame, AudioFrames, AudioMode, DEFAULT_LATENCY, IMMEDIATE_LATENCY, Player,
        PlayerConfig, PlayerStatus, RenditionMode,
    },
    remote::RemoteBroadcast,
    stats::{AudioPlaybackStats, FrameTiming, MediaKind, PlaybackStats, VideoPlaybackStats},
};
