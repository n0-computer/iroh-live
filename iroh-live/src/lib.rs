//! Live audio and video over iroh.
//!
//! [`Live`] is a node ready for live media: it binds the [`Moq`] transport to an
//! iroh [`Endpoint`](iroh::Endpoint), publishes broadcasts at paths that name
//! this node, and resolves other nodes' broadcasts over whichever link serves
//! them. Everything else is re-exported: the media crate as [`media`], the
//! transport as [`moq`], and rooms as `rooms` behind the `rooms` feature, so an
//! application needs one dependency and builds against the exact versions this
//! crate does.
//!
//! ```no_run
//! use iroh_live::{Live, LocalBroadcast, MediaPreset};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MediaPreset).await?;
//! let live = Live::builder(endpoint).with_router().spawn();
//!
//! let broadcast = LocalBroadcast::new();
//! let publication = live.publish("studio", &broadcast)?;
//! println!("share {}", publication.ticket().expect("a live path"));
//! # Ok(())
//! # }
//! ```

mod error;
mod live;
mod network;

pub use hang::catalog;
pub use iroh_live_media as media;
pub use iroh_live_media::{
    AudioOutput, AudioSource, Latency, LocalBroadcast, Player, PlayerConfig, RemoteBroadcast,
    RenditionMode, VideoSource,
};
pub use iroh_moq as moq;
pub use iroh_moq::{
    Audience, BroadcastTicket, EndpointOptions, Mdns, MediaPreset, Moq, MoqConfig, Publication,
    Reach, Session, Subscription,
};
#[cfg(feature = "rooms")]
pub use iroh_rooms as rooms;

pub use self::{
    error::Error,
    live::{Live, LiveBuilder},
};
