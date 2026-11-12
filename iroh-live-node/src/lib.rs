//! MoQ over iroh.
//!
//! A [`Node`] publishes broadcasts at paths and subscribes to paths. It
//! keeps one route table fed by every link it has: direct sessions with peers,
//! and moq relays it is attached to. A path resolves to its cheapest route,
//! and moq moves a subscription to another route of the same instance when
//! that route dies.
//!
//! ```no_run
//! use iroh::EndpointId;
//! use iroh_live_node::{Audience, MoqPreset, Node, NodeConfig, Reach};
//!
//! # async fn run(peer: EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
//! let moq = Node::new(endpoint, NodeConfig::default());
//!
//! // Publish a broadcast this process writes, created in the route table.
//! let publication = moq.publish("demo/studio", Audience::Granted)?;
//! let _track = publication.broadcast().create_track("video", None)?;
//!
//! // Resolve a peer's broadcast, dialing the peer if no route exists yet.
//! let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
//! let remote = subscription.broadcast();
//! # drop(remote);
//! # Ok(())
//! # }
//! ```
//!
//! The main types:
//!
//! - [`Node`]: the node. Mount it on a [`Router`](iroh::protocol::Router) with
//!   [`Node::mount`].
//! - [`Publication`]: a published broadcast. Its [`Audience`] says who sees it.
//!   [`Publication::unpublish_on_drop`] scopes it to a guard.
//! - [`Subscription`]: a path resolved in the route table.
//! - [`Session`]: a direct session with a peer. Its [`Grant`] says what the
//!   peer may subscribe to and publish.
//! - [`RelayLink`]: a moq relay the node stays attached to.
//!
//! Every link runs a connection monitor, and [`Subscription::serving_link`]
//! returns its latest [`NetworkSample`]. [`Session::moq`] gives the moq-net
//! session itself, with its raw statistics, and [`Session::connection`] the
//! iroh connection, with its paths.
//!
//! # Trust and grants
//!
//! Every link serves only this node's own publications, never a route it
//! learned from another link. A peer sees the exact paths of the publications
//! whose audience admits it, within its [`Grant`]. The node re-authorizes every
//! session when a publication or an audience changes, so a peer taken out of
//! an audience loses the path at once, and what it read there ends with
//! [`moq_net::Error::Unauthorized`].
//!
//! A [`policy`] decides who may hold a session and with which grant, on
//! accept and on dial (see [`NodeConfig::policy`]). The node asks it again
//! when an authorization expires, and on [`Node::reauthorize`]. [`Grant::publish_under`] lets a peer publish only
//! at paths that name it, so it cannot stand in for another peer.
//!
//! [`Node::subscribe_ticket`] claims a [`BroadcastTicket`]'s path for its peer
//! with [`Node::claim_path`], which widens the peer's grant to exactly that
//! path while the subscription lives. The peer has to own the path (see
//! [`NodeConfig::layout`]), so a claim never overlaps another peer's paths.
//!
//! The grants also keep one publisher from taking over another's readers. moq
//! moves a reader between routes that claim the same instance, so a peer or
//! relay that announces another publisher's instance could take over its
//! readers. The table's contents prevent it:
//!
//! - A direct peer announces only paths its grant lets it publish.
//! - An instance is a random string that only reaches the links whose grant
//!   lets them see the publication.
//! - A relay is trusted with the paths it forwards. Attach only relays that
//!   keep each publisher to its own paths, as moq-relay's tokens do.
//!
//! # Cancellation safety
//!
//! | Future | Safe | When dropped |
//! |---|---|---|
//! | [`Node::connect`], [`Node::connect_with`], [`Node::subscribe`], [`Node::subscribe_ticket`], [`Session::subscribe`] | yes | a dial it started continues for other callers |
//! | [`policy::Queue::next`] | yes | loses nothing |
//! | [`RelayLink::detach`] | yes | the link's task finishes the close |
//! | [`Publication::withdrawn`], [`Subscription::closed`], [`Session::closed`] | yes | loses nothing |
//! | [`Node::closed`] | yes | loses nothing |
//! | `next()` on [`Node::accepted`] | yes | loses nothing |
//! | [`Node::shutdown`] | no | call it again to finish |

mod actor;
mod admission;
mod claim;
mod error;
mod handshake;
mod link;
mod node;
pub mod policy;
mod publish;
mod reauth;
mod relay;
mod route;
mod session;
mod state;
// Helpers for this workspace's tests, not part of the public API.
#[cfg(feature = "test-util")]
#[doc(hidden)]
pub mod test_util;
mod ticket;

pub use moq_ext_access::{Direction, Layout, LinkKind};
/// Typed JSON tracks, as [`Node::publish_value`] and [`Subscription::value_track`] use them.
pub use moq_json as json;
/// The moq-net this crate builds against.
pub use moq_net;
use moq_transport_iroh as transport;
pub use moq_transport_iroh::{
    ALPN, Congestion, HANDSHAKE_TIMEOUT, MoqPreset, NetworkSample, alpns,
};

pub use self::{
    admission::{ConnectOptions, Grant, SessionRequest},
    claim::PathClaim,
    error::Error,
    link::{LinkId, ServingLink},
    node::{DECIDE_TIMEOUT, Node, NodeConfig, Reach},
    publish::{Audience, Publication, UnpublishOnDrop},
    relay::{RelayConfig, RelayLink, RelayStatus, TokenFn},
    route::Subscription,
    session::Session,
    ticket::BroadcastTicket,
};
