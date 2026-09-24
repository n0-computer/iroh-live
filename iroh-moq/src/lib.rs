//! MoQ transport over iroh.
//!
//! A [`Moq`] node keeps one route table fed by every link it has: direct
//! sessions with peers, and relays it is attached to. Broadcasts are published
//! at paths that name their publisher (`live/<endpoint id>/<name>`), so the
//! same broadcast has the same path over every link, and subscribing is
//! resolving a path in the table rather than asking one session.
//!
//! Two settings decide what flows over a session. A publication's [`Audience`]
//! says who may see it, and a session's [`Grant`], decided when the session is
//! admitted, says what the peer may subscribe to and publish. A publication is
//! offered on a session when both allow it.
//!
//! ```no_run
//! use iroh_moq::{Audience, BroadcastTicket, MediaPreset, Moq, MoqConfig, Reach};
//!
//! # async fn run(ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MediaPreset).await?;
//! let moq = Moq::new(endpoint, MoqConfig::default());
//!
//! // Publish a broadcast this process writes.
//! let broadcast = moq_net::broadcast::Info::new().produce();
//! let _publication = moq.publish("studio", &broadcast, Audience::Everyone)?;
//! println!("share {}", moq.ticket("studio"));
//!
//! // Resolve someone else's, dialing its publisher if no route exists yet.
//! let subscription = moq
//!     .subscribe(ticket.path(), Reach::Both(ticket.peer()))
//!     .await?;
//! let remote = subscription.as_moq();
//! # drop(remote);
//! # Ok(())
//! # }
//! ```
//!
//! # Links
//!
//! A link is a direct [`Session`] with a peer or a relay link (`RelayLink`,
//! behind the `relay-links` feature). Every link runs a connection monitor, and
//! [`Session::link`], `RelayLink::link` and [`Subscription::link`] return its
//! latest [`LinkSample`]: round trip, loss, arriving goodput and the peer's
//! delivery estimate, with `None` for whatever is not measured yet. It traces
//! each reading at `trace` level as `link sample` or `relay link sample`.
//!
//! A relay attached with `RelayConfig::new` also consumes: every route the
//! relay announces enters this node's route table, priced at the relay's cost.
//! A node that only publishes through a relay turns that off with
//! `RelayConfig::with_consume(false)`.
//!
//! # Cancellation safety
//!
//! [`Moq::connect`], [`Moq::connect_with`], [`Moq::subscribe`] and
//! [`Session::subscribe`] are cancellation safe: dropping the future abandons
//! the wait, and a dial it started continues for other callers.
//! [`Moq::accept`] loses nothing when dropped, and [`Incoming::admit`] rejects
//! the session if dropped before the handshake completes and admits it all the
//! same after. [`EndpointOptions::bind`] and [`EndpointOptions::builder`] bind
//! nothing when dropped, and [`transport::dial`] and [`transport::accept`] drop
//! the connection they were setting up. `RelayLink::detach` signals the close
//! before its first wait, so dropping it leaves the rest to the link's task.
//! [`Publication::withdrawn`], [`Subscription::closed`] and [`Session::closed`]
//! lose nothing. [`Moq::shutdown`] is not cancellation safe, and is
//! idempotent: call it again to finish.

mod endpoint;
mod error;
mod grant;
mod link;
mod node;
mod path;
mod publish;
#[cfg(feature = "relay-links")]
mod relay;
mod route;
mod session;
mod state;
mod ticket;
pub mod transport;

/// The moq-net this crate builds against.
///
/// An application that needs moq-net's types uses these, and never has to
/// match a second dependency's version to ours.
pub use moq_net as net;

#[cfg(feature = "relay-links")]
pub use self::relay::{DEFAULT_RELAY_COST, RelayConfig, RelayLink, RelayOffer, RelayStatus};
pub use self::{
    endpoint::{EndpointOptions, Mdns, MediaPreset},
    error::Error,
    grant::{Admission, ConnectOptions, Grant, Reject, Role, SessionRequest},
    link::{LinkSample, ServingLink},
    node::{Moq, MoqConfig, Reach},
    path::{live_path, publisher_of},
    publish::{Audience, OfferGuard, Publication},
    route::{LinkId, LinkKind, RouteInfo, Subscription},
    session::{Incoming, Session},
    ticket::BroadcastTicket,
};

/// The ALPN this node prefers, the newest MoQ version it speaks.
///
/// A peer that only speaks an older one still connects: [`alpns`] carries the
/// whole list, and both the dial and the router registration offer all of it.
pub const ALPN: &[u8] = moq_net::ALPNS[0].as_bytes();

/// Returns every ALPN this node accepts, newest first, then HTTP/3.
///
/// Mount a [`Moq`] under all of them so a peer built against a different moq
/// release still finds a version in common. HTTP/3 is last because
/// WebTransport over H3 needs framing not every H3 endpoint supports.
pub fn alpns() -> Vec<&'static [u8]> {
    moq_net::ALPNS
        .iter()
        .map(|alpn| alpn.as_bytes())
        .chain(std::iter::once(web_transport_iroh::ALPN_H3.as_bytes()))
        .collect()
}
