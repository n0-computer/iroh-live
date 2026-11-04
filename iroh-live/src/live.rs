//! The node: the MoQ transport on an endpoint, with the live path layout.

use std::{collections::BTreeSet, sync::Arc};

use iroh::{Endpoint, EndpointId, protocol::RouterBuilder};
use iroh_live_node::{
    Audience, Grant, Node, NodeConfig, Publication, Reach, Subscription,
    net::{Pattern, Patterns},
};
#[cfg(feature = "media")]
use moq_ext_player::RemoteBroadcast;
use moq_net::broadcast;
use n0_error::e;
use n0_watcher::Watchable;
use tracing::{info, instrument};

#[cfg(feature = "media")]
use crate::network;
use crate::{BroadcastTicket, Error, paths};

/// A function naming the paths a peer may publish under, given its id.
///
/// [`paths::publish_scope`] is one; `rooms::publish_scope` is another.
pub type PublishScope = fn(EndpointId) -> Pattern;

/// Returns the grant a live node gives `peer`.
///
/// The peer may subscribe to anything, and publish only under
/// [`paths::publish_scope`], so no peer can publish at a path that names another.
/// [`grant_with`] adds more scopes, such as rooms'.
#[must_use]
pub fn grant(peer: EndpointId) -> Grant {
    grant_with(peer, &[])
}

/// Returns [`grant`] widened by `scopes`, each applied to `peer`.
///
/// What a peer may publish is the application's choice: a node that runs
/// rooms passes `rooms::publish_scope` here, and one that does not leaves it
/// out, whatever features the build has.
#[must_use]
pub fn grant_with(peer: EndpointId, scopes: &[PublishScope]) -> Grant {
    let mut publish = Patterns::from(paths::publish_scope(peer));
    for scope in scopes {
        publish.insert(scope(peer));
    }
    Grant::publish_under(publish)
}

/// Returns the [`NodeConfig`] of a live node: admission open, with [`grant`].
#[must_use]
pub fn node_config() -> NodeConfig {
    node_config_with(&[])
}

/// Returns the [`NodeConfig`] of a live node with [`grant_with`] and `scopes`.
#[must_use]
pub fn node_config_with(scopes: &[PublishScope]) -> NodeConfig {
    let scopes: Arc<[PublishScope]> = scopes.into();
    NodeConfig {
        grant: Some(Arc::new(move |peer| grant_with(peer, &scopes))),
        ..Default::default()
    }
}

/// Who this node's side of a call is offered to, from [`Live::call`].
///
/// Cheap to clone; every clone controls one offer. Dropping it leaves the
/// offer as it is. The call side stays published until its broadcast closes.
#[derive(Debug, Clone)]
pub struct CallOffer {
    publication: Publication,
    peers: Watchable<BTreeSet<EndpointId>>,
}

impl CallOffer {
    /// Offers the call side to `peer`, which then sees it as a call.
    pub fn ring(&self, peer: EndpointId) {
        let mut peers = self.peers.get();
        if peers.insert(peer) {
            self.peers.set(peers).ok();
        }
    }

    /// Withdraws the call side from `peer`.
    ///
    /// The peer loses the path, which tells it the call is over, and what it
    /// read there ends.
    pub fn hang_up(&self, peer: EndpointId) {
        let mut peers = self.peers.get();
        if peers.remove(&peer) {
            self.peers.set(peers).ok();
        }
    }

    /// Returns the peers the call side is offered to.
    #[must_use]
    pub fn peers(&self) -> BTreeSet<EndpointId> {
        self.peers.get()
    }

    /// Returns the publication of the call side.
    #[must_use]
    pub fn publication(&self) -> &Publication {
        &self.publication
    }
}

/// A node ready for live media.
///
/// Cheap to clone. [`shutdown`](Self::shutdown) ends it for every clone.
///
/// The node does not own its endpoint, and it accepts nothing on its own.
/// Mount it on the application's [`Router`](iroh::protocol::Router) with
/// [`mount`](Self::mount), or it serves only the peers it dials and its
/// attached relays. Whoever binds the endpoint closes it, directly or through
/// [`Router::shutdown`](iroh::protocol::Router::shutdown).
///
/// # Examples
///
/// ```no_run
/// use iroh::protocol::Router;
/// use iroh_live::{EndpointOptions, Live};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = EndpointOptions::from_env()?.bind().await?;
/// let live = Live::new(endpoint.clone());
/// let router = live.mount(Router::builder(endpoint)).spawn();
///
/// // publish and subscribe ...
///
/// live.shutdown().await;
/// router.shutdown().await?; // closes the endpoint
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Live {
    moq: Node,
    #[cfg(feature = "rooms")]
    rooms: Option<iroh_live_rooms::Rooms>,
}

