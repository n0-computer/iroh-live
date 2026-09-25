//! Live audio and video over iroh.
//!
//! A [`Live`] node binds the [`Moq`] transport to an iroh endpoint. It
//! publishes broadcasts at `live/<its endpoint id>/<name>` and subscribes to
//! other nodes' broadcasts over a direct session or an attached relay. The
//! media crate is re-exported as [`media`], the transport as [`moq`], and the
//! rooms crate as `rooms` with the `rooms` feature.
//!
//! This publishes a test pattern and plays someone else's broadcast:
//!
//! ```no_run
//! use iroh_live::{
//!     BroadcastTicket, Live, LocalBroadcast, MoqPreset, PlayerConfig, VideoSource,
//!     media::{VideoEncoding, VideoRendition, video},
//! };
//!
//! # async fn run(ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
//! let live = Live::builder(endpoint).with_router().spawn();
//!
//! let broadcast = LocalBroadcast::new();
//! let source = VideoSource::test_pattern(
//!     video::Size::new(1280, 720),
//!     video::Rate::new(30, 1).expect("a valid rate"),
//! );
//! broadcast.set_video(
//!     source,
//!     VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
//! )?;
//! live.publish("studio", &broadcast)?;
//! println!("share {}", live.ticket("studio"));
//!
//! let remote = live.subscribe(&ticket).await?;
//! let player = remote.play(PlayerConfig::default())?;
//! let mut frames = player.video();
//! while let Some(frame) = frames.next().await {
//!     // hand `frame` to a renderer
//! #   drop(frame);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The main types:
//!
//! - [`Live`]: the node. [`LiveBuilder`] mounts more protocols on its router.
//! - [`BroadcastTicket`]: a publisher's endpoint id and a broadcast name.
//! - [`Call`]: a one-to-one call, each side publishing [`CALL`].
//! - [`LocalBroadcast`] and [`RemoteBroadcast`], from [`media`].
//!
//! # Cancellation safety
//!
//! Every public future of this crate and the crates it re-exports. "Safe"
//! means dropping the future leaves nothing half done that a later call
//! cannot pick up.
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | `VideoSource::capture`, `VideoSource::rpicam`, `EncodedVideoSource::rpicam`, `AudioSource::file` | yes | stops the thread or subprocess and releases the device |
//! | `AudioSource::microphone` | yes | nothing is open yet |
//! | `AudioOutput::open`, `AudioOutput::devices` | yes | closes the device, or abandons the query |
//! | `AudioOutput::switch` | yes | the switch completes, only its result is lost |
//! | `EndpointOptions::bind`, `EndpointOptions::builder` | yes | nothing is bound |
//! | [`Live::subscribe`], [`Call::dial`], [`Call::accept`], `Moq::subscribe`, `Moq::connect`, `Moq::connect_with`, `Session::subscribe`, `Room::subscribe` | yes | abandons the wait; a dial it started continues for other callers |
//! | `moq::transport::dial`, `moq::transport::accept` | yes | drops the connection being set up |
//! | `Moq::accept` | yes | a queued session stays queued |
//! | `Incoming::admit` | yes | rejects the session before the handshake completes, admits it after |
//! | `Rooms::join` | yes | leaves the topic, publishes nothing |
//! | every `closed()`, `Publication::withdrawn`, `VideoFrames::next`, every watcher's `updated()` | yes | loses nothing |
//! | `Player::wait_for_rendition` | yes | the switch continues |
//! | `Recording::wait`, `Recording::stop` | yes, while the `Recording` is kept | a later `wait` returns the result; dropping the `Recording` stops it without flushing |
//! | `RelayLink::detach` | yes | the link's task finishes the close |
//! | [`Live::shutdown`], `Moq::shutdown`, `Room::leave` | no, idempotent | call again to finish |
//!
//! No public call takes a deadline. Bound a wait with `tokio::time::timeout`.
//!
//! # Features
//!
//! `capture`, `render` and `sound-server` are on by default. `playback`,
//! `aec`, `pipewire`, `vaapi`, `nvidia`, `v4l2` and `rpicam` add devices and
//! codecs to [`media`]. `rooms` re-exports `iroh-live-rooms`, and `auth`
//! enables `Grant::from_claims`.

mod call;
mod error;
mod live;
mod network;
mod ticket;

pub use iroh_live_media as media;
pub use iroh_live_media::{
    AudioOutput, AudioSource, Latency, LocalBroadcast, Player, PlayerConfig, RemoteBroadcast,
    RenditionMode, VideoSource,
};
#[cfg(feature = "rooms")]
pub use iroh_live_rooms as rooms;
pub use iroh_moq as moq;
pub use iroh_moq::{
    Audience, EndpointOptions, Mdns, Moq, MoqConfig, MoqPreset, Publication, Reach, Session,
    Subscription,
};

pub use self::{
    call::{CALL, Call},
    error::Error,
    live::{Live, LiveBuilder, grant, moq_config, publish_scope},
    ticket::BroadcastTicket,
};
