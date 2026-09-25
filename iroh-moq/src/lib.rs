//! MoQ over iroh.
//!
//! A [`Moq`] node publishes broadcasts at paths and subscribes to paths. It
//! keeps one route table fed by every link it has: direct sessions with peers,
//! and moq relays it is attached to. A path resolves to its cheapest route.
//! When that route dies, moq moves to another route from the same first hop,
//! and [`Subscription::closed`] asks the table again for any other. The
//! application picks the paths.
//!
//! ```no_run
//! use iroh::EndpointId;
//! use iroh_moq::{Audience, Moq, MoqConfig, MoqPreset, Reach};
//!
//! # async fn run(peer: EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
//! let moq = Moq::new(endpoint, MoqConfig::default());
//!
//! // Publish a broadcast this process writes.
//! let broadcast = moq_net::broadcast::Info::new().produce();
//! let _publication = moq.publish("demo/studio", &broadcast, Audience::Everyone)?;
//!
//! // Resolve a peer's broadcast, dialing the peer if no route exists yet.
//! let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
//! let remote = subscription.as_moq();
//! # drop(remote);
//! # Ok(())
//! # }
//! ```
//!
//! The main types:
//!
//! - [`Moq`]: the node. Mount it on a [`Router`](iroh::protocol::Router) with
//!   [`Moq::mount`].
//! - [`Publication`]: a published broadcast. Its [`Audience`] says who sees it.
//! - [`Subscription`]: a path resolved in the route table.
//! - [`Session`]: a direct session with a peer. Its [`Grant`] says what the
//!   peer may subscribe to and publish.
//! - [`RelayLink`]: a moq relay the node stays attached to.
//!
//! A publication is offered on a session when both its audience and the
//! session's grant allow it. [`MoqConfig::grant`] gives each peer its grant
//! from its endpoint id. Without it, any peer may publish at any path, and so
//! stand in for another peer. [`Grant::publish_under`] keeps a peer to the
//! paths that name it.
//!
//! Every link runs a connection monitor. [`Session::link`],
//! [`RelayLink::link`] and [`Subscription::link`] return its latest
//! [`LinkSample`].
//!
//! # Cancellation safety
//!
//! | Future | Safe | When dropped |
//! |---|---|---|
//! | [`Moq::connect`], [`Moq::connect_with`], [`Moq::subscribe`], [`Session::subscribe`] | yes | a dial it started continues for other callers |
//! | [`Moq::accept`] | yes | loses nothing |
//! | [`Incoming::admit`] | yes | rejects the session before the handshake completes, admits it after |
//! | [`transport::dial`], [`transport::accept`] | yes | drops the connection |
//! | [`RelayLink::detach`] | yes | the link's task finishes the close |
//! | [`Publication::withdrawn`], [`Subscription::closed`], [`Session::closed`] | yes | loses nothing |
//! | [`Moq::shutdown`] | no | call it again to finish |

mod admission;
mod error;
mod link;
mod node;
mod preset;
mod publish;
mod relay;
mod route;
mod session;
mod state;
pub mod transport;

/// The moq-net this crate builds against.
pub use moq_net as net;

pub use self::{
    admission::{
        Admission, ConnectOptions, Grant, GrantFn, HANDSHAKE_TIMEOUT, Incoming, SessionRequest,
    },
    error::Error,
    link::{LinkSample, ServingLink},
    node::{Moq, MoqConfig, Reach},
    preset::MoqPreset,
    publish::{Audience, OfferGuard, Publication},
    relay::{DEFAULT_RELAY_COST, RelayConfig, RelayLink, RelayOffer, RelayStatus},
    route::{LinkId, LinkKind, Subscription},
    session::Session,
};

/// The newest MoQ version this build speaks.
///
/// A dial offers every ALPN in [`alpns`], so a peer on an older version still
/// connects.
pub const ALPN: &[u8] = moq_net::ALPNS[0].as_bytes();

/// Returns every ALPN a node accepts, newest MoQ version first, then HTTP/3.
///
/// [`Moq::mount`] mounts a node under all of them.
pub fn alpns() -> Vec<&'static [u8]> {
    moq_net::ALPNS
        .iter()
        .map(|alpn| alpn.as_bytes())
        .chain(std::iter::once(web_transport_iroh::ALPN_H3.as_bytes()))
        .collect()
}
