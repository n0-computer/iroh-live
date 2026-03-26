use derive_more::Debug;
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::presets,
    protocol::{Router, RouterBuilder},
};
use iroh_gossip::Gossip;
use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};
use moq_media::{
    format::PlaybackConfig,
    publish::LocalBroadcast,
    subscribe::{MediaTracks, RemoteBroadcast},
    traits::AudioStreamFactory,
};
use n0_error::Result;
use tracing::{error, info, instrument};

use crate::rooms::{Room, RoomTicket};

/// Entry point for iroh-live. Manages the iroh endpoint, MoQ transport,
/// and optionally gossip for room membership.
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
/// Obtained via [`Live::builder`] from an existing endpoint.
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
    /// Enables gossip (required for rooms).
    ///
    /// When set, a [`Gossip`] instance is created and auto-mounted on the
    /// Router (if [`with_router`](Self::with_router) is also set).
    pub fn with_gossip(mut self) -> Self {
        self.with_gossip = true;
        self
    }

    /// Sets an externally-managed gossip instance.
    ///
    /// Use this instead of [`with_gossip`](Self::with_gossip) when you manage
    /// Gossip yourself. You are responsible for mounting it on your Router.
    pub fn gossip(mut self, gossip: Gossip) -> Self {
        self.gossip = Some(gossip);
        self.with_gossip = false;
        self
    }

    /// Spawns an internal Router that accepts incoming connections.
    ///
    /// Without this, only outbound connections work. The Router is populated
    /// with the MoQ ALPN handler and gossip (if enabled).
    pub fn with_router(mut self) -> Self {
        self.with_router = true;
        self
    }

    /// Builds the [`Live`] instance.
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
    /// Returns a builder starting from an existing endpoint.
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        LiveBuilder {
            endpoint,
            gossip: None,
            with_gossip: false,
            with_router: false,
        }
    }

    /// Creates a new Live instance without a builder.
    ///
    /// Equivalent to `Live::builder(endpoint).spawn()`.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::builder(endpoint).spawn()
    }

    /// Creates a builder from environment variables.
    ///
    /// Reads `IROH_SECRET` for the secret key (generates one if unset),
    /// binds the iroh endpoint, and returns a [`LiveBuilder`].
    ///
    /// ```rust,no_run
    /// # async fn example() -> n0_error::Result<()> {
    /// use iroh_live::Live;
    /// // Minimal: no router, no gossip
    /// let live = Live::from_env().await?.spawn();
    /// // Full: router + gossip for rooms
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

    /// Mounts Live's protocol handlers on a RouterBuilder.
    ///
    /// Registers the MoQ ALPN handler and, if gossip was enabled,
    /// the gossip ALPN handler too.
    pub fn register_protocols(&self, router: RouterBuilder) -> RouterBuilder {
        let router = router.accept(iroh_moq::ALPN, self.moq.protocol_handler());
        if let Some(ref gossip) = self.gossip {
            return router.accept(iroh_gossip::ALPN, gossip.clone());
        }
        router
    }

    /// Returns the iroh endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the MoQ transport handle for direct/advanced operations.
    pub fn transport(&self) -> &Moq {
        &self.moq
    }

    /// Returns the gossip instance, if enabled.
    pub fn gossip(&self) -> Option<&Gossip> {
        self.gossip.as_ref()
    }

    /// Returns the MoQ protocol handler for manual Router mounting.
    pub fn protocol_handler(&self) -> MoqProtocolHandler {
        self.moq.protocol_handler()
    }

    /// Announces a broadcast so any peer (current + future) can subscribe.
    pub async fn publish(&self, name: impl ToString, broadcast: &LocalBroadcast) -> Result<()> {
        self.moq.publish(name, broadcast.producer()).await
    }

    /// Announces a broadcast producer directly.
    ///
    /// Prefer [`publish`](Self::publish) with a [`LocalBroadcast`] for the
    /// common case. This method is for advanced use where you have a raw producer.
    pub async fn publish_producer(
        &self,
        name: impl ToString,
        producer: moq_lite::BroadcastProducer,
    ) -> Result<()> {
        self.moq.publish(name, producer).await
    }

    /// Connects to a remote peer and subscribes to a named broadcast.
    ///
    /// Returns a [`Subscription`](crate::Subscription) that owns the session,
    /// broadcast, and network signal receiver. Stats recording and signal
    /// production are auto-wired.
    #[instrument("Subscribe", skip_all, fields(remote=tracing::field::Empty))]
    pub async fn subscribe(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
    ) -> Result<crate::Subscription> {
        let remote = remote.into();
        tracing::Span::current().record("remote", tracing::field::display(remote.id.fmt_short()));
        let mut session = self.moq.connect(remote).await?;
        info!(id=%session.conn().remote_id(), "connected");
        let consumer = session.subscribe(broadcast_name).await?;
        let broadcast = RemoteBroadcast::new(broadcast_name, consumer).await?;
        Ok(crate::Subscription::new(session, broadcast))
    }

    /// Connects to a remote peer, subscribes, and decodes video+audio in one step.
    ///
    /// Uses dynamic decoder dispatch. For explicit decoder selection, use
    /// [`subscribe_media_with_decoders`](Self::subscribe_media_with_decoders).
    pub async fn subscribe_media(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
        audio_backend: &dyn AudioStreamFactory,
        config: PlaybackConfig,
    ) -> Result<(MoqSession, MediaTracks)> {
        self.subscribe_media_with_decoders::<moq_media::codec::DefaultDecoders>(
            remote,
            broadcast_name,
            audio_backend,
            config,
        )
        .await
    }

    /// Connects to a remote peer, subscribes, and decodes video+audio in one step
    /// with a custom decoder type.
    pub async fn subscribe_media_with_decoders<D: moq_media::traits::Decoders>(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
        audio_backend: &dyn AudioStreamFactory,
        config: PlaybackConfig,
    ) -> Result<(MoqSession, MediaTracks)> {
        let sub = self.subscribe(remote, broadcast_name).await?;
        let track = sub
            .broadcast()
            .media_with_decoders::<D>(audio_backend, config)
            .await?;
        let (session, _, _) = sub.into_parts();
        Ok((session, track))
    }

    /// Joins a room using the given ticket.
    pub async fn join_room(&self, ticket: RoomTicket) -> Result<Room> {
        Room::new(self, ticket).await
    }

    /// Shuts down the Live instance, closing all sessions.
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