/// Builds a [`Live`] on an endpoint.
///
/// Obtained from [`Live::builder`].
#[derive(Debug)]
#[must_use]
pub struct LiveBuilder {
    endpoint: Endpoint,
    config: NodeConfig,
    #[cfg(feature = "rooms")]
    rooms: bool,
}

impl LiveBuilder {
    /// Runs rooms on the node.
    ///
    /// Widens every peer's grant by `rooms::publish_scope`, so members can
    /// publish their room broadcasts besides their own `live/` ones. Without
    /// a grant, peers get [`Grant::subscribe_only`] plus that scope.
    /// [`Live::mount`] then mounts rooms' gossip protocol as well. Read the
    /// service with [`Live::rooms`], or build with
    /// [`spawn_with_rooms`](Self::spawn_with_rooms).
    #[cfg(feature = "rooms")]
    pub fn with_rooms(mut self) -> Self {
        self.rooms = true;
        let inner = self.config.grant.take();
        self.config.grant = Some(Arc::new(move |peer| {
            let mut grant = match &inner {
                Some(inner) => inner(peer),
                None => Grant::subscribe_only(),
            };
            grant.publish.insert(iroh_live_rooms::publish_scope(peer));
            grant
        }));
        self
    }

    /// Creates the node as [`with_rooms`](Self::with_rooms) says.
    ///
    /// Returns the node with its rooms.
    #[cfg(feature = "rooms")]
    pub fn spawn_with_rooms(self) -> (Live, iroh_live_rooms::Rooms) {
        let live = self.with_rooms().spawn();
        let rooms = live.rooms.clone().expect("with_rooms creates the rooms");
        (live, rooms)
    }

    /// Creates the node, and its rooms if asked to.
    ///
    /// Must be called within a tokio runtime. Mount the node with
    /// [`Live::mount`] to accept sessions.
    pub fn spawn(self) -> Live {
        let id = self.endpoint.id();
        let moq = Node::new(self.endpoint, self.config);
        #[cfg(feature = "rooms")]
        let rooms = self.rooms.then(|| iroh_live_rooms::Rooms::new(&moq));
        #[cfg(feature = "rooms")]
        info!(%id, rooms = rooms.is_some(), "live node started");
        #[cfg(not(feature = "rooms"))]
        info!(%id, "live node started");
        Live {
            moq,
            #[cfg(feature = "rooms")]
            rooms,
        }
    }
}

