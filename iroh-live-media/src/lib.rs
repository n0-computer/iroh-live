//! Live audio and video for iroh-live: sources, broadcasts and players.
//!
//! The codecs and devices come from `moq_video` and `moq_audio`, re-exported
//! as [`video`] and [`audio`]. This crate adds what a live app needs on top:
//! a broadcast that encodes one source into several renditions, and a player
//! that picks the rendition the link can carry and keeps audio and video in
//! step.
//!
//! Nothing here depends on iroh. A transport publishes a [`LocalBroadcast`]
//! through `moq_net::Consume`, builds a [`RemoteBroadcast`] from what it
//! subscribed to, and attaches its view of the link with
//! [`RemoteBroadcast::with_network`].
//!
//! # Example
//!
//! This publishes a test pattern and plays it back in-process:
//!
//! ```no_run
//! # async fn example() -> Result<(), iroh_live_media::Error> {
//! use iroh_live_media::{
//!     LocalBroadcast, PlayerConfig, RemoteBroadcast, VideoEncoding, VideoRendition, VideoSource,
//!     video,
//! };
//!
//! let source = VideoSource::test_pattern(
//!     video::Size::new(1280, 720),
//!     video::Rate::new(30, 1).expect("a valid rate"),
//! );
//! let broadcast = LocalBroadcast::new();
//! broadcast.set_video(
//!     source,
//!     VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
//! )?;
//!
//! let player = RemoteBroadcast::local(&broadcast).play(PlayerConfig::default())?;
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
//! - [`VideoSource`] and [`AudioSource`]: an opened device, file or generator,
//!   running on its own thread.
//! - [`LocalBroadcast`]: encodes one video source into a ladder of renditions,
//!   and one audio source.
//! - [`RemoteBroadcast`]: a subscribed broadcast and its [`Catalog`].
//!   [`RemoteBroadcast::play`] starts a [`Player`].
//! - [`VideoFrames`]: the frame stream every renderer reads, from a player or
//!   from a source's preview.
//! - [`AudioOutput`]: an opened speaker. Players play through it, and a
//!   microphone can cancel its echo.
//!
//! A source fails where it opens: `VideoSource::capture` returns once the
//! device produced a frame. A microphone is different, because moq-audio opens
//! it inside the publication that encodes it. `AudioSource::microphone` only
//! checks that the device exists, and a device that fails later shows in
//! [`LocalBroadcast::status`].
//!
//! A [`RemoteBroadcast`] that follows a route table looks for another route
//! when its broadcast ends. A player adapts only to the network signals a
//! transport attached. Without them, [`RenditionMode::Auto`] plays the best
//! rendition its limits allow.
//!
//! # Cancellation safety
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | `VideoSource::capture`, `VideoSource::rpicam`, `EncodedVideoSource::rpicam`, [`AudioSource::file`] | yes | stops the thread or subprocess and releases the device |
//! | `AudioSource::microphone` | yes | nothing is open yet |
//! | `AudioOutput::open`, `AudioOutput::devices` | yes | closes the device, or abandons the query |
//! | `AudioOutput::switch` | yes | the switch still completes, only its result is lost |
//! | [`VideoFrames::next`], [`FrameSender::closed`], [`LocalBroadcast::closed`], [`RemoteBroadcast::closed`] | yes | loses nothing |
//! | [`Player::wait_for_rendition`] | yes | the switch continues |
//! | [`Recording::wait`], [`Recording::stop`] | yes, while the [`Recording`] is kept | the recording runs on its own task; dropping the `Recording` stops it without flushing |

mod catalog;
mod error;
mod frames;
#[cfg(any(feature = "capture", all(target_os = "linux", feature = "rpicam")))]
mod local_task;
mod network;
mod output;
mod player;
mod publish;
mod record;
mod remote;
mod source;
mod stats;

/// The upstream audio stack: capture, encode, decode and playback.
pub use moq_audio as audio;
/// A rate in bits per second, as moq-net measures it.
pub use moq_net::bandwidth::Rate as Bitrate;
/// The upstream video stack: capture, encode, decode, render and
/// [`Frame`](moq_video::Frame).
pub use moq_video as video;

#[cfg(feature = "capture")]
pub use self::source::MicrophoneConfig;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::source::RpicamConfig;
pub use self::{
    catalog::Catalog,
    error::{Closed, Error, SwitchError},
    frames::VideoFrames,
    network::NetworkSample,
    output::AudioOutput,
    player::{Adaptation, Latency, Player, PlayerConfig, PlayerStatus, RenditionMode},
    publish::{
        AudioEncoding, LocalBroadcast, PublishStatus, RenditionState, SlotState, VideoEncoding,
        VideoRendition,
    },
    record::{RecordConfig, RecordFormat, Recording},
    remote::RemoteBroadcast,
    source::{AudioFormat, AudioSource, EncodedVideoSource, FrameSender, VideoFormat, VideoSource},
    stats::{
        AudioEncodeStats, AudioPlaybackStats, EncodeStats, FrameTiming, MediaKind, PlaybackStats,
        PublishStats, VideoPlaybackStats,
    },
};
