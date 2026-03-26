use std::sync::Arc;

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
#[derive(Clone)]
pub struct Live {
    endpoint: Endpoint,
    moq: Arc<Moq>,
    gossip: Option<Arc<Gossip>>,
    router: Option<Arc<Router>>,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("endpoint", &self.endpoint.id())
            .finish_non_exhaustive()
    }
}

/// Builder for [`Live`].
#[derive(Debug)]
pub struct LiveBuilder {
    endpoint: Endpoint,
    gossip: Option<Arc<Gossip>>,
}

impl LiveBuilder {
    /// Enables gossip (required for rooms). Creates a Gossip instance
    /// internally and auto-mounts it via [`Live::register_protocols`].
    pub fn enable_gossip(mut self) -> Self {
        self.gossip = Some(Arc::new(Gossip::builder().spawn(self.endpoint.clone())));
        self
    }

    /// Sets an externally-managed gossip instance (for rooms).
    ///
    /// Use this instead of `enable_gossip()` when you manage Gossip
    /// yourself. You are responsible for mounting it on the Router
    /// (it will NOT be auto-mounted by `register_protocols`).
    pub fn gossip(mut self, gossip: Gossip) -> Self {
        self.gossip = Some(Arc::new(gossip));
        self
    }

    /// Creates the MoQ transport and optional gossip. Does NOT create a Router.
    ///
    /// Use [`Live::register_protocols`] to mount on your own Router.
    pub fn spawn(self) -> Live {
        let moq = Moq::new(self.endpoint.clone());
        Live {
            endpoint: self.endpoint,
            moq: Arc::new(moq),
            gossip: self.gossip,
            router: None,
        }
    }

    /// Creates the MoQ transport, optional gossip, AND a Router.
    ///
    /// The Router is stored on Live and kept alive for its lifetime.
    /// All Live protocols are auto-mounted.
    pub fn spawn_with_router(self) -> Live {
        let endpoint = self.endpoint.clone();
        let mut live = self.spawn();
        let router = live.register_protocols(Router::builder(endpoint));
        live.router = Some(Arc::new(router.spawn()));
        live
    }
}

impl Live {
    /// Returns a builder. Always starts from an existing endpoint.
    pub fn builder(endpoint: Endpoint) -> LiveBuilder {
        LiveBuilder {
            endpoint,
            gossip: None,
        }
    }

    /// Creates a new Live instance without a builder.
    ///
    /// Equivalent to `Live::builder(endpoint).spawn()`.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::builder(endpoint).spawn()
    }

    /// Creates a new node from environment variables.
    ///
    /// Reads `IROH_SECRET` for the secret key (generates one if unset),
    /// binds an endpoint, and spawns with gossip and a protocol router.
    pub async fn from_env() -> Result<Self> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(crate::util::secret_key_from_env()?)
            .bind()
            .await?;
        info!(endpoint_id=%endpoint.id(), "endpoint bound");

        let live = Self::builder(endpoint).enable_gossip().spawn_with_router();
        Ok(live)
    }

    /// Mounts Live's protocol handlers on a RouterBuilder.
    ///
    /// Registers the MoQ ALPN handler and, if gossip was enabled via
    /// `enable_gossip()`, the gossip ALPN handler too.
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
        self.gossip.as_deref()
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
    /// Returns both the session (for stats, closing, etc.) and the broadcast.
    /// If you only need the broadcast, use [`subscribe`](Self::subscribe).
    #[instrument("Subscribe", skip_all, fields(remote=tracing::field::Empty))]
    pub async fn subscribe(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
    ) -> Result<(MoqSession, RemoteBroadcast)> {
        let remote = remote.into();
        tracing::Span::current().record("remote", tracing::field::display(remote.id.fmt_short()));
        let mut session = self.moq.connect(remote).await?;
        info!(id=%session.conn().remote_id(), "connected");
        let consumer = session.subscribe(broadcast_name).await?;
        let broadcast = RemoteBroadcast::new(broadcast_name, consumer).await?;
        Ok((session, broadcast))
    }

    /// Subscribes and starts both stat recording and network signal production.
    ///
    /// Returns the session, broadcast, and a signal receiver for adaptive
    /// rendition selection. Equivalent to calling [`subscribe`](Self::subscribe)
    /// followed by [`spawn_stats_recorder`](crate::util::spawn_stats_recorder)
    /// and [`spawn_signal_producer`](crate::util::spawn_signal_producer).
    pub async fn subscribe_with_stats(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
    ) -> Result<(
        MoqSession,
        RemoteBroadcast,
        tokio::sync::watch::Receiver<moq_media::net::NetworkSignals>,
    )> {
        let (session, broadcast) = self.subscribe(remote, broadcast_name).await?;
        crate::util::spawn_stats_recorder(
            session.conn(),
            broadcast.stats().net.clone(),
            broadcast.shutdown_token(),
        );
        let signals =
            crate::util::spawn_signal_producer(session.conn(), broadcast.shutdown_token());
        Ok((session, broadcast, signals))
    }

    /// Connects to a remote peer, subscribes, and decodes video+audio in one step.
    ///
    /// Uses dynamic decoder dispatch. For explicit decoder selection, use
    /// [`subscribe_media_with_decoders`](Self::subscribe_media_with_decoders).
    #[cfg(any_codec)]
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
        let (session, broadcast) = self.subscribe(remote, broadcast_name).await?;
        let track = broadcast
            .media_with_decoders::<D>(audio_backend, config)
            .await?;
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
