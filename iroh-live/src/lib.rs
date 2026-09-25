//! Live audio and video over iroh.
//!
//! A [`Live`] node binds the [`Moq`] transport to an iroh endpoint. It
//! publishes broadcasts at `live/<its endpoint id>/<name>` and subscribes to
//! other nodes' broadcasts over a direct session or an attached relay. The
//! transport is re-exported as [`moq`], the media crate as `media` and the
//! rooms crate as `rooms`, each with its feature.
#![cfg_attr(
    feature = "media",
    doc = r#"
This publishes a test pattern and plays someone else's broadcast:

```no_run
use iroh_live::{
    BroadcastTicket, Live, LocalBroadcast, MoqPreset, PlayerConfig, VideoSource,
    media::{VideoEncoding, VideoRendition, video},
};

# async fn run(ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
let live = Live::builder(endpoint).with_router().spawn();

let broadcast = LocalBroadcast::new();
let source = VideoSource::test_pattern(
    video::Size::new(1280, 720),
    video::Rate::new(30, 1).expect("a valid rate"),
);
broadcast.set_video(
    source,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
live.publish("studio", &broadcast)?;
println!("share {}", live.ticket("studio"));

let subscription = live.subscribe(&ticket).await?;
let player = live
    .remote_broadcast(&subscription)
    .play(PlayerConfig::default())?;
let mut frames = player.video();
while let Some(frame) = frames.next().await {
    // hand `frame` to a renderer
#   drop(frame);
}
# Ok(())
# }
```
"#
)]
//!
//! The main types:
//!
//! - [`Live`]: the node. [`LiveBuilder`] mounts more protocols on its router.
//! - [`BroadcastTicket`]: a publisher's endpoint id and a broadcast name.
#![cfg_attr(
    feature = "media",
    doc = "- [`LocalBroadcast`] and [`RemoteBroadcast`], from [`media`]."
)]
//!
//! # Cancellation safety
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | [`EndpointOptions::bind`] | yes | nothing is bound |
//! | [`Live::subscribe`] | yes | abandons the wait; a dial it started continues for other callers |
//! | [`Live::shutdown`] | no, idempotent | call again to finish |
//!
//! `media`, [`moq`] and `rooms` list their own futures.
//! No public call takes a deadline. Bound a wait with `tokio::time::timeout`.
//!
//! # Features
//!
//! `media`, `capture`, `render` and `sound-server` are on by default. `media`
//! adds the `media` module and `Live::remote_broadcast`. Without it the crate
//! is the `live/` layout only, as the relay uses it. `playback`, `aec`,
//! `pipewire`, `vaapi`, `nvidia`, `v4l2` and `rpicam` add devices and codecs,
//! and imply `media`. `rooms` re-exports `iroh-live-rooms`.

mod endpoint;
mod error;
mod live;
#[cfg(feature = "media")]
mod network;
mod ticket;

#[cfg(feature = "media")]
pub use iroh_live_media as media;
#[cfg(feature = "media")]
pub use iroh_live_media::{
    AudioOutput, AudioSource, Latency, LocalBroadcast, Player, PlayerConfig, RemoteBroadcast,
    RenditionMode, VideoSource,
};
#[cfg(feature = "rooms")]
pub use iroh_live_rooms as rooms;
pub use iroh_moq as moq;
pub use iroh_moq::{
    Audience, Moq, MoqConfig, MoqPreset, Publication, Reach, Session, Subscription,
};

pub use self::{
    endpoint::{EndpointOptions, Mdns, secret_key_file},
    error::Error,
    live::{Live, LiveBuilder, grant, moq_config, publish_scope},
    ticket::{BroadcastTicket, CALL},
};
