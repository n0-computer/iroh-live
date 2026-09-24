//! Live audio and video over iroh.
//!
//! [`Live`] is a node ready for live media: it binds the [`Moq`] transport to an
//! iroh [`Endpoint`](iroh::Endpoint), publishes broadcasts at paths that name
//! this node, and resolves other nodes' broadcasts over whichever link serves
//! them, a direct session or an attached relay. Everything else is re-exported:
//! the media crate as [`media`], the transport as [`moq`], and rooms as `rooms`
//! behind the `rooms` feature, so an application needs one dependency and
//! builds against the exact versions this crate does.
//!
//! # Example
//!
//! Publish a generated picture, and watch someone else's broadcast:
//!
//! ```no_run
//! use iroh_live::{
//!     BroadcastTicket, Live, LocalBroadcast, MediaPreset, PlayerConfig, VideoSource,
//!     media::{VideoEncoding, VideoRendition, video},
//! };
//!
//! # async fn run(ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MediaPreset).await?;
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
//! # Behaviour worth knowing
//!
//! - [`Live::subscribe`] returns once a route to the broadcast is found, without
//!   waiting for its catalog. Watch [`RemoteBroadcast::catalog`] for that.
//! - A subscribed broadcast follows its path in the route table, so a change of
//!   route (a direct peer gone, a relay still serving) is a switch, not an end.
//!   The price is that [`RemoteBroadcast::closed`] cannot tell a publisher that
//!   hung up from a change of route: it resolves about three seconds after the
//!   publisher closed, once no route has answered.
//! - Players adapt on the link that serves the broadcast, direct session or
//!   relay link, as its connection monitor reads it
//!   ([`moq::LinkSample`]).
//! - `AudioSource::microphone` checks that the device exists; the device
//!   opens when a broadcast first has a listener for it, and a failure then
//!   shows in [`LocalBroadcast::status`].
//! - A relay attached with its default config also consumes: every route the
//!   relay knows enters this node's route table. A node that only publishes
//!   through a relay turns that off with
//!   `RelayConfig::with_consume(false)` (`relay-links` feature).
//! - In a room, a member can end a broadcast and publish it again under the
//!   same name without the room's state changing. A grid of tiles has to drop
//!   the players whose [`RemoteBroadcast::is_closed`] and open them again, on a
//!   timer as well as on state updates; `iroh_rooms` shows the pattern.
//!
//! # Cancellation safety
//!
//! Every public future of this crate and the crates it re-exports, in one
//! place. "Safe" means dropping the future before it resolves leaves nothing
//! half done that a later call cannot pick up.
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | `VideoSource::capture`, `VideoSource::rpicam`, `EncodedVideoSource::rpicam`, `AudioSource::file` | yes | stops the thread or subprocess and releases the device |
//! | `AudioSource::microphone` | yes | nothing is open yet; the device opens when a broadcast first wants it |
//! | `AudioOutput::open`, `AudioOutput::devices` | yes | closes the device, or abandons the query |
//! | `AudioOutput::switch` | yes | the switch was queued before the first wait and completes; only its result is lost |
//! | `EndpointOptions::bind`, `EndpointOptions::builder` | yes | nothing is bound |
//! | [`Live::subscribe`], `Moq::subscribe`, `Moq::connect`, `Moq::connect_with`, `Session::subscribe`, `Room::subscribe` | yes | abandons the wait; a dial it started continues for other callers |
//! | `moq::transport::dial`, `moq::transport::accept` | yes | drops the connection being set up |
//! | `Moq::accept` | yes | loses nothing: a queued session stays queued |
//! | `Incoming::admit` | yes | before the handshake completes, rejects the session; after, it is admitted all the same |
//! | `Rooms::join` | yes | leaves the topic, publishes nothing |
//! | every `closed()`, `Publication::withdrawn`, `VideoFrames::next`, `ChatReceiver::recv`, every watcher's `updated()` | yes | loses nothing |
//! | `Player::wait_for_rendition` | yes | the switch continues |
//! | `Recording::wait`, `Recording::stop` | yes, while the `Recording` is kept | the recording runs on its own task, `stop` signals before its first wait, and a later `wait` returns the result; dropping the `Recording` stops it without flushing |
//! | `RelayLink::detach` | yes | the close was signalled before the first wait; the link's task finishes it |
//! | `Room::send_chat` | no | the message may or may not have been sent |
//! | [`Live::shutdown`], `Moq::shutdown`, `Room::leave` | no, idempotent | call again to finish |
//!
//! No public call takes a deadline; bound a wait with `tokio::time::timeout`.
//!
//! # Errors
//!
//! [`Error`] is the facade's: [`Error::Transport`] carries an
//! [`iroh_moq::Error`] (a peer unreachable, a path unresolved or unpublishable),
//! and [`Error::Media`] an [`iroh_live_media::Error`] (a device, a codec, a
//! broadcast's tracks). Rooms carry the transport's error the same way, as
//! `iroh_rooms::Error::Transport`.
//!
//! # Features
//!
//! `capture`, `render` and `sound-server` are on by default; `playback`,
//! `aec`, `pipewire`, `vaapi`, `nvidia`, `v4l2` and `rpicam` add devices and
//! codecs, and forward to [`media`]. `rooms` re-exports `iroh-rooms`,
//! `relay-links` enables `Moq::attach_relay`, and `auth` enables
//! `Grant::from_claims`. `test-util` exposes the player's adaptation timers
//! for tests, as `media::test_util`; no application should enable it.

mod error;
mod live;
mod network;
mod ticket;

pub use iroh_live_media as media;
pub use iroh_live_media::{
    AudioOutput, AudioSource, Latency, LocalBroadcast, Player, PlayerConfig, RemoteBroadcast,
    RenditionMode, VideoSource,
};
pub use iroh_moq as moq;
pub use iroh_moq::{
    Audience, EndpointOptions, Mdns, MediaPreset, Moq, MoqConfig, Publication, Reach, Session,
    Subscription,
};
#[cfg(feature = "rooms")]
pub use iroh_rooms as rooms;

pub use self::{
    error::Error,
    live::{Live, LiveBuilder, grant, moq_config, publish_scope},
    ticket::BroadcastTicket,
};
