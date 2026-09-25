//! The node: an endpoint, the MoQ transport on it, and the router that accepts.

use std::sync::Arc;

use iroh::{
    Endpoint, EndpointId,
    protocol::{DynProtocolHandler, ProtocolHandler, Router},
};
#[cfg(feature = "media")]
use iroh_live_media::RemoteBroadcast;
use iroh_moq::{
    Audience, Grant, Moq, MoqConfig, Publication, Reach, Subscription,
    net::{Pattern, Patterns},
};
use moq_net::{Consume, broadcast};
use n0_error::e;
use tracing::{error, info, instrument};

#[cfg(feature = "media")]
use crate::network;
use crate::{BroadcastTicket, Error};

/// Returns the paths `peer` publishes its own broadcasts under: `live/<peer>/**`.
pub fn publish_scope(peer: EndpointId) -> Pattern {
    format!("live/{peer}/**")
        .parse()
        .expect("an endpoint id is a valid path segment")
}

/// Returns the grant a live node gives `peer`.
///
/// The peer may subscribe to anything, and publish only under
/// [`publish_scope`] and, with the `rooms` feature, `rooms::publish_scope`. So
/// no peer can publish at a path that names another.
pub fn grant(peer: EndpointId) -> Grant {
    #[cfg_attr(
        not(feature = "rooms"),
        allow(unused_mut, reason = "rooms add a scope")
    )]
    let mut publish = Patterns::from(publish_scope(peer));
    #[cfg(feature = "rooms")]
    publish.insert(iroh_live_rooms::publish_scope(peer));
    Grant::publish_under(publish)
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
/// Cheap to clone. [`shutdown`](Self::shutdown) ends it for every clone and
/// closes the endpoint it was built on.
#[derive(Debug, Clone)]
pub struct Live {
    endpoint: Endpoint,
    moq: Moq,
    router: Option<Router>,
}

/// Builds a [`Live`] on an endpoint.
///
/// Obtained from [`Live::builder`]. The node takes over the endpoint:
/// [`Live::shutdown`] closes it.
#[derive(derive_more::Debug)]
#[must_use]
pub struct LiveBuilder {
    endpoint: Endpoint,
    moq: Moq,
    router: bool,
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
    /// Implies [`with_router`](Self::with_router). Rooms mount their gossip
    /// this way.
    pub fn accept(mut self, alpn: impl AsRef<[u8]>, handler: impl ProtocolHandler) -> Self {
        self.protocols
            .push((alpn.as_ref().to_vec(), handler.into()));
        self
    }

    /// Returns the node's MoQ transport, for handing to `Rooms` before the
    /// router is built.
    pub fn moq(&self) -> &Moq {
        &self.moq
    }

    /// Creates the node, spawning its router if asked to.
    pub fn spawn(self) -> Live {
        let moq = self.moq;
        let router = (self.router || !self.protocols.is_empty()).then(|| {
            let mut router = moq.mount(Router::builder(self.endpoint.clone()));
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
    /// Creates the node's [`Moq`] with [`moq_config`], so it must be called
    /// within a tokio runtime.
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let endpoint = iroh::Endpoint::bind(iroh_live::MoqPreset).await?;
    /// let live = iroh_live::Live::builder(endpoint).with_router().spawn();
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        LiveBuilder {
            moq: Moq::new(endpoint.clone(), moq_config()),
            endpoint,
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
    /// For publishing to another audience or attaching relays.
    pub fn moq(&self) -> &Moq {
        &self.moq
    }

    /// Publishes a broadcast as `live/<this node's id>/<name>` to everyone.
    ///
    /// Everyone includes attached relays. [`ticket`](Self::ticket) returns
    /// what to share. Pass a media crate `LocalBroadcast` by reference, so the
    /// application keeps changing its sources. For
    /// another audience, publish through [`moq`](Self::moq).
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
    /// Does not check that anything is published there.
    pub fn ticket(&self, name: &str) -> BroadcastTicket {
        BroadcastTicket::new(self.endpoint.id(), name)
    }

    /// Resolves the ticket's broadcast over whichever link serves it.
    ///
    /// Dials the publisher, and waits on attached relays at the same time.
    /// Returns once a route is found. Read the media with
    /// [`remote_broadcast`](Self::remote_broadcast).
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
        RemoteBroadcast::from_resolved(
            subscription.as_origin(),
            subscription.path(),
            subscription.as_moq(),
        )
        .with_network(network::signals(subscription.clone()))
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
