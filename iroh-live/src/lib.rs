//! Live audio and video over iroh.
//!
//! A [`Live`] node binds the [`Moq`] transport to an iroh endpoint. It
//! publishes broadcasts at `live/<its endpoint id>/<name>` and subscribes to
//! other nodes' broadcasts over a direct session or an attached relay. The
//! transport is re-exported as [`moq`]. The `media` module gathers the media
//! crates (sources, broadcasts, players, codecs), and `rooms` re-exports the
//! rooms crate, each behind its feature.
#![cfg_attr(
    feature = "media",
    doc = r#"
This publishes a test pattern and plays someone else's broadcast:

```no_run
use iroh_live::{
    BroadcastTicket, Live, MoqPreset,
    media::{
        LocalBroadcast, PlayerConfig, VideoEncoding, VideoRendition, VideoSource, video,
    },
};

# async fn run(ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
let live = Live::builder(endpoint).with_router().spawn();

// Created in the node's route table: peers read it without a copy.
let broadcast: LocalBroadcast = live.publish("studio")?;
let source = VideoSource::test_pattern(
    video::Size::new(1280, 720),
    video::FrameRate::new(30, 1).expect("a valid rate"),
);
broadcast.set_video(
    source,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
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
//! The `live/<id>/<name>` layout, the grant and the endpoint setup are
//! defaults. [`Live::publish_at`] and [`Live::subscribe_path`] take any path,
//! [`Live::builder_with`] takes any [`MoqConfig`], [`EndpointOptions::apply`]
//! composes with your own endpoint builder, and [`Live::moq`] is the escape
//! hatch to the transport. [`Live::call`] publishes this node's side of
//! one-to-one calls, and its [`CallOffer`] rings and hangs up each peer.
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
//! | [`Live::subscribe`] | yes | abandons the wait, and a dial it started continues for other callers |
//! | [`Live::subscribe_path`] | yes | as [`Live::subscribe`] |
//! | [`Live::answer`] | yes | abandons the wait |
//! | [`EndpointOptions::apply`] | yes | the builder is lost, nothing is bound |
//! | [`Live::shutdown`] | no, idempotent | call again to finish |
//!
//! `media`, [`moq`] and `rooms` list their own futures.
//! No public call takes a deadline. Bound a wait with `tokio::time::timeout`.
//!
//! # Features
//!
//! No feature is on by default. `full` turns on `media`, `capture`, `render`,
//! `record` and `sound-server`. `media` adds the `media` module and
//! `Live::remote_broadcast`. Without it the crate is the `live/` layout only,
//! as the relay uses it. `playback`, `aec`, `pipewire`, `vaapi`, `nvidia`,
//! `v4l2` and `rpicam` add devices and codecs, and imply `media`. `rooms`
//! re-exports `iroh-live-rooms` and adds `LiveBuilder::with_rooms`.

mod endpoint;
mod error;
mod live;
#[cfg(feature = "media")]
mod network;
pub mod paths;
mod ticket;

/// Sources, broadcasts and players, with the codec stacks they sit on.
#[cfg(feature = "media")]
pub mod media {
    pub use moq_audio as audio;
    pub use moq_ext_player::*;
    pub use moq_ext_publisher::*;
    #[cfg(feature = "capture")]
    pub use moq_ext_sources::MicrophoneConfig;
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    pub use moq_ext_sources::RpicamConfig;
    pub use moq_ext_sources::{
        AudioFormat, AudioSource, EncodedVideoSource, FrameSender, VideoFormat, VideoSource,
    };
    pub use moq_video as video;
}
#[cfg(feature = "rooms")]
pub use iroh_live_rooms as rooms;
pub use iroh_moq as moq;
pub use iroh_moq::{
    Audience, Moq, MoqConfig, MoqPreset, Publication, Reach, Session, Subscription,
};

pub use self::{
    endpoint::{EndpointOptions, Mdns, secret_key_file},
    error::Error,
    live::{
        CallOffer, Live, LiveBuilder, PublishScope, grant, grant_with, moq_config, moq_config_with,
    },
    paths::CALL,
    ticket::BroadcastTicket,
};
