use derive_more::Debug;
use iroh::{
    Endpoint,
    endpoint::presets,
    protocol::{Router, RouterBuilder},
};
use iroh_gossip::Gossip;
use iroh_moq::{Moq, MoqProtocolHandler};
use moq_media::publish::LocalBroadcast;
use n0_error::Result;
use tracing::{error, info, instrument};

use crate::rooms::{Room, RoomTicket};

/// Entry point for iroh-live. Manages the iroh [`Endpoint`], MoQ transport,
/// and optionally [`Gossip`] for room membership.
#[derive(Clone, Debug)]
pub struct Live {
    #[debug("{}", endpoint.id())]
    endpoint: Endpoint,
    #[debug(skip)]
    moq: Moq,
    #[debug(skip)]
    gossip: Option<Gossip>,
    #[debug(skip)]
    router: Option<Router>,
}

/// Builder for [`Live`].
///
/// Obtained via [`Live::builder`] from an existing [`Endpoint`] or via
/// [`Live::from_env`] which creates an endpoint from environment variables.
///
/// ```rust,no_run
/// # async fn example() -> n0_error::Result<()> {
/// use iroh_live::Live;
/// let live = Live::from_env().await?.with_router().spawn();
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
#[must_use]
pub struct LiveBuilder {
    #[debug(skip)]
    endpoint: Endpoint,
    #[debug(skip)]
    gossip: Option<Gossip>,
    with_gossip: bool,
    with_router: bool,
}

impl LiveBuilder {
    /// Enables gossip, which is required for room membership.
    ///
    /// Creates a [`Gossip`] instance internally and mounts it on the
    /// [`Router`] if [`with_router`](Self::with_router) is also set.
    pub fn with_gossip(mut self) -> Self {
        self.with_gossip = true;
        self
    }

    /// Sets an externally-managed [`Gossip`] instance.
    ///
    /// Use this instead of [`with_gossip`](Self::with_gossip) when you manage
    /// gossip yourself. You are responsible for mounting it on your own router.
    pub fn gossip(mut self, gossip: Gossip) -> Self {
        self.gossip = Some(gossip);
        self.with_gossip = false;
        self
    }

    /// Spawns an internal [`Router`] so that the endpoint accepts incoming
    /// MoQ sessions. Any broadcasts registered via [`Live::publish`] will be
    /// served to peers that connect.
    ///
    /// Without this, only outbound connections initiated via
    /// [`Live::subscribe`] or [`Moq::connect`](iroh_moq::Moq::connect) work.
    ///
    /// If you already have a router (for instance because the endpoint serves
    /// other protocols too), skip this and call
    /// [`Live::register_protocols`] on your own [`RouterBuilder`] instead.
    pub fn with_router(mut self) -> Self {
        self.with_router = true;
        self
    }

    /// Consumes the builder and creates a [`Live`] instance.
    pub fn spawn(self) -> Live {
        let gossip = self.gossip.or_else(|| {
            if self.with_gossip {
                Some(Gossip::builder().spawn(self.endpoint.clone()))
            } else {
                None
            }
        });

        let moq = Moq::new(self.endpoint.clone());
        let mut live = Live {
            endpoint: self.endpoint.clone(),
            moq,
            gossip,
            router: None,
        };

        if self.with_router {
            let router = live.register_protocols(Router::builder(self.endpoint));
            live.router = Some(router.spawn());
        }

        live
    }
}

