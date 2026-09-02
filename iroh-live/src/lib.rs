//! Live audio and video over iroh.
//!
//! [`Live`] binds an iroh [`Endpoint`](iroh::Endpoint) to a MoQ transport:
//! [`Live::publish`] hands back a broadcast every connected peer can subscribe
//! to, and [`Live::subscribe`] reaches one a peer publishes. [`Call`] is 1:1
//! sugar over the two.
//!
//! The media itself comes from [`moq_media`], which is plumbing over upstream
//! `moq-video` and `moq-audio`. Rooms live in the separate `iroh-rooms` crate;
//! [`Live::gossip`] is what they need from here.

mod call;
mod live;
mod subscription;
pub mod ticket;
mod types;
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
