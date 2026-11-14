//! MoQ sessions over iroh connections.
//!
//! Everything needed to turn an iroh connection into a [`moq_net::Session`],
//! and nothing about nodes, routes or publications:
//!
//! - [`Transport`] implements moq-net's poll transport on an iroh connection.
//!   [`Transport::connect`] dials a peer, and [`Transport::new`] wraps a
//!   connection that is already open, accepted or dialed. Both check the
//!   negotiated ALPN. Hand the transport to [`moq_net::Client::connect`] or
//!   [`moq_net::Server::accept_request`].
//! - [`SendGroup`] is moq-net's send group on iroh: it packs a group's
//!   priority and its streams' orders into iroh's stream urgency, so
//!   equal-priority subscriptions share the bytes.
//! - [`ALPN`], [`alpns`] and [`HANDSHAKE_TIMEOUT`] are the protocol
//!   identifiers to dial and mount a handler with, and the bound on a
//!   session's setup.
//!
//! MoQ runs on raw QUIC. Raw QUIC has no request URI, so the request path and
//! any token travel in the MoQ SETUP.
//!
//! ```no_run
//! use iroh::EndpointId;
//! use moq_transport_iroh::Transport;
//!
//! # async fn run(peer: EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
//! let transport = Transport::connect(&endpoint, peer).await?;
//! let (session, driver) = moq_net::Client::new()
//!     .connect(moq_net::time::Instant::now(), transport)
//!     .await?;
//! # drop((session, driver));
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod error;
mod group;
mod stream;
mod transport;

use std::time::Duration;

pub use self::{
    error::{Error, TransportError},
    group::SendGroup,
    stream::{RecvStream, SendStream},
    transport::Transport,
};

/// How long a connection may take to open its MoQ session, dialed or accepted.
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