impl Live {
    /// Returns a builder for an existing [`Endpoint`].
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        LiveBuilder {
            endpoint,
            gossip: None,
            with_gossip: false,
            with_router: false,
        }
    }

    /// Creates a [`Live`] instance from an existing endpoint without a builder.
    ///
    /// Equivalent to `Live::builder(endpoint).spawn()`. Does not accept
    /// incoming connections and does not enable gossip.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::builder(endpoint).spawn()
    }

    /// Binds an iroh [`Endpoint`] and returns a [`LiveBuilder`].
    ///
    /// Reads `IROH_SECRET` for the secret key and generates a new one if
    /// the variable is not set. The endpoint uses the [`N0`](presets::N0)
    /// preset for relay and DNS discovery.
    ///
    /// ```rust,no_run
    /// # async fn example() -> n0_error::Result<()> {
    /// use iroh_live::Live;
    /// // Outbound connections only, no gossip:
    /// let live = Live::from_env().await?.spawn();
    /// // Accept incoming connections and enable gossip for rooms:
    /// let live = Live::from_env().await?.with_router().with_gossip().spawn();
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_env() -> Result<LiveBuilder> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(crate::util::secret_key_from_env()?)
            .bind()
            .await?;
        info!(endpoint_id=%endpoint.id(), "endpoint bound");
        Ok(Self::builder(endpoint))
    }

    /// Mounts the MoQ and gossip protocol handlers onto a [`RouterBuilder`].
    ///
    /// Use this when you manage your own [`Router`] instead of calling
    /// [`LiveBuilder::with_router`].
    pub fn register_protocols(&self, router: RouterBuilder) -> RouterBuilder {
        let router = router.accept(iroh_moq::ALPN, self.moq.protocol_handler());
        if let Some(ref gossip) = self.gossip {
            return router.accept(iroh_gossip::ALPN, gossip.clone());
        }
        router
    }

    /// Returns the iroh [`Endpoint`].
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the MoQ transport handle for advanced operations.
    pub fn transport(&self) -> &Moq {
        &self.moq
    }

    /// Returns the [`Gossip`] instance if gossip was enabled.
    pub fn gossip(&self) -> Option<&Gossip> {
        self.gossip.as_ref()
    }

    /// Returns the MoQ protocol handler for manual [`Router`] mounting.
    pub fn protocol_handler(&self) -> MoqProtocolHandler {
        self.moq.protocol_handler()
    }

    /// Registers a broadcast so that every connected peer can subscribe to it.
    ///
    /// The broadcast is published on all existing MoQ sessions immediately and
    /// on every new session that is established afterwards, whether incoming
    /// (accepted by the [`Router`]) or outbound (created via [`subscribe`](Self::subscribe)
    /// or [`Moq::connect`](iroh_moq::Moq::connect)).
    ///
    /// To publish on a single session instead, use
    /// [`MoqSession::publish`](iroh_moq::MoqSession::publish) directly.
    pub async fn publish(&self, name: impl ToString, broadcast: &LocalBroadcast) -> Result<()> {
        self.moq.publish(name, broadcast.producer()).await
    }

    /// Registers a raw [`BroadcastProducer`](moq_lite::BroadcastProducer).
    ///
    /// Prefer [`publish`](Self::publish) with a [`LocalBroadcast`] for the
    /// common case. This method exists for situations where you construct the
    /// producer yourself, for instance when importing a media file.
    ///
    /// The same session-scoping rules as [`publish`](Self::publish) apply.
    pub async fn publish_broadcast_producer(
        &self,
        name: impl ToString,
        producer: moq_lite::BroadcastProducer,
    ) -> Result<()> {
        self.moq.publish(name, producer).await
    }

    /// Subscribes to a broadcast across one or more transport sources.
    ///
    /// `sources` accepts anything convertible into a
    /// [`SourceSetHandle`](crate::sources::SourceSetHandle):
    /// [`EndpointAddr`](iroh::EndpointAddr) or
    /// [`EndpointId`](iroh::EndpointId) for a single direct peer,
    /// [`RelayTarget`](crate::relay::RelayTarget) for a single
    /// relay, [`SourceSet`](crate::sources::SourceSet) for a
    /// static set of candidates, or a `SourceSetHandle` for a
    /// reactively mutable set.
    ///
    /// The returned [`Subscription`](crate::Subscription) opens a
    /// MoQ session for every source in the set, picks the active
    /// one through the default
    /// [`PreferOrdered`](crate::PreferOrdered) policy, and emits
    /// [`SubscriptionEvent`](crate::SubscriptionEvent)s as sources
    /// come and go.
    ///
    /// # Examples
    ///
    /// Single direct peer:
    ///
    /// ```rust,no_run
    /// # async fn example(live: iroh_live::Live, peer: iroh::EndpointAddr) -> n0_error::Result<()> {
    /// let sub = live.subscribe(peer, "cam");
    /// let active = sub.ready().await?;
    /// let _video = active.broadcast().video_ready().await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Mutable set with two candidates:
    ///
    /// ```rust,no_run
    /// # async fn example(
    /// #     live: iroh_live::Live,
    /// #     peer: iroh::EndpointAddr,
    /// #     relay: iroh_live::relay::RelayTarget,
    /// # ) -> n0_error::Result<()> {
    /// use iroh_live::{SourceSet, SourceSetHandle, TransportSource};
    ///
    /// let mut set = SourceSet::new();
    /// set.push(TransportSource::direct(peer));
    /// set.push(TransportSource::relay(relay));
    /// let sources = SourceSetHandle::new(set);
    /// let _sub = live.subscribe(sources, "cam");
    /// # Ok(())
    /// # }
    /// ```
    #[instrument("Subscribe", skip_all, fields(broadcast = %broadcast_name.to_string()))]
    pub fn subscribe(
        &self,
        sources: impl Into<crate::sources::SourceSetHandle>,
        broadcast_name: impl ToString,
    ) -> crate::Subscription {
        crate::Subscription::spawn(self.clone(), broadcast_name, sources)
    }

    /// Subscribes to the broadcast named in `ticket` using the ticket's
    /// direct endpoint and any embedded relay offers as candidate
    /// sources.
    ///
    /// Equivalent to building a `SourceSet` from the ticket and
    /// calling [`Live::subscribe`].
    pub fn subscribe_ticket(&self, ticket: &crate::ticket::LiveTicket) -> crate::Subscription {
        let sources = ticket.source_set();
        self.subscribe(sources, ticket.broadcast_name.clone())
    }

    /// Joins a room using the given ticket.
    pub async fn join_room(&self, ticket: RoomTicket) -> Result<Room> {
        Room::new(self, ticket).await
    }

    /// Spawns a [`Broadcaster`](crate::Broadcaster) that attaches
    /// `producer` to every session named by `sources`.
    ///
    /// Adding a source opens an outbound session and starts
    /// announcing `name` there; removing a source drops the announce
    /// on that session only.
    pub fn broadcaster(
        &self,
        name: impl ToString,
        producer: moq_lite::BroadcastProducer,
        sources: impl Into<crate::sources::SourceSetHandle>,
    ) -> crate::Broadcaster {
        crate::Broadcaster::spawn(self.clone(), name, producer, sources)
    }

    /// Shuts down the [`Live`] instance.
    ///
    /// Closes all MoQ sessions, stops the [`Router`] if one was spawned, and
    /// closes the iroh [`Endpoint`] unconditionally.
    pub async fn shutdown(&self) {
        self.moq.shutdown();
        if let Some(router) = self.router.as_ref()
            && let Err(err) = router.shutdown().await
        {
            error!("Error while shutting down the iroh router: {err:#}");
        } else {
            self.endpoint.close().await
        }
    }
}
