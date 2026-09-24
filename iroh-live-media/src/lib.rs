//! Live audio and video for iroh-live: sources, broadcasts and players.
//!
//! The media itself is upstream: `moq_video` captures, encodes, decodes and
//! renders; `moq_audio` does the same for sound and owns the speaker. What
//! lives here is the layer an application talks to, in concepts of its own:
//!
//! - A [`VideoSource`] or [`AudioSource`] is an opened device or generator,
//!   running on a thread of its own.
//! - A [`LocalBroadcast`] encodes one video source into a ladder of
//!   renditions, and one audio source, whatever transport carries it.
//! - A [`RemoteBroadcast`] reads a broadcast's [`Catalog`], and a [`Player`]
//!   plays it: it picks a rendition as the link allows, switches without the
//!   picture going blank or stepping backwards, and keeps audio and video in
//!   step through its own playout clock.
//! - [`VideoFrames`] is the one frame stream every renderer reads, whether
//!   from a player, a source's preview, or a scanner.
//! - An [`AudioOutput`] is an opened speaker, passed to every player that
//!   plays through it and to the microphone whose echo it cancels.
//!
//! Nothing here depends on iroh. A transport publishes a [`LocalBroadcast`]
//! through `moq_net::Consume`, builds a [`RemoteBroadcast`] from what it
//! subscribed to, and attaches its view of the link as [`NetworkSignals`].
//!
//! # Example
//!
//! Publish a generated pattern and play it back in-process:
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

mod bitrate;
mod catalog;
mod error;
mod frames;
mod local_task;
mod network;
mod output;
mod player;
mod publish;
mod record;
mod remote;
mod source;
mod stats;

/// The upstream audio stack: capture, encode, decode, playback, and echo
/// cancellation.
pub use moq_audio as audio;
/// The upstream video stack: capture, encode, decode, render, and the
/// [`Frame`](moq_video::Frame) vocabulary every one of them speaks.
pub use moq_video as video;

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::source::RpicamConfig;
pub use self::{
    bitrate::Bitrate,
    catalog::{
        AudioRenditionInfo, Catalog, Chat, IrohLiveExt, Metadata, TrackRef, User,
        VideoRenditionInfo,
    },
    error::{Closed, Error, SwitchError},
    frames::VideoFrames,
    network::{NetworkSample, NetworkSignals},
    output::AudioOutput,
    player::{Latency, Player, PlayerConfig, PlayerStatus, RenditionMode},
    publish::{
        AudioEncoding, LocalBroadcast, PublishStatus, RenditionState, SlotState, VideoEncoding,
        VideoRendition,
    },
    record::{RecordConfig, RecordFormat, Recording},
    remote::RemoteBroadcast,
    source::{
        AudioFormat, AudioSource, EncodedVideoSource, FrameSender, MicrophoneConfig, VideoFormat,
        VideoSource,
    },
    stats::{
        AudioEncodeStats, AudioPlaybackStats, EncodeStats, PlaybackStats, PublishStats,
        VideoPlaybackStats,
    },
};
