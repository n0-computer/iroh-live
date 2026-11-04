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
//! let mut frames = player.video();
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
//! - [`AudioTarget`]: where a player's audio goes, a `moq_audio::playback::Engine`
//!   or nowhere. A microphone can cancel the echo of an engine's mix.
//! - [`RenditionMode`]: how the video rendition is chosen, which is
//!   `moq_ext_adaptive::Mode`.
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
//! | [`VideoFrames::next`], [`RemoteBroadcast::closed`] | yes | loses nothing |
//! | [`Player::wait_for_rendition`] | yes | the switch continues |
//! | `Recording::wait`, `Recording::stop` (feature `record`) | yes, while the recording is kept | the recording runs on its own task; dropping the `Recording` stops it without flushing |

mod catalog;
mod error;
mod network;
mod player;
#[cfg(feature = "record")]
mod record;
mod remote;
mod stats;

use moq_ext_sources::frames;

#[cfg(feature = "record")]
pub use self::record::{RecordConfig, RecordFormat, Recording};
pub use self::{
    catalog::VideoRenditions,
    error::SwitchError,
    network::NetworkSample,
    player::{Adaptation, AudioTarget, Latency, Player, PlayerConfig, PlayerStatus, RenditionMode},
    remote::RemoteBroadcast,
    stats::{AudioPlaybackStats, FrameTiming, MediaKind, PlaybackStats, VideoPlaybackStats},
};
pub use hang::catalog::Catalog;
pub use moq_ext_media_core::{Closed, Error, SlotState};
pub use moq_ext_sources::VideoFrames;
