//! The node: an endpoint, the MoQ transport on it, and the router that accepts.

use std::{collections::BTreeSet, sync::Arc};

use iroh::{
    Endpoint, EndpointId,
    protocol::{DynProtocolHandler, ProtocolHandler, Router},
};
use iroh_moq::{
    Audience, Grant, Moq, MoqConfig, Publication, Reach, Subscription,
    net::{Pattern, Patterns},
};
#[cfg(feature = "media")]
use moq_ext_player::RemoteBroadcast;
use moq_net::{Consume, broadcast};
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
pub fn grant(peer: EndpointId) -> Grant {
    grant_with(peer, &[])
}

/// Returns [`grant`] widened by `scopes`, each applied to `peer`.
///
/// What a peer may publish is the application's choice: a node that runs
/// rooms passes `rooms::publish_scope` here, and one that does not leaves it
/// out, whatever features the build has.
pub fn grant_with(peer: EndpointId, scopes: &[PublishScope]) -> Grant {
    let mut publish = Patterns::from(paths::publish_scope(peer));
    for scope in scopes {
        publish.insert(scope(peer));
    }
    Grant::publish_under(publish)
}

/// Returns the [`MoqConfig`] of a live node: admission open, with [`grant`].
pub fn moq_config() -> MoqConfig {
    moq_config_with(&[])
}

/// Returns the [`MoqConfig`] of a live node whose peers may also publish
/// under `scopes`, as [`grant_with`] says.
pub fn moq_config_with(scopes: &[PublishScope]) -> MoqConfig {
    let scopes: Arc<[PublishScope]> = scopes.into();
    MoqConfig {
        grant: Some(Arc::new(move |peer| grant_with(peer, &scopes))),
        ..Default::default()
    }
}

/// A node ready for live media.
///
/// Cheap to clone. [`shutdown`](Self::shutdown) ends it for every clone and
/// closes the endpoint it was built on.
#[derive(Debug, Clone)]
pub struct Live {
    endpoint: Endpoint,
    moq: Moq,
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
    config: MoqConfig,
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

    /// Creates the node as [`with_rooms`](Self::with_rooms) says, and returns
    /// its rooms with it.
    #[cfg(feature = "rooms")]
    pub fn spawn_with_rooms(self) -> (Live, iroh_live_rooms::Rooms) {
        let live = self.with_rooms().spawn();
        let rooms = live.rooms.clone().expect("with_rooms creates the rooms");
        (live, rooms)
    }

    /// Creates the node, spawning its router if asked to.
    pub fn spawn(self) -> Live {
        let moq = Moq::new(self.endpoint.clone(), self.config);
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
    /// The node's [`Moq`] has [`moq_config`]: open admission, and each peer
    /// may publish only under `live/<its id>/`. Use
    /// [`builder_with`](Self::builder_with) to change that. [`spawn`](LiveBuilder::spawn)
    /// creates the [`Moq`], so it must be called within a tokio runtime.
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let endpoint = iroh::Endpoint::bind(iroh_live::MoqPreset).await?;
    /// let live = iroh_live::Live::builder(endpoint).with_router().spawn();
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        Self::builder_with(endpoint, moq_config())
    }

    /// Returns a builder for a node on `endpoint` with its own [`MoqConfig`].
    ///
    /// This is the override path for everything [`moq_config`] decides: a
    /// node whose peers may publish more or less than [`grant`] allows, with
    /// its own admission, or with no grant at all (`MoqConfig::default()`
    /// lets peers subscribe but not publish). Nothing else in [`Live`] depends
    /// on the grant. With rooms, prefer [`LiveBuilder::with_rooms`], which
    /// widens the grant for you.
    pub fn builder_with(endpoint: Endpoint, config: MoqConfig) -> LiveBuilder {
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
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the MoQ transport.
    ///
    /// The escape hatch for everything [`Live`] has no call for: any path
    /// layout, any [`Audience`], relays, sessions, the route table. The
    /// calls here are conveniences over it.
    pub fn moq(&self) -> &Moq {
        &self.moq
    }

    /// Publishes a broadcast as `live/<this node's id>/<name>` to everyone.
    ///
    /// Everyone includes attached relays. [`ticket`](Self::ticket) returns
    /// what to share. Pass a `LocalBroadcast` by reference, so the application
    /// keeps changing its sources. For another audience, publish through
    /// [`moq`](Self::moq).
    ///
    /// # Errors
    ///
    /// Fails if something is already published under `name`, or the name is
    /// not a valid path.
    pub fn publish(
        &self,
        name: &str,
        broadcast: impl Consume<broadcast::Consumer>,
    ) -> Result<Publication, Error> {
        if name.is_empty() {
            return Err(e!(iroh_moq::Error::InvalidPath {
                path: name.to_owned()
            })
            .into());
        }
        Ok(self
            .moq
            .publish(self.ticket(name).path(), broadcast, Audience::Everyone)?)
    }

    /// Publishes a broadcast at `path` to `audience`.
    ///
    /// The path-level call under [`publish`](Self::publish): it applies no
    /// layout, so `path` need not be under `live/`. Peers can subscribe only
    /// where their grant on this node allows it, and a peer that publishes
    /// into this node must be allowed `path` by its grant on the other side.
    ///
    /// # Errors
    ///
    /// Fails if something is already published at `path`, or it is not a
    /// valid path.
    pub fn publish_at(
        &self,
        path: &str,
        broadcast: impl Consume<broadcast::Consumer>,
        audience: Audience,
    ) -> Result<Publication, Error> {
        Ok(self.moq.publish(path, broadcast, audience)?)
    }

    /// Resolves the broadcast at `path` over the links `reach` names.
    ///
    /// The path-level call under [`subscribe`](Self::subscribe).
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails if no link reaches the broadcast, or `path` is not a valid path.
    #[instrument("subscribe_path", skip_all, fields(path))]
    pub async fn subscribe_path(&self, path: &str, reach: Reach) -> Result<Subscription, Error> {
        Ok(self.moq.subscribe(path, reach).await?)
    }

    /// Offers `broadcast` as this node's side of a call with `peer`.
    ///
    /// Publishes at `live/<this node's id>/call`, visible to `peer` only. The
    /// peer learns it is called when that path appears in its route table, and
    /// that it was hung up when the path ends: call
    /// [`Publication::unpublish`] to hang up. Pair it with
    /// [`answer`](Self::answer) on the other side.
    ///
    /// # Errors
    ///
    /// Fails if a call offer is already published.
    pub fn call(
        &self,
        peer: EndpointId,
        broadcast: impl Consume<broadcast::Consumer>,
    ) -> Result<Publication, Error> {
        self.publish_at(
            &paths::call(self.endpoint.id()),
            broadcast,
            Audience::Peers(Watchable::new(BTreeSet::from([peer]))),
        )
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
