//! The node: an endpoint, the MoQ transport on it, and the router that accepts.

use iroh::{
    Endpoint,
    protocol::{DynProtocolHandler, ProtocolHandler, Router},
};
use iroh_live_media::subscribe::RemoteBroadcast;
use iroh_moq::{Audience, BroadcastTicket, Moq, MoqConfig, Publication, RouteInfo, Subscription};
use moq_net::{Consume, broadcast};
use tracing::{error, info, instrument};

use crate::{Error, network};

/// A node ready for live media.
///
/// Cheap to clone; [`shutdown`](Self::shutdown) ends it for every clone, and
/// closes the endpoint it was built on.
#[derive(Debug, Clone)]
pub struct Live {
    endpoint: Endpoint,
    moq: Moq,
    router: Option<Router>,
}

/// Builds a [`Live`] on an endpoint the application owns.
///
/// Obtained from [`Live::builder`]. [`Live::shutdown`] closes the endpoint, so
/// build on one the node may own.
#[derive(derive_more::Debug)]
#[must_use]
pub struct LiveBuilder {
    endpoint: Endpoint,
    moq: Option<Moq>,
    router: bool,
    #[debug(skip)]
    protocols: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
}

impl LiveBuilder {
    /// Accepts incoming sessions on a router this builder spawns.
    ///
    /// Without it the node only dials: it can subscribe, and serve its
    /// publications to the peers it dialed, but nobody can connect to it.
    pub fn with_router(mut self) -> Self {
        self.router = true;
        self
    }

    /// Also mounts `handler` under `alpn` on the router.
    ///
    /// Implies [`with_router`](Self::with_router), since a handler needs a
    /// router to accept anything; rooms mount their gossip this way.
    pub fn accept(mut self, alpn: impl AsRef<[u8]>, handler: impl ProtocolHandler) -> Self {
        self.protocols
            .push((alpn.as_ref().to_vec(), handler.into()));
        self
    }

    /// Uses a [`Moq`] the application created first.
    ///
    /// So it can hand it to `Rooms` before the router is built.
    ///
    /// # Panics
    ///
    /// Panics if `moq` runs on another endpoint than the builder's: the router
    /// would then accept for one endpoint and publish from another.
    pub fn with_moq(mut self, moq: Moq) -> Self {
        assert_eq!(
            moq.endpoint().id(),
            self.endpoint.id(),
            "the Moq node runs on another endpoint than the Live builder"
        );
        self.moq = Some(moq);
        self
    }

    /// Creates the node, spawning its router if asked to.
    pub fn spawn(self) -> Live {
        let moq = self
            .moq
            .unwrap_or_else(|| Moq::new(self.endpoint.clone(), MoqConfig::default()));
        let router = (self.router || !self.protocols.is_empty()).then(|| {
            let mut router = Router::builder(self.endpoint.clone());
            // Every MoQ version this build speaks, not only the newest, so a
            // peer built against a different moq release still finds one in
            // common.
            for alpn in iroh_moq::alpns() {
                router = router.accept(alpn, moq.clone());
            }
            for (alpn, handler) in self.protocols {
                router = router.accept(alpn, handler);
            }
            router.spawn()
        });
        info!(id = %self.endpoint.id(), accepting = router.is_some(), "live node started");
        Live {
            endpoint: self.endpoint,
            moq,
            router,
        }
    }
}

impl Live {
    /// Returns a builder for a node on `endpoint`.
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let endpoint = iroh::Endpoint::bind(iroh_live::MediaPreset).await?;
    /// let live = iroh_live::Live::builder(endpoint).with_router().spawn();
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        LiveBuilder {
            endpoint,
            moq: None,
            router: false,
            protocols: Vec::new(),
        }
    }

    /// Returns the endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the MoQ transport.
    ///
    /// For publishing to another audience, admitting sessions, or attaching
    /// relays.
    pub fn moq(&self) -> &Moq {
        &self.moq
    }

    /// Publishes a broadcast as `live/<this node's id>/<name>` to everyone.
    ///
    /// Attached relays included. `publication.ticket()` is what to share.
    ///
    /// For another audience, publish through [`moq`](Self::moq).
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
        Ok(self.moq.publish(name, broadcast, Audience::Everyone)?)
    }

    /// Resolves the ticket's broadcast and starts reading its catalog.
    ///
    /// Over whichever link serves it.
    ///
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails if no link reaches the broadcast, or its catalog cannot be read.
    #[instrument("subscribe", skip_all, fields(ticket = %ticket))]
    pub async fn subscribe(&self, ticket: &BroadcastTicket) -> Result<RemoteBroadcast, Error> {
        let subscription = self.moq.subscribe(ticket.path(), self.moq.reach()).await?;
        self.remote_broadcast(&subscription).await
    }

    /// Wraps a subscription the way [`subscribe`](Self::subscribe) does.
    ///
    /// For a subscription from a room or from [`Moq::subscribe`]: reads its
    /// catalog, and records the serving link's statistics into the broadcast's
    /// network stats.
    ///
    /// # Errors
    ///
    /// Fails if the broadcast's catalog cannot be read.
    pub async fn remote_broadcast(
        &self,
        subscription: &Subscription,
    ) -> Result<RemoteBroadcast, Error> {
        let broadcast =
            RemoteBroadcast::new(subscription.path().as_str(), subscription.as_moq()).await?;
        network::record_stats(
            subscription,
            broadcast.stats().net.clone(),
            broadcast.shutdown_token(),
        );
        Ok(broadcast)
    }

    /// Returns every route to the ticket's broadcast, as the routes change.
    pub fn routes(&self, ticket: &BroadcastTicket) -> n0_watcher::Direct<Vec<RouteInfo>> {
        self.moq.routes(ticket.path())
    }

    /// Shuts the node down, the router and the endpoint included.
    ///
    /// Idempotent; not cancellation safe, call it again to finish.
    pub async fn shutdown(&self) {
        self.moq.shutdown().await;
        if let Some(router) = self.router.as_ref()
            && let Err(err) = router.shutdown().await
        {
            // Report it and close anyway: leaving the endpoint open because its
            // router complained strands the socket.
            error!(error = %err, "failed to shut down the iroh router");
        }
        self.endpoint.close().await;
    }
}
