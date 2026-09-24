//! The node: an endpoint, the MoQ transport on it, and the router that accepts.

use std::sync::Arc;

use iroh::{
    Endpoint, EndpointId,
    protocol::{DynProtocolHandler, ProtocolHandler, Router},
};
use iroh_live_media::RemoteBroadcast;
use iroh_moq::{
    Audience, Grant, Moq, MoqConfig, Publication, Reach, RouteInfo, Subscription,
    net::{Pattern, Patterns},
};
use moq_net::{Consume, broadcast};
use n0_error::e;
use tracing::{error, info, instrument};

use crate::{BroadcastTicket, Error, network};

/// Returns the paths `peer` publishes its own broadcasts under: `live/<peer>/**`.
pub fn publish_scope(peer: EndpointId) -> Pattern {
    format!("live/{peer}/**")
        .parse()
        .expect("an endpoint id is a valid path segment")
}

/// Returns the grant a live node gives `peer`.
///
/// The peer may subscribe to anything and publish only its own broadcasts,
/// under [`publish_scope`], so no peer can stand in for another at a path that
/// names it. With the `rooms` feature it may also publish into rooms, under
/// `rooms::publish_scope`.
pub fn grant(peer: EndpointId) -> Grant {
    #[cfg_attr(
        not(feature = "rooms"),
        allow(unused_mut, reason = "rooms add a scope")
    )]
    let mut publish = Patterns::from(publish_scope(peer));
    #[cfg(feature = "rooms")]
    publish.insert(iroh_live_rooms::publish_scope(peer));
    Grant {
        subscribe: Patterns::from(Pattern::all()),
        publish,
    }
}

/// Returns the [`MoqConfig`] of a live node: admission open, with [`grant`].
pub fn moq_config() -> MoqConfig {
    MoqConfig {
        grant: Some(Arc::new(grant)),
        ..Default::default()
    }
}

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

    /// Mounts `handler` under `alpn` on the router as well.
    ///
    /// Implies [`with_router`](Self::with_router), since a handler needs a
    /// router to accept anything; rooms mount their gossip this way.
    pub fn accept(mut self, alpn: impl AsRef<[u8]>, handler: impl ProtocolHandler) -> Self {
        self.protocols
            .push((alpn.as_ref().to_vec(), handler.into()));
        self
    }

    /// Uses a [`Moq`] the application created first, on the builder's endpoint.
    ///
    /// So it can hand it to `Rooms` before the router is built. Create it with
    /// [`moq_config`], or a config whose grant keeps peers to their own paths
    /// as [`grant`] does.
    pub fn with_moq(mut self, moq: Moq) -> Self {
        self.moq = Some(moq);
        self
    }

    /// Creates the node, spawning its router if asked to.
    pub fn spawn(self) -> Live {
        let moq = self
            .moq
            .unwrap_or_else(|| Moq::new(self.endpoint.clone(), moq_config()));
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
    /// Attached relays included. [`ticket`](Self::ticket) returns what to
    /// share. Pass
    /// a [`LocalBroadcast`](iroh_live_media::LocalBroadcast) by reference: the
    /// publication reads it for as long as it exists, and the application keeps
    /// changing its sources.
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

    /// Returns the ticket for this node's broadcast `name`.
    ///
    /// Names `live/<this node's id>/<name>`, where [`publish`](Self::publish)
    /// puts it, so it is what to share after publishing. The ticket is only a
    /// name: it does not check that anything is published there.
    pub fn ticket(&self, name: &str) -> BroadcastTicket {
        BroadcastTicket::new(self.endpoint.id(), name)
    }

    /// Resolves the ticket's broadcast over whichever link serves it.
    ///
    /// Returns once a route is found, without waiting for the catalog: watch
    /// [`RemoteBroadcast::catalog`] for it. The broadcast follows the path in
    /// the route table, so a change of route shows as a switch rather than an
    /// end, and its players adapt on the serving link, a direct session or a
    /// relay.
    ///
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails if no link reaches the broadcast.
    #[instrument("subscribe", skip_all, fields(ticket = %ticket))]
    pub async fn subscribe(&self, ticket: &BroadcastTicket) -> Result<RemoteBroadcast, Error> {
        let subscription = self
            .moq
            .subscribe(ticket.path(), Reach::Both(ticket.peer()))
            .await?;
        Ok(self.remote_broadcast(&subscription))
    }

    /// Wraps a subscription the way [`subscribe`](Self::subscribe) does.
    ///
    /// For a subscription from a room or from [`Moq::subscribe`]: the
    /// broadcast starts from the one the subscription resolved, follows the
    /// subscription's path through the route table it was resolved in, and
    /// attaches the serving link so its players adapt. A subscription pinned
    /// to one session, such as a room member's, re-resolves through that
    /// session only. Synchronous and infallible: nothing here waits for the
    /// catalog.
    pub fn remote_broadcast(&self, subscription: &Subscription) -> RemoteBroadcast {
        RemoteBroadcast::from_resolved(
            subscription.as_origin(),
            subscription.path(),
            subscription.as_moq(),
        )
        .with_network(network::signals(subscription.clone()))
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
