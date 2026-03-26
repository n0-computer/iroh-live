//! High-level API for live audio/video sessions over iroh.
//!
//! Provides [`Live`] as the main entry point for publishing and subscribing
//! to media broadcasts, [`Call`] for 1:1 call sugar, and [`rooms`] for
//! multi-party rooms with gossip-based coordination.

mod call;
mod live;
/// Multi-party room coordination over gossip.
pub mod rooms;
mod subscription;
/// Ticket types for sharing connection info.
pub mod ticket;
mod types;
/// Utility helpers (stats smoothing, etc.).
pub mod util;

pub use hang::catalog;
pub use iroh_moq as moq;
pub use iroh_moq::ALPN;
pub use moq_media as media;

pub use self::{
    call::{Call, CallError},
    live::{Live, LiveBuilder},
    subscription::Subscription,
    types::DisconnectReason,
};
