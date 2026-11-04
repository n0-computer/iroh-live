//! MoQ sessions over iroh connections.
//!
//! Everything needed to turn an iroh connection into a [`moq_net::Session`],
//! and nothing about nodes, routes or publications:
//!
//! - [`dial`] and [`accept`] check the negotiated ALPN and return a
//!   [`Transport`], which implements moq-net's poll transport on the iroh
//!   connection. Hand it to [`moq_net::Client::connect`] or
//!   [`moq_net::Server::accept_request`].
//! - [`MoqPreset`] binds an endpoint with a QUIC configuration tuned for MoQ.
//! - [`LinkSampler`] and [`LinkSample`] read a connection's selected path:
//!   round trip, loss, delivery rate.
//!
//! MoQ runs on raw QUIC. Raw QUIC has no request URI, so the request path and
//! any token travel in the MoQ SETUP.
//!
//! ```no_run
//! use iroh::EndpointId;
//! use moq_transport_iroh::MoqPreset;
//!
//! # async fn run(peer: EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
//! let transport = moq_transport_iroh::dial(&endpoint, peer).await?;
//! let (session, driver) = moq_net::Client::new()
//!     .connect(moq_net::time::Instant::now(), transport)
//!     .await?;
//! # drop((session, driver));
//! # Ok(())
//! # }
//! ```

mod error;
mod link;
mod preset;
mod stream;
mod transport;

use std::time::Duration;

pub use self::{
    error::{Error, TransportError},
    link::{LinkSample, LinkSampler, SAMPLE_INTERVAL, watch},
    preset::MoqPreset,
    stream::{RecvStream, SendStream},
    transport::{Transport, accept, dial},
};

/// How long an incoming connection may take to open its MoQ session.
///
/// Without a bound, a peer that never sends its setup holds a task for as long
/// as the connection lives.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The newest MoQ version this build speaks.
///
/// A dial offers only this ALPN. A server accepts every ALPN in [`alpns`], so
/// a peer that offers an older version still connects.
pub const ALPN: &[u8] = moq_net::ALPNS[0].as_bytes();

/// Returns every ALPN a server accepts, newest MoQ version first.
///
/// Mount a protocol handler under all of them.
#[must_use]
pub fn alpns() -> Vec<&'static [u8]> {
    moq_net::ALPNS.iter().map(|alpn| alpn.as_bytes()).collect()
}
