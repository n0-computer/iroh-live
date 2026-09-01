use derive_more::Debug;
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::presets,
    protocol::{Router, RouterBuilder},
};
use iroh_gossip::Gossip;
use iroh_moq::{Moq, MoqProtocolHandler};
use moq_media::{publish::LocalBroadcast, subscribe::RemoteBroadcast};
use n0_error::Result;
use tracing::{error, info, instrument};

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
        // Every MoQ version this build speaks, not only the newest, so a peer
        // built against a different moq release still finds one in common.
        let handler = self.moq.protocol_handler();
        let mut router = router;
        for alpn in iroh_moq::alpns() {
            router = router.accept(alpn, handler.clone());
        }
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

    /// Creates a media broadcast at `path`, announced to every peer.
    ///
    /// Configure it through [`LocalBroadcast::video`] and
    /// [`LocalBroadcast::audio`]; peers reach it with [`subscribe`](Self::subscribe)
    /// under the same path.
    ///
    /// # Errors
    ///
    /// Fails if a broadcast already exists at `path`, or the catalog track
    /// cannot be created.
    pub fn publish(&self, path: impl moq_net::AsPath) -> Result<LocalBroadcast> {
        Ok(LocalBroadcast::new(self.moq.publish(path)?)?)
    }

    /// Creates a raw broadcast at `path`, without the media catalog.
    ///
    /// For a caller writing its own tracks, such as an importer replaying a
    /// file it already muxed.
    ///
    /// # Errors
    ///
    /// Fails if a broadcast already exists at `path`.
    pub fn publish_raw(&self, path: impl moq_net::AsPath) -> Result<moq_net::broadcast::Producer> {
        Ok(self.moq.publish(path)?)
    }

    /// Connects to a remote peer and subscribes to a named broadcast.
    ///
    /// Returns a [`Subscription`](crate::Subscription) that owns the
    /// [`MoqSession`], [`RemoteBroadcast`], and a network signals receiver.
    /// Stats recording and signal production are wired up automatically.
    #[instrument("Subscribe", skip_all, fields(remote=tracing::field::Empty))]
    pub async fn subscribe(
        &self,
        remote: impl Into<EndpointAddr>,
        path: &str,
    ) -> Result<crate::Subscription> {
        let remote = remote.into();
        tracing::Span::current().record("remote", tracing::field::display(remote.id.fmt_short()));
        let session = self.moq.connect(remote).await?;
        info!(id=%session.conn().remote_id(), "connected");
        let consumer = session.subscribe(path).await?;
        let broadcast = RemoteBroadcast::new(path, consumer).await?;
        Ok(crate::Subscription::new(session, broadcast))
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
