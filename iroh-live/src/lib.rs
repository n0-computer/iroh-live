//! High-level API for live audio/video sessions over iroh.
//!
//! Provides [`Live`] as the main entry point for publishing and subscribing
//! to media broadcasts, [`Call`] for 1:1 call sugar, and [`rooms`] for
//! multi-party rooms with gossip-based coordination.

mod call;
mod live;
mod publisher;
/// Client-side helpers for addressing an iroh-live-relay.
pub mod relay;
/// Multi-party room coordination over gossip.
pub mod rooms;
/// Frame-level seamless track swap on top of [`Subscription`].
pub mod seamless;
/// Transport source abstractions for multi-origin publish and subscribe.
pub mod sources;
mod subscription;
/// Ticket types for sharing connection info.
pub mod ticket;
mod types;
/// Utility helpers (stats smoothing, network signal production).
pub mod util;

pub use hang::catalog;
pub use iroh_moq as moq;
pub use iroh_moq::ALPN;
pub use moq_media as media;

pub use self::{
    call::{Call, CallError},
    live::{Live, LiveBuilder},
    publisher::Broadcaster,
    sources::{
        Candidate, DirectSource, Pinned, PreferOrdered, RelayOffer, SelectionPolicy, SourceId,
        SourceSet, SourceSetHandle, TransportSource,
    },
    subscription::{ActiveSource, Subscription, SubscriptionEvent, SubscriptionEvents},
    types::DisconnectReason,
};
