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
//! let publication = moq.publish("studio", &broadcast, Audience::Everyone)?;
//! println!("share {}", publication.ticket().expect("a live path"));
//!
//! // Resolve someone else's, dialing its publisher if no route exists yet.
//! let subscription = moq.subscribe(ticket.path(), Reach::default()).await?;
//! let remote = subscription.as_moq();
//! # drop(remote);
//! # Ok(())
//! # }
//! ```
//!
//! # Cancellation safety
//!
//! [`Moq::connect`], [`Moq::subscribe`], [`Moq::accept`], [`Incoming::admit`],
//! [`Session::subscribe`] and every `closed()` are cancellation safe: dropping
//! the future abandons the wait, and a dial it started continues for other
//! callers. [`Moq::shutdown`] is not, and is idempotent: call it again to
//! finish.

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

/// The moq-net this crate builds against.
///
/// An application that needs moq-net's types uses these, and never has to
/// match a second dependency's version to ours.
pub use moq_net as net;

#[cfg(feature = "relay-links")]
pub use self::relay::{DEFAULT_RELAY_COST, LinkStatus, RelayConfig, RelayLink, RelayOffer};
pub use self::{
    endpoint::{EndpointOptions, Mdns, MediaPreset},
    error::Error,
    grant::{Admission, ConnectOptions, Grant, Reject, Role, SessionRequest},
    link::LinkSample,
    node::{Moq, MoqConfig, Reach},
    path::{live_path, publisher_of},
    publish::{Audience, OfferGuard, Publication},
    route::{LinkId, LinkKind, RouteInfo, Subscription},
    session::{Incoming, Session, SessionStats, dial},
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
