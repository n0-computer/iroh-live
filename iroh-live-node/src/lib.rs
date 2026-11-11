//! MoQ over iroh.
//!
//! A [`Node`] publishes broadcasts at paths and subscribes to paths. It
//! keeps one route table fed by every link it has: direct sessions with peers,
//! and moq relays it is attached to. A path resolves to its cheapest route.
//! When that route dies, moq moves to another route of the same instance
//! without ending the subscription, and [`Subscription::closed`] asks the table
//! again for any other once the instance is gone. The application picks the
//! paths.
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
//! let publication = moq.publish("demo/studio", Audience::Everyone)?;
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
//! - [`Subscription`]: a path resolved in the route table.
//! - [`Session`]: a direct session with a peer. Its [`Grant`] says what the
//!   peer may subscribe to and publish.
//! - [`RelayLink`]: a moq relay the node stays attached to.
//!
//! Every link runs a connection monitor. [`Session::link_sample`],
//! [`RelayLink::link_sample`] and [`Subscription::serving_link`] return its
//! latest [`LinkSample`].
//!
//! # Trust and grants
//!
//! A publication is a broadcast in the route table, and every link serves the
//! table's local view: this node's publications, never a route it learned from
//! another link. What a link's peer sees is the grant moq enforces on the live
//! session: the exact paths of the publications whose audience admits the
//! peer, within the peer's [`Grant`], which every session with the peer
//! shares. The node re-authorizes every session when a publication or an
//! audience changes, so a peer taken out of an audience loses the path at
//! once, and what it read there ends with [`moq_net::Error::Unauthorized`].
//! This holds on both sides of a session, dialed or accepted, and on every
//! protocol version.
//!
//! A [`policy`] decides who may hold a session and with which grant, on
//! accept and on dial (see [`NodeConfig::policy`]). The node asks it again
//! when an authorization expires, when the policy reports a change, and on
//! [`Node::reauthorize`], and applies the answer to the peer's live sessions.
//! Without a policy, [`NodeConfig::grant`] gives each peer its grant from its
//! endpoint id, and without that, a peer may subscribe to anything and
//! publish nothing. [`Grant::publish_under`] lets a peer publish only at
//! paths that name it, so it cannot stand in for another peer.
//!
//! A [`BroadcastTicket`] names a peer and the path it publishes at.
//! [`Node::subscribe_ticket`] claims that path for that peer with
//! [`Node::claim_path`], which widens the peer's grant to exactly that path for
//! as long as the subscription lives, so a viewer needs no grant of its own.
//! The peer has to own the path: by default, exactly one segment of it is the
//! peer's own endpoint id (see [`NodeConfig::owner`]). A claim therefore never
//! overlaps another peer's paths, which hop pinning and the route table rely on.
//!
//! The grants are also what keeps one publisher from taking over another's
//! readers. A route carries the instance its announcer claims, and moq moves a
//! reader between routes that claim the same instance. A peer or relay that
//! announces a path under another publisher's instance could therefore take
//! over that publisher's readers. The table's contents prevent it:
//!
//! - A direct peer announces only paths its grant lets it publish. With grants
//!   that name the peer, it cannot reach another peer's paths.
//! - An instance is a random string. It only reaches the links whose grant
//!   lets them see the publication.
//! - A relay is trusted with the paths it forwards. Attach only relays that
//!   keep each publisher to its own paths, as moq-relay's tokens do.
//!
//! Hops take no part in this, since moq moves readers by instance and not by
//! the first hop a publisher claims.
//!
//! # Cancellation safety
//!
//! | Future | Safe | When dropped |
//! |---|---|---|
//! | [`Node::connect`], [`Node::connect_with`], [`Node::subscribe`], [`Node::subscribe_ticket`], [`Session::subscribe`] | yes | a dial it started continues for other callers |
//! | [`policy::Queue::next`] | yes | loses nothing |
//! | [`moq_transport_iroh::dial`] | yes | drops the connection |
//! | [`RelayLink::detach`] | yes | the link's task finishes the close |
//! | [`Publication::withdrawn`], [`Subscription::closed`], [`Session::closed`] | yes | loses nothing |
//! | [`Node::closed`] | yes | loses nothing |
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
mod ticket;

/// The moq-net this crate builds against.
pub use moq_net as net;
use moq_transport_iroh as transport;
pub use moq_transport_iroh::{ALPN, Congestion, HANDSHAKE_TIMEOUT, LinkSample, MoqPreset, alpns};

pub use self::{
    admission::{Admission, ConnectOptions, Grant, GrantFn, SessionRequest},
    claim::{OwnerFn, PathClaim},
    error::Error,
    link::{LinkId, LinkKind, ServingLink},
    node::{DECIDE_TIMEOUT, Node, NodeConfig, Reach},
    publish::{Audience, Publication},
    relay::{DEFAULT_RELAY_COST, RelayConfig, RelayLink, RelayOffer, RelayStatus, TokenFn},
    route::Subscription,
    session::Session,
    ticket::BroadcastTicket,
};
