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
    gossip: GossipChoice,
    with_router: bool,
}

/// Where the [`Gossip`] instance comes from, if there is one.
///
/// One choice rather than an `Option` beside a flag, so the last call wins and
/// no combination of [`LiveBuilder::with_gossip`] and [`LiveBuilder::gossip`]
/// can mean two things at once.
#[derive(Debug, Default)]
enum GossipChoice {
    /// Rooms are not in use, so nothing is spawned or mounted.
    #[default]
    Disabled,
    /// Spawn one on the endpoint when the builder does.
    Internal,
    /// Use the caller's, which they already spawned.
    External(#[debug(skip)] Gossip),
}

impl LiveBuilder {
    /// Enables gossip, which is required for room membership.
    ///
    /// Spawns a [`Gossip`] instance on the endpoint and mounts it on the
    /// [`Router`] if [`with_router`](Self::with_router) is also set. Overrides
    /// an earlier [`gossip`](Self::gossip).
    pub fn with_gossip(mut self) -> Self {
        self.gossip = GossipChoice::Internal;
        self
    }

    /// Uses a [`Gossip`] instance the caller already spawned.
    ///
    /// The alternative to [`with_gossip`](Self::with_gossip), and it overrides
    /// an earlier call to it. Mounting still follows
    /// [`with_router`](Self::with_router): the builder's own router mounts
    /// whichever instance it ends up with, and a caller running its own router
    /// mounts it there through [`Live::register_protocols`].
    pub fn gossip(mut self, gossip: Gossip) -> Self {
        self.gossip = GossipChoice::External(gossip);
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
        let gossip = match self.gossip {
            GossipChoice::Disabled => None,
            GossipChoice::Internal => Some(Gossip::builder().spawn(self.endpoint.clone())),
            GossipChoice::External(gossip) => Some(gossip),
        };

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
            gossip: GossipChoice::default(),
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
    pub fn register_protocols(&self, mut router: RouterBuilder) -> RouterBuilder {
        // Every MoQ version this build speaks, not only the newest, so a peer
        // built against a different moq release still finds one in common.
        let handler = self.moq.protocol_handler();
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
    /// [`MoqSession`](iroh_moq::MoqSession), [`RemoteBroadcast`], and the
    /// transport signals that drive rendition adaptation.
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
    /// closes the iroh [`Endpoint`] unconditionally. [`Live`] is [`Clone`] and
    /// every clone shares one endpoint, so this shuts down all of them.
    pub async fn shutdown(&self) {
        self.moq.shutdown();
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
