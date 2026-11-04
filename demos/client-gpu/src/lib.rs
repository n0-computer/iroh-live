//! Shared pieces of the client-gpu latency demo.
//!
//! The `client` binary publishes video to the `gpu` binary, which filters it
//! and publishes it back. Both sides share the frame-id stamp that correlates
//! a returned picture with the one the client sent, the filters, the watermark
//! and the control track.

pub mod control;
pub mod filter;
pub mod i420;
pub mod latency;
pub mod pipeline;
pub mod session;
pub mod stamp;
pub mod watermark;

use iroh::EndpointId;
use iroh_live::{Live, Publication};

/// A call offer that is withdrawn when dropped.
#[derive(Debug)]
pub struct Offer(Publication);

impl Offer {
    /// Offers `broadcast` as this node's side of a call with `peer`.
    ///
    /// # Errors
    ///
    /// Fails if a call offer is already published.
    pub fn new(
        live: &Live,
        peer: EndpointId,
        broadcast: impl moq_net::Consume<moq_net::broadcast::Consumer>,
    ) -> Result<Self, iroh_live::Error> {
        Ok(Self(live.call(peer, broadcast)?))
    }
}

impl Drop for Offer {
    fn drop(&mut self) {
        self.0.unpublish();
    }
}