impl Live {
    /// Creates a node on `endpoint` with [`node_config`].
    ///
    /// Short for `Live::builder(endpoint).spawn()`. Must be called within a
    /// tokio runtime.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::builder(endpoint).spawn()
    }

    /// Returns a builder for a node on `endpoint`.
    ///
    /// The node's [`Node`] has [`node_config`]: open admission, and each peer
    /// may publish only under `live/<its id>/`. Use
    /// [`builder_with`](Self::builder_with) to change that. [`spawn`](LiveBuilder::spawn)
    /// creates the [`Node`], so it must be called within a tokio runtime.
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let endpoint = iroh::Endpoint::bind(iroh_live::MoqPreset).await?;
    /// let live = iroh_live::Live::builder(endpoint).spawn();
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        Self::builder_with(endpoint, node_config())
    }

    /// Returns a builder for a node on `endpoint` with its own [`NodeConfig`].
    ///
    /// This is the override path for everything [`node_config`] decides: a
    /// node whose peers may publish more or less than [`grant`] allows, with
    /// its own admission, or with no grant at all (`NodeConfig::default()`
    /// lets peers subscribe but not publish). Nothing else in [`Live`] depends
    /// on the grant. With rooms, prefer [`LiveBuilder::with_rooms`], which
    /// widens the grant for you.
    pub fn builder_with(endpoint: Endpoint, config: NodeConfig) -> LiveBuilder {
        LiveBuilder {
            endpoint,
            config,
            #[cfg(feature = "rooms")]
            rooms: false,
        }
    }

    /// Mounts the node on `router` under every moq ALPN, and rooms' gossip if it runs rooms.
    ///
    /// Without a mount the endpoint refuses every incoming connection, so
    /// only the peers this node dials and its attached relays can read what
    /// it publishes. `router` must be built on this node's endpoint. A
    /// handler the caller mounts under one of these ALPNs afterwards replaces
    /// the node's.
    ///
    /// ```no_run
    /// use iroh::protocol::Router;
    /// # async fn run(live: iroh_live::Live) {
    /// let router = live.mount(Router::builder(live.endpoint().clone())).spawn();
    /// # }
    /// ```
    pub fn mount(&self, router: RouterBuilder) -> RouterBuilder {
        let router = self.moq.mount(router);
        #[cfg(feature = "rooms")]
        if let Some(rooms) = &self.rooms {
            return router.accept(iroh_live_rooms::ALPN, rooms.protocol_handler());
        }
        router
    }

    /// Returns the endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        self.moq.endpoint()
    }

    /// Returns the MoQ transport.
    ///
    /// The escape hatch for everything [`Live`] has no call for: any path
    /// layout, any [`Audience`], relays, sessions, the route table. The
    /// calls here are conveniences over it.
    #[must_use]
    pub fn node(&self) -> &Node {
        &self.moq
    }

    /// Publishes a broadcast as `live/<this node's id>/<name>` to everyone, and returns it.
    ///
    /// The broadcast is created in the node's route table, and peers read it
    /// with nothing copied in between. `B` is what fills it, built from the
    /// table's `broadcast::Producer`: a `LocalBroadcast`, or the producer
    /// itself. Everyone includes attached relays. [`ticket`](Self::ticket)
    /// returns what to share.
    ///
    /// The publication lasts until the broadcast closes. A `LocalBroadcast`
    /// closes when it is closed or its last clone drops. A producer closes
    /// only when it is closed, since the node holds the broadcast open while
    /// it is published: dropping it leaves the name taken until the node
    /// shuts down. [`publish_to`](Self::publish_to) publishes to another
    /// audience. To keep the [`Publication`], for example to unpublish
    /// without closing, use [`publish_at`](Self::publish_at), or
    /// [`Node::publish_with`] on [`node`](Self::node) to fill it first.
    ///
    /// # Errors
    ///
    /// Fails if something is already published under `name`, the name is not
    /// a valid path, or `B` cannot be built from the broadcast.
    pub fn publish<B>(&self, name: &str) -> Result<B, Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        self.publish_to(name, Audience::Everyone)
    }

    /// Publishes a broadcast as `live/<this node's id>/<name>` to `audience`, and returns it.
    ///
    /// As [`publish`](Self::publish), for an audience other than everyone,
    /// such as `Audience::peers([peer])`. `B` is built before the broadcast is
    /// announced, so no peer in the audience sees it empty. Attached relays
    /// never see a [`Peers`](Audience::Peers) audience.
    ///
    /// # Errors
    ///
    /// As [`publish`](Self::publish).
    pub fn publish_to<B>(&self, name: &str, audience: Audience) -> Result<B, Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        if name.is_empty() {
            return Err(e!(iroh_live_node::Error::InvalidPath {
                path: name.to_owned()
            })
            .into());
        }
        // Built before the broadcast is announced, so no reader finds it
        // without the tracks `B` creates, such as a catalog.
        let (_publication, broadcast) =
            self.moq
                .publish_with(self.ticket(name).path(), audience, |producer| {
                    B::try_from(producer.clone())
                })?;
        Ok(broadcast)
    }

    /// Publishes a broadcast at `path` to `audience`.
    ///
    /// The path-level call under [`publish`](Self::publish): it applies no
    /// layout, so `path` need not be under `live/`. Fill the broadcast through
    /// [`Publication::broadcast`], for example with
    /// `LocalBroadcast::new_in(publication.broadcast().clone())?`. The path is
    /// announced before that, so a peer may see the broadcast empty for a
    /// moment. [`publish_to`](Self::publish_to) and [`Node::publish_with`]
    /// fill it first. Peers can subscribe only where their grant on this node
    /// allows it, and a peer that publishes into this node must be allowed
    /// `path` by its grant on the other side.
    ///
    /// # Errors
    ///
    /// Fails if something is already published at `path`, or it is not a
    /// valid path.
    pub fn publish_at(&self, path: &str, audience: Audience) -> Result<Publication, Error> {
        Ok(self.moq.publish(path, audience)?)
    }

    /// Resolves the broadcast at `path` over the links `reach` names.
    ///
    /// The path-level call under [`subscribe`](Self::subscribe).
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails if no link reaches the broadcast, or `path` is not a valid path.
    #[instrument("subscribe_path", skip_all, fields(%path))]
    pub async fn subscribe_path(&self, path: &str, reach: Reach) -> Result<Subscription, Error> {
        Ok(self.moq.subscribe(path, reach).await?)
    }

    /// Publishes this node's side of its calls, and returns it with its offer.
    ///
    /// The broadcast lives at `live/<this node's id>/call` and is visible to
    /// nobody until [`CallOffer::ring`] offers it to a peer. The peer learns
    /// it is called when that path appears in its route table, and that it
    /// was hung up when the path goes: [`CallOffer::hang_up`]. One broadcast
    /// serves every call in turn, so a camera and a preview keep running
    /// between calls. Pair it with [`answer`](Self::answer) on the other side.
    ///
    /// # Errors
    ///
    /// Fails if this node's call side is already published, or `B` cannot
    /// be built from the broadcast.
    pub fn call<B>(&self) -> Result<(B, CallOffer), Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        let peers = Watchable::new(BTreeSet::new());
        let (publication, broadcast) = self.moq.publish_with(
            paths::call(self.endpoint().id()),
            Audience::Peers(peers.clone()),
            |producer| B::try_from(producer.clone()),
        )?;
        Ok((broadcast, CallOffer { publication, peers }))
    }

    /// Resolves `peer`'s side of a call over a direct session.
    ///
    /// Waits until `peer` offers it. Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails if the peer cannot be reached.
    pub async fn answer(&self, peer: EndpointId) -> Result<Subscription, Error> {
        self.subscribe_path(&paths::call(peer), Reach::Direct(peer))
            .await
    }

    /// Returns the rooms of a node built with [`LiveBuilder::with_rooms`].
    #[cfg(feature = "rooms")]
    pub fn rooms(&self) -> Option<&iroh_live_rooms::Rooms> {
        self.rooms.as_ref()
    }

    /// Returns the ticket for this node's broadcast `name`.
    ///
    /// Does not check that anything is published there.
    #[must_use]
    pub fn ticket(&self, name: &str) -> BroadcastTicket {
        BroadcastTicket::new(self.endpoint().id(), name)
    }

    /// Resolves the ticket's broadcast over whichever link serves it.
    ///
    /// Dials the publisher, and waits on attached relays at the same time.
    /// Returns once a route is found.
    #[cfg_attr(
        feature = "media",
        doc = "Read the media with [`remote_broadcast`](Self::remote_broadcast)."
    )]
    ///
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails if no link reaches the broadcast.
    #[instrument("subscribe", skip_all, fields(ticket = %ticket))]
    pub async fn subscribe(&self, ticket: &BroadcastTicket) -> Result<Subscription, Error> {
        Ok(self
            .moq
            .subscribe(ticket.path(), Reach::Both(ticket.peer()))
            .await?)
    }

    /// Reads the media of a subscription.
    ///
    /// The broadcast follows the subscription's path through the route table
    /// it was resolved in, so a change of route does not end it, and its
    /// players adapt to the link that serves it. A subscription on one session
    /// re-resolves through that session only. The catalog arrives later:
    /// watch [`RemoteBroadcast::catalog`] for it.
    #[cfg(feature = "media")]
    pub fn remote_broadcast(&self, subscription: &Subscription) -> RemoteBroadcast {
        let remote = RemoteBroadcast::from_resolved(
            subscription.as_origin(),
            subscription.path().clone(),
            subscription.as_moq(),
        );
        remote.set_network(network::signals(subscription.clone()));
        remote
    }

    /// Shuts the node down, and its rooms if it runs them.
    ///
    /// Closes every session and withdraws every publication. Does not close
    /// the endpoint: close it afterwards, or shut down the router, which
    /// closes it. Idempotent. Not cancellation safe: call it again to finish.
    pub async fn shutdown(&self) {
        #[cfg(feature = "rooms")]
        if let Some(rooms) = &self.rooms {
            rooms.shutdown().await;
        }
        self.moq.shutdown().await;
    }
}

