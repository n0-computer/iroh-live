//! MoQ sessions over iroh connections.
//!
//! Everything needed to turn an iroh connection into a [`moq_net::Session`],
//! and nothing about nodes, routes or publications:
//!
//! - [`dial`] and [`accept`] negotiate raw QUIC or HTTP/3 by ALPN and complete
//!   the WebTransport handshake.
//! - [`poll_session`] wraps the result for moq-net's client or server.
//! - [`MoqPreset`] binds an endpoint with a QUIC configuration tuned for MoQ.
//! - [`LinkSampler`] and [`LinkSample`] read a connection's selected path:
//!   round trip, loss, delivery rate.
//!
//! ```no_run
//! use iroh::EndpointId;
//! use moq_transport_iroh::{MoqPreset, poll_session};
//!
//! # async fn run(peer: EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
//! let session = moq_transport_iroh::dial(&endpoint, peer).await?;
//! // Hand `poll_session(session, false)` to `moq_net::Client::connect`.
//! # drop(poll_session(session, false));
//! # Ok(())
//! # }
//! ```

mod adapter;
mod error;
mod link;
mod preset;
mod transport;

use std::time::Duration;

/// The poll transport adapter sessions run over, for tests that drive
/// moq-net directly.
#[doc(hidden)]
pub use adapter::Session as AdaptedTransport;

pub use self::{
    error::Error,
    link::{LinkSample, LinkSampler, SAMPLE_INTERVAL, watch},
    preset::MoqPreset,
    transport::{DialOptions, Dialed, accept, accept_with_headers, dial, dial_with, poll_session},
};

/// How long an incoming connection may take to open its MoQ session.
///
/// Without a bound, a peer that never sends its setup holds a task for as long
/// as the connection lives.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The newest MoQ version this build speaks.
///
/// A dial offers every ALPN in [`alpns`], so a peer on an older version still
/// connects.
pub const ALPN: &[u8] = moq_net::ALPNS[0].as_bytes();

/// Returns every ALPN a server accepts, newest MoQ version first, then HTTP/3.
///
/// Mount a protocol handler under all of them.
#[must_use]
pub fn alpns() -> Vec<&'static [u8]> {
    moq_net::ALPNS
        .iter()
        .map(|alpn| alpn.as_bytes())
        .chain(std::iter::once(web_transport_iroh::ALPN_H3.as_bytes()))
        .collect()
}
