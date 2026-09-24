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
//!
//! # Behaviour worth knowing
//!
//! - Opening a source is where its failure shows: [`VideoSource::capture`]
//!   returns once the device produced a frame. The microphone is the
//!   exception. `AudioSource::microphone` checks that a matching device exists
//!   and that echo cancellation, if asked for, is compiled in; the device
//!   itself opens when a broadcast first has a listener for it, and a failure
//!   then shows in [`LocalBroadcast::status`]. Upstream opens a microphone only
//!   inside the publication that encodes it.
//! - A [`RemoteBroadcast`] that follows a route table
//!   ([`RemoteBroadcast::from_origin`], [`RemoteBroadcast::from_resolved`])
//!   treats the end of its broadcast as a possible change of route and asks the
//!   table again, so it closes about three seconds after the publisher went.
//! - Players adapt only on [`NetworkSignals`] a transport attached; without
//!   them a player in [`RenditionMode::Auto`] holds the best rendition its
//!   constraints allow.
//!
//! # Cancellation safety
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | [`VideoSource::capture`], `VideoSource::rpicam`, `EncodedVideoSource::rpicam`, [`AudioSource::file`] | yes | stops the thread or subprocess and releases the device |
//! | `AudioSource::microphone` | yes | nothing is open yet |
//! | `AudioOutput::open`, `AudioOutput::devices` | yes | closes the device, or abandons the query |
//! | `AudioOutput::switch` | yes | the switch was queued before the first wait and completes; only its result is lost |
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

/// The upstream audio stack: capture, encode, decode, playback, and echo
/// cancellation.
pub use moq_audio as audio;
/// A rate in bits per second, as moq-net measures it.
pub use moq_net::bandwidth::Rate as Bitrate;
/// The upstream video stack: capture, encode, decode, render, and the
/// [`Frame`](moq_video::Frame) vocabulary every one of them speaks.
pub use moq_video as video;

#[cfg(feature = "capture")]
pub use self::source::MicrophoneConfig;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::source::RpicamConfig;
pub use self::{
    catalog::Catalog,
    error::{Closed, Error, SwitchError},
    frames::VideoFrames,
    network::{NetworkSample, NetworkSignals},
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
