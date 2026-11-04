//! The node: an endpoint, the MoQ transport on it, and the router that accepts.

use std::{collections::BTreeSet, sync::Arc};

use iroh::{
    Endpoint, EndpointId,
    protocol::{DynProtocolHandler, ProtocolHandler, Router},
};
use iroh_live_node::{
    Audience, Grant, Node, NodeConfig, Publication, Reach, Subscription,
    net::{Pattern, Patterns},
};
#[cfg(feature = "media")]
use moq_ext_player::RemoteBroadcast;
use moq_net::broadcast;
use n0_error::e;
use n0_watcher::Watchable;
use tracing::{error, info, instrument};

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
/// Cheap to clone. [`shutdown`](Self::shutdown) ends it for every clone and
/// closes the endpoint it was built on.
#[derive(Debug, Clone)]
pub struct Live {
    endpoint: Endpoint,
    moq: Node,
    router: Option<Router>,
    #[cfg(feature = "rooms")]
    rooms: Option<iroh_live_rooms::Rooms>,
}

/// Builds a [`Live`] on an endpoint.
///
/// Obtained from [`Live::builder`]. The node takes over the endpoint:
/// [`Live::shutdown`] closes it.
#[derive(derive_more::Debug)]
#[must_use]
pub struct LiveBuilder {
    endpoint: Endpoint,
    config: NodeConfig,
    router: bool,
    #[cfg(feature = "rooms")]
    rooms: bool,
    #[debug(skip)]
    protocols: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
}

impl LiveBuilder {
    /// Accepts incoming sessions on a router this builder spawns.
    ///
    /// Without a router the node only dials. It can subscribe and serve the
    /// peers it dialed, but nobody can connect to it.
    pub fn with_router(mut self) -> Self {
        self.router = true;
        self
    }

    /// Mounts `handler` under `alpn` on the router as well.
    ///
    /// Implies [`with_router`](Self::with_router).
    pub fn accept(mut self, alpn: impl AsRef<[u8]>, handler: impl ProtocolHandler) -> Self {
        self.protocols
            .push((alpn.as_ref().to_vec(), handler.into()));
        self
    }

    /// Runs rooms on the node.
    ///
    /// Mounts rooms' gossip protocol on the router, which this implies, and
    /// widens every peer's grant by `rooms::publish_scope`, so members can
    /// publish their room broadcasts besides their own `live/` ones. Without
    /// a grant, peers get [`Grant::subscribe_only`] plus that scope. Read the service with
    /// [`Live::rooms`], or build with [`spawn_with_rooms`](Self::spawn_with_rooms).
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

    /// Creates the node, spawning its router if asked to.
    pub fn spawn(self) -> Live {
        let moq = Node::new(self.endpoint.clone(), self.config);
        #[cfg(feature = "rooms")]
        let rooms = self.rooms.then(|| iroh_live_rooms::Rooms::new(&moq));
        #[cfg(feature = "rooms")]
        let serve = self.router || !self.protocols.is_empty() || rooms.is_some();
        #[cfg(not(feature = "rooms"))]
        let serve = self.router || !self.protocols.is_empty();
        let router = serve.then(|| {
            let mut router = moq.mount(Router::builder(self.endpoint.clone()));
            for (alpn, handler) in self.protocols {
                router = router.accept(alpn, handler);
            }
            #[cfg(feature = "rooms")]
            if let Some(rooms) = &rooms {
                router = router.accept(iroh_live_rooms::ALPN, rooms.protocol_handler());
            }
            router.spawn()
        });
        info!(id = %self.endpoint.id(), accepting = router.is_some(), "live node started");
        Live {
            endpoint: self.endpoint,
            moq,
            router,
            #[cfg(feature = "rooms")]
            rooms,
        }
    }
}

impl Live {
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
    /// let live = iroh_live::Live::builder(endpoint).with_router().spawn();
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
            router: false,
            #[cfg(feature = "rooms")]
            rooms: false,
            protocols: Vec::new(),
        }
    }

    /// Returns the endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
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
            paths::call(self.endpoint.id()),
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
        BroadcastTicket::new(self.endpoint.id(), name)
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

    /// Shuts the node down, the router and the endpoint included.
    ///
    /// Idempotent. Not cancellation safe: call it again to finish.
    pub async fn shutdown(&self) {
        self.moq.shutdown().await;
        if let Some(router) = self.router.as_ref()
            && let Err(err) = router.shutdown().await
        {
            // Close the endpoint anyway, or its socket stays open.
            error!(error = %err, "failed to shut down the iroh router");
        }
        self.endpoint.close().await;
    }
}

#[cfg(all(test, feature = "rooms"))]
mod tests {
    use iroh::{SecretKey, endpoint::presets};

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
        live.shutdown().await;
    }
}
