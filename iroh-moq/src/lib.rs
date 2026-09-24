//! MoQ over iroh.
//!
//! A [`Moq`] node keeps one route table fed by every link it has: direct
//! sessions with peers, and relays it is attached to. The application picks
//! the paths. If a broadcast has the same path over every link, the table sees
//! every route to it, and subscribing is resolving a path in the table rather
//! than asking one session.
//!
//! Two settings decide what flows over a session. A publication's [`Audience`]
//! says who may see it, and a session's [`Grant`], decided when the session is
//! admitted, says what the peer may subscribe to and publish. A publication is
//! offered on a session when both allow it. [`MoqConfig::grant`] gives each
//! peer its grant from its endpoint id, which is how an application keeps a
//! peer to the paths that name it.
//!
//! ```no_run
//! use iroh::EndpointId;
//! use iroh_moq::{Audience, MediaPreset, Moq, MoqConfig, Reach};
//!
//! # async fn run(peer: EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MediaPreset).await?;
//! let moq = Moq::new(endpoint, MoqConfig::default());
//!
//! // Publish a broadcast this process writes.
//! let broadcast = moq_net::broadcast::Info::new().produce();
//! let _publication = moq.publish("demo/studio", &broadcast, Audience::Everyone)?;
//!
//! // Resolve a peer's broadcast, dialing it if no route exists yet.
//! let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
//! let remote = subscription.as_moq();
//! # drop(remote);
//! # Ok(())
//! # }
//! ```
//!
//! # Links
//!
//! A link is a direct [`Session`] with a peer or a [`RelayLink`]. Every link
//! runs a connection monitor, and [`Session::link`], [`RelayLink::link`] and
//! [`Subscription::link`] return its
//! latest [`LinkSample`]: round trip, loss, arriving goodput and the peer's
//! delivery estimate, with `None` for whatever is not measured yet. It traces
//! each reading at `trace` level as `link sample` or `relay link sample`.
//!
//! A relay attached with [`RelayConfig::new`] also consumes: every route the
//! relay announces enters this node's route table, priced at the relay's cost.
//! A node that only publishes through a relay sets [`RelayConfig::consume`]
//! to false.
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
//! the connection they were setting up. [`RelayLink::detach`] signals the close
//! before its first wait, so dropping it leaves the rest to the link's task.
//! [`Publication::withdrawn`], [`Subscription::closed`] and [`Session::closed`]
//! lose nothing. [`Moq::shutdown`] is not cancellation safe, and is
//! idempotent: call it again to finish.

mod admission;
mod endpoint;
mod error;
mod link;
mod node;
mod publish;
mod relay;
mod route;
mod session;
mod state;
pub mod transport;

/// The moq-net this crate builds against.
///
/// An application that needs moq-net's types uses these, and never has to
/// match a second dependency's version to ours.
pub use moq_net as net;

pub use self::{
    admission::{Admission, ConnectOptions, Grant, GrantFn, Incoming, SessionRequest},
    endpoint::{EndpointOptions, Mdns, MediaPreset},
    error::Error,
    link::{LinkSample, ServingLink},
    node::{Moq, MoqConfig, Reach},
    publish::{Audience, OfferGuard, Publication},
    relay::{DEFAULT_RELAY_COST, RelayConfig, RelayLink, RelayOffer, RelayStatus},
    route::{LinkId, LinkKind, RouteInfo, Subscription},
    session::Session,
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