#[cfg(all(test, feature = "rooms"))]
mod tests {
    use iroh::{SecretKey, endpoint::presets, protocol::Router};

    use super::*;

    #[tokio::test]
    async fn only_with_rooms_may_a_peer_publish_room_broadcasts() {
        let endpoint = Endpoint::bind(presets::Minimal).await.expect("binds");
        let peer = SecretKey::generate().public();
        let room = format!("rooms/topic/{peer}/camera");

        let plain = Live::builder(endpoint.clone());
        let grant = plain.config.grant.as_ref().expect("a grant")(peer);
        assert!(grant.allows_publish(&paths::broadcast(peer, "cam")));
        assert!(!grant.allows_publish(&room));

        let rooms = Live::builder(endpoint.clone()).with_rooms();
        let grant = rooms.config.grant.as_ref().expect("a grant")(peer);
        assert!(grant.allows_publish(&paths::broadcast(peer, "cam")));
        assert!(grant.allows_publish(&room));
        assert!(!grant.allows_publish(&paths::broadcast(SecretKey::generate().public(), "cam")));

        let (live, _rooms) = rooms.spawn_with_rooms();
        assert!(live.rooms().is_some());
        let router = live.mount(Router::builder(endpoint)).spawn();
        // The router shuts the rooms and the node down a second time.
        live.shutdown().await;
        router.shutdown().await.expect("the router shuts down");
    }
}
