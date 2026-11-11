//! Live audio and video over iroh.
//!
//! A [`Node`] publishes and subscribes to broadcasts at paths, over a direct
//! session with a peer or an attached relay. [`node`] re-exports the
//! transport and [`net`] the moq-net under it. `media` and `rooms` gather the
//! media and rooms crates, each behind its feature.
//!
//! The moq crates are pinned to a git rev of moq-dev/moq `main`, so this crate
//! is not on crates.io yet.
//!
//! # Paths and layouts
//!
//! The node takes any path. iroh-live ships one convention,
//! [`layout::LiveLayout`], which publishes a peer's broadcasts at
//! `<root>/<peer>/<name>` (the root is `live` by default) and lets each peer
//! publish only under its own id. An application with its own scheme passes
//! its own paths and policy, see [`NodeConfig::with_policy`].
//!
//! A [`BroadcastTicket`] names a peer and the path it publishes at, whatever
//! the layout. [`Node::subscribe_ticket`] grants the ticket's publisher that
//! one path while the subscription lives, so a viewer needs no layout.
#![cfg_attr(
    feature = "media",
    doc = r#"
This publishes a test pattern and plays someone else's broadcast:

```no_run
use iroh::protocol::Router;
use iroh_live::{
    BroadcastTicket, EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        LocalBroadcast, PlayerConfig, VideoEncoding, VideoRendition, VideoSource,
        remote_broadcast, video,
    },
};

# async fn run(ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
let layout = LiveLayout::default();
let endpoint = EndpointConfig::from_env()?.bind().await?;
let node = Node::new(endpoint.clone(), layout.config());
// Accepts the peers that read the broadcast. Keep the router alive.
let router = node.mount(Router::builder(endpoint)).spawn();

// Created in the node's route table: peers read it without a copy.
let broadcast: LocalBroadcast = layout.publish(&node, "studio")?;
let source = VideoSource::test_pattern(
    video::Size::new(1280, 720),
    video::FrameRate::new(30, 1).expect("a valid rate"),
)?;
broadcast.set_video(
    source,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
println!("share {}", layout.ticket(node.id(), "studio")?);

let subscription = node.subscribe_ticket(&ticket).await?;
let player = remote_broadcast(&subscription).play(PlayerConfig::default())?;
let mut frames = player.video();
while let Some(frame) = frames.next().await {
    // hand `frame` to a renderer
#   drop(frame);
}

// Shuts the node down and every mounted service, then closes the endpoint.
router.shutdown().await?;
# Ok(())
# }
```
"#
)]
//!
//! [`EndpointConfig::apply`] composes with your own endpoint builder, and
//! [`calls::Calls`] places and answers one-to-one calls over a layout.
//!
//! The main types:
//!
//! - [`Node`]: the node, mounted on the application's router with
//!   [`Node::mount`].
//! - [`layout::LiveLayout`]: the `<root>/<peer>/<name>` path convention.
//! - [`BroadcastTicket`]: a publisher's endpoint id and the path of a broadcast.
//! - [`calls::Calls`]: one-to-one calls.
#![cfg_attr(
    feature = "media",
    doc = "- [`LocalBroadcast`](media::LocalBroadcast) and [`RemoteBroadcast`](media::RemoteBroadcast), from [`media`]."
)]
//!
//! # Cancellation safety
//!
//! | Future | Safe | Dropping it |
//! |---|---|---|
//! | [`EndpointConfig::bind`] | yes | nothing is bound |
//! | [`Node::subscribe`], [`Node::subscribe_ticket`] | yes | abandons the wait, and a dial it started continues for other callers |
//! | [`calls::Calls::call`], [`calls::Calls::answer`] | yes | hangs up |
//! | [`EndpointConfig::apply`] | yes | the builder is lost, nothing is bound |
//! | [`Node::shutdown`] | no, idempotent | call again to finish |
//!
//! [`Node::shutdown`] does not close the endpoint. Whoever binds it closes
//! it, directly or through `Router::shutdown`.
//!
//! `media`, [`node`] and `rooms` list their own futures.
//! No public call takes a deadline. Bound a wait with `tokio::time::timeout`.
//!
//! # Features
//!
//! No feature is on by default. `full` turns on `media`, `capture`, `render`,
//! `record` and `sound-server`. `media` adds the `media` module. Without it
//! the crate is the layout, calls and endpoint setup only. `playback`, `aec`,
//! `pipewire`, `vaapi`, `nvidia`, `v4l2` and `rpicam` add devices and codecs,
//! and imply `media`. `rooms` re-exports `iroh-live-rooms`.

mod app;
pub mod calls;
mod endpoint;
mod error;
pub mod layout;
#[cfg(feature = "media")]
mod network;

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
    pub use moq_net::bandwidth::Rate;
    pub use moq_video as video;

    pub use crate::network::remote_broadcast;
}
pub use iroh_live_node as node;
/// The moq-net this crate builds against, as [`node::net`].
pub use iroh_live_node::net;
pub use iroh_live_node::{
    Audience, BroadcastTicket, Congestion, ConnectOptions, MoqPreset, Node, NodeConfig,
    Publication, Reach, RelayConfig, Session, Subscription, TokenFn,
};
#[cfg(feature = "rooms")]
pub use iroh_live_rooms as rooms;
/// Typed JSON tracks over moq-net, including a latest-value track.
///
/// [`snapshot`](json::snapshot) is the latest-value track, for state such
/// as a setting. [`Consumer::latest`](json::snapshot::Consumer::latest) skips
/// to the newest value. A track keeps its newest group, so a late reader gets
/// the current value at once without the writer repeating it.
/// [`stream`](json::stream) is the lossless log, for records that must all
/// arrive.
///
/// This publishes a latency in milliseconds and reads it back:
///
/// ```no_run
/// use iroh_live::{Audience, Node, Reach, json::snapshot, layout::LiveLayout, net::track};
///
/// # async fn run(node: Node, other: Node) -> Result<(), Box<dyn std::error::Error>> {
/// let path = LiveLayout::default().path(node.id(), "control")?;
/// let (_publication, track) = node.publish_with(path.clone(), Audience::Granted, |producer| {
///     producer.create_track("latency", track::Info::default())
/// })?;
/// let mut latency = snapshot::Producer::<u64>::new(track, snapshot::Config::default());
/// latency.update(&150)?;
///
/// // On another node.
/// let subscription = other.subscribe(&path, Reach::Direct(node.id())).await?;
/// let track = subscription.broadcast().track("latency")?;
/// let mut latency = snapshot::Consumer::<u64>::new(track, Default::default());
/// while let Some(newest) = latency.latest().await? {
///     println!("latency {} ms", newest.value);
/// }
/// # Ok(())
/// # }
/// ```
pub use moq_json as json;

pub use self::{
    app::BoundNode,
    endpoint::{EndpointConfig, Mdns, secret_key_file},
    error::Error,
};
