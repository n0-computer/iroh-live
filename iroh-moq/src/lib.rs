//! MoQ transport layer over iroh.
//!
//! Provides [`Moq`] for managing sessions and [`MoqSession`] for
//! publish/subscribe operations over QUIC connections. Uses an internal
//! actor for connection deduplication and broadcast routing.

use std::{
    collections::{HashMap, hash_map},
    fmt,
    sync::Arc,
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{ConnectError, Connection, ConnectionError, WriteError},
    protocol::{AcceptError, ProtocolHandler},
};
use moq_lite::{
    Origin,
    broadcast::{Consumer as BroadcastConsumer, Producer as BroadcastProducer},
    origin::{Consumer as OriginConsumer, Producer as OriginProducer},
};
use n0_error::{AnyError, Result, StdResultExt, anyerr, e, stack_error};
use n0_future::{
    FuturesUnordered, StreamExt,
    boxed::BoxFuture,
    task::{AbortOnDropHandle, JoinSet, spawn},
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, field, info, instrument, warn};
use web_transport_iroh::SessionError;

/// The ALPN protocol identifier for MoQ-lite connections.
///
/// `moq-lite-04` is the current wire version: it carries real origin ids in
/// announce hop chains (lite-03 sent anonymous `UNKNOWN` placeholders) and
/// supports `AnnounceInterest.exclude_hop` for sender-side loop suppression.
pub const ALPN: &[u8] = b"moq-lite-04";

#[stack_error(derive, add_meta, from_sources)]
#[allow(private_interfaces, reason = "trait impl uses private types")]
pub enum Error {
    #[error(transparent)]
    Connect(ConnectError),
    #[error(transparent)]
    Moq(#[error(source, std_err)] moq_lite::Error),
    #[error(transparent)]
    Server(#[error(source, std_err)] web_transport_iroh::ServerError),
    #[error("internal consistency error")]
    InternalConsistencyError(#[error(source)] LiveActorDiedError),
    #[error("failed to perform request")]
    Request(#[error(source, std_err)] WriteError),
}

#[stack_error(derive, add_meta, from_sources)]
#[allow(private_interfaces, reason = "trait impl uses private types")]
pub enum SubscribeError {
    #[error("track was not announced")]
    NotAnnounced,
    #[error("track was closed")]
    Closed,
    #[error("session was closed")]
    SessionClosed(#[error(source, std_err)] SessionError),
}

#[stack_error(derive)]
#[error("live actor died")]
struct LiveActorDiedError;

impl From<mpsc::error::SendError<ActorMessage>> for LiveActorDiedError {
    fn from(_value: mpsc::error::SendError<ActorMessage>) -> Self {
        Self
    }
}

/// MoQ transport layer managing sessions, broadcasts, and subscriptions.
///
/// Runs an internal actor that handles connection lifecycle, broadcast
/// announcements, and subscription routing.
#[derive(Debug, Clone)]
pub struct Moq {
    tx: mpsc::Sender<ActorMessage>,
    incoming_session_tx: broadcast::Sender<MoqSession>,
    shutdown_token: CancellationToken,
    origin: Origin,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl Moq {
    /// Creates a new MoQ transport bound to the given endpoint.
    pub fn new(endpoint: Endpoint) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let (incoming_session_tx, _) = broadcast::channel(16);
        // One non-zero origin id identifies this node in broadcast announce hop
        // chains. It is created once here and shared across every session this
        // node opens or accepts, matching the per-node identity that relays use
        // for loop detection and shortest-path routing.
        let origin = Origin::random();
        let actor = Actor::new(endpoint, incoming_session_tx.clone(), origin);
        let shutdown_token = actor.shutdown_token.clone();
        let actor_task =
            spawn(async move { actor.run(rx).await }.instrument(error_span!("LiveActor")));
        Self {
            shutdown_token,
            tx,
            incoming_session_tx,
            origin,
            _actor_handle: Arc::new(AbortOnDropHandle::new(actor_task)),
        }
    }

    /// Returns a protocol handler for accepting incoming connections via a [`Router`](iroh::protocol::Router).
    pub fn protocol_handler(&self) -> MoqProtocolHandler {
        MoqProtocolHandler {
            tx: self.tx.clone(),
            origin: self.origin,
        }
    }

    /// Publishes a broadcast with the given name, making it available to all connected peers.
    pub async fn publish(&self, name: impl ToString, producer: BroadcastProducer) -> Result<()> {
        self.tx
            .send(ActorMessage::LocalBroadcast {
                broadcast_name: name.to_string(),
                producer,
            })
            .await
            .std_context("live actor died")?;
        Ok(())
    }

    /// Returns the names of all currently published broadcasts.
    pub async fn published_broadcasts(&self) -> Vec<String> {
        let (reply, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ActorMessage::GetPublished { reply })
            .await
            .is_err()
        {
            return vec![];
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Connects to a remote peer and returns a [`MoqSession`] for publish/subscribe.
    pub async fn connect(&self, remote: impl Into<EndpointAddr>) -> Result<MoqSession, AnyError> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Connect {
                remote: remote.into(),
                reply,
            })
            .await
            .map_err(|_| LiveActorDiedError)?;
        reply_rx
            .await
            .map_err(|_| LiveActorDiedError)?
            .map_err(|err| anyerr!(err))
    }

    /// Returns a stream of incoming MoQ sessions from remote peers.
    pub fn incoming_sessions(&self) -> IncomingSessionStream {
        IncomingSessionStream {
            rx: self.incoming_session_tx.subscribe(),
        }
    }

    /// Shuts down the transport, closing all sessions.
    pub fn shutdown(&self) {
        self.shutdown_token.cancel();
    }
}

/// Protocol handler for accepting incoming MoQ connections.
///
/// Register with a [`Router`](iroh::protocol::Router) via
/// `.accept(ALPN, handler)`.
#[derive(Debug, Clone)]
pub struct MoqProtocolHandler {
    tx: mpsc::Sender<ActorMessage>,
    origin: Origin,
}

impl MoqProtocolHandler {
    async fn handle_connection(&self, connection: Connection) -> Result<(), Error> {
        info!(remote = %connection.remote_id().fmt_short(), "accepted");
        let session = web_transport_iroh::Session::raw(connection);
        let session = MoqSession::session_accept(session, self.origin).await?;
        self.tx
            .send(ActorMessage::HandleSession {
                session: Box::new(session),
            })
            .await
            .map_err(LiveActorDiedError::from)?;
        Ok(())
    }
}

impl ProtocolHandler for MoqProtocolHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.handle_connection(connection)
            .await
            .map_err(AnyError::from)?;
        Ok(())
    }
}

/// Stream of incoming MoQ sessions.
#[derive(Debug)]
pub struct IncomingSessionStream {
    rx: broadcast::Receiver<MoqSession>,
}

impl IncomingSessionStream {
    /// Returns the next incoming session, or `None` if the transport is shut down.
    pub async fn next(&mut self) -> Option<IncomingSession> {
        loop {
            match self.rx.recv().await {
                Ok(session) => return Some(IncomingSession { session }),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    info!("incoming session stream lagged, skipped {n} sessions");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Incoming MoQ session, not yet fully accepted by the application.
///
/// The MoQ handshake has already completed. The application can inspect
/// the remote peer's identity before deciding to accept or reject.
#[derive(Debug)]
pub struct IncomingSession {
    session: MoqSession,
}

impl IncomingSession {
    /// Returns the remote peer's endpoint ID.
    pub fn remote_id(&self) -> EndpointId {
        self.session.remote_id()
    }

    /// Accepts the session, returning the [`MoqSession`] for publish/subscribe.
    pub fn accept(self) -> MoqSession {
        self.session
    }

    /// Rejects the session, closing the connection.
    pub fn reject(self) {
        self.session.close(1u32, b"rejected");
    }
}

/// MoQ session with a remote peer.
///
/// Supports publishing local broadcasts and subscribing to remote ones.
/// Created via [`Moq::connect`] or from an [`IncomingSession`].
#[derive(Clone)]
pub struct MoqSession {
    wt_session: web_transport_iroh::Session,
    _moq_session: Arc<moq_lite::Session>,
    _driver: Arc<AbortOnDropHandle<moq_lite::Result<()>>>,
    forwards: Forwards,
    publish: OriginProducer,
    subscribe: OriginConsumer,
}

/// Live announcements for the broadcasts this session serves, keyed by name.
type Forwards = Arc<std::sync::Mutex<HashMap<String, BroadcastAnnouncement>>>;

/// An announced broadcast, served for as long as this value is held.
///
/// Dropping it stops serving and finishes the broadcast, so subscribers see a clean
/// end rather than a dropped producer.
#[derive(Debug)]
#[must_use = "dropping this stops serving the announced broadcast"]
pub struct BroadcastAnnouncement {
    _task: AbortOnDropHandle<()>,
}

/// Announces `upstream` on `origin` under `name` and serves it in the background.
///
/// This replaces moq-lite 0.1's `origin::Producer::publish_broadcast(name, consumer)`.
/// See [`forward_broadcast`] for why the work is needed at all.
pub fn announce_broadcast(
    origin: &OriginProducer,
    name: &str,
    upstream: BroadcastConsumer,
) -> Result<BroadcastAnnouncement, moq_lite::Error> {
    let announced = origin.create_broadcast(name, moq_lite::broadcast::Route::announced())?;
    Ok(BroadcastAnnouncement {
        _task: AbortOnDropHandle::new(spawn(forward_broadcast(announced, upstream))),
    })
}

/// Announces `upstream` to the remote peer under `name` and serves it.
///
/// moq-lite 0.1 had `origin::Producer::publish_broadcast(name, consumer)`, which
/// installed an existing consumer under a name and announced it in one step. 0.2
/// inverted the ownership: `create_broadcast` mints a producer the origin owns, and
/// there is no public way to hand an existing consumer to an announced path. So we
/// create the announced broadcast and forward into it ourselves — the same shape
/// moq-net's own subscriber uses when it re-serves a remote broadcast locally.
///
/// The announcement is the part that matters. A peer's subscriber only builds its
/// local source when an announcement arrives, so a dynamically served (unannounced)
/// broadcast is never reachable across a session, however correct it looks locally.
pub(crate) async fn forward_broadcast(
    // Held for the task's lifetime: dropping the producer finishes the broadcast.
    announced: moq_lite::broadcast::Producer,
    upstream: BroadcastConsumer,
) {
    let mut dynamic = announced.dynamic();
    let mut tracks = JoinSet::new();
    while let Ok(request) = dynamic.requested_track().await {
        // Reap finished track forwarders; a long-lived broadcast can serve many.
        while tracks.try_join_next().is_some() {}

        let name = request.name().to_string();
        match upstream.track(&name) {
            Ok(track) => {
                tracks.spawn(forward_track(track, request));
            }
            Err(err) => {
                debug!(%name, "rejecting unknown track: {err:#}");
                request.reject(err);
            }
        }
    }
}

/// Copies one track's groups and frames from `upstream` into the requested track.
///
/// The downstream request's own preferences are carried upstream, and the request is
/// only accepted once upstream resolves, so the accepted track reports the metadata
/// the source actually has rather than a default. A failure to subscribe upstream
/// rejects the request instead of accepting a track that can never produce.
///
/// Upstream group sequences are preserved rather than re-numbered, so a subscriber
/// still sees the publisher's ordering and any gaps it needs for skip decisions.
async fn forward_track(upstream: moq_lite::track::Consumer, request: moq_lite::track::Request) {
    let mut subscriber = match upstream.subscribe(request.subscription()).await {
        Ok(subscriber) => subscriber,
        Err(err) => {
            request.reject(err);
            return;
        }
    };
    let mut downstream = request.accept(subscriber.info().clone());
    let mut groups = JoinSet::new();
    let outcome = loop {
        // Reap finished copies so the set does not grow for the life of the track.
        while groups.try_join_next().is_some() {}

        match subscriber.next_group().await {
            Ok(Some(group)) => {
                let sequence = group.sequence;
                match downstream.create_group(moq_lite::group::Info { sequence }) {
                    Ok(out) => {
                        groups.spawn(forward_group(group, out));
                    }
                    Err(err) => break Err(err),
                }
            }
            Ok(None) => break Ok(()),
            Err(err) => break Err(err),
        }
    };

    // Let the frames already in flight land before closing the track: dropping the
    // JoinSet aborts its tasks, which would truncate the groups still being copied.
    while groups.join_next().await.is_some() {}

    match outcome {
        Ok(()) => {
            let _ = downstream.finish();
        }
        Err(err) => {
            let _ = downstream.abort(err);
        }
    }
}

/// Copies one group's frames, preserving each frame's timestamp.
async fn forward_group(
    mut upstream: moq_lite::group::Consumer,
    mut downstream: moq_lite::group::Producer,
) {
    loop {
        match upstream.read_frame().await {
            Ok(Some(frame)) => {
                if let Err(err) = downstream.write_frame(frame.timestamp, frame.payload) {
                    let _ = downstream.abort(err);
                    return;
                }
            }
            Ok(None) => {
                let _ = downstream.finish();
                return;
            }
            Err(err) => {
                let _ = downstream.abort(err);
                return;
            }
        }
    }
}

impl fmt::Debug for MoqSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqSession")
            .field("remote_id", &self.wt_session.remote_id())
            .finish_non_exhaustive()
    }
}

impl MoqSession {
    #[instrument(skip_all, fields(remote=field::Empty))]
    pub async fn connect(
        endpoint: &Endpoint,
        remote_addr: impl Into<EndpointAddr>,
        origin: Origin,
    ) -> Result<Self, Error> {
        let addr = remote_addr.into();
        tracing::Span::current().record("remote", field::display(addr.id.fmt_short()));
        let connection = endpoint.connect(addr, ALPN).await?;
        let wt_session = web_transport_iroh::Session::raw(connection);
        Self::session_connect(wt_session, origin).await
    }

    /// Establishes a MoQ session as the client (initiator) over an existing WebTransport session.
    ///
    /// `origin` is this node's identity, stamped onto the broadcasts it announces
    /// to the peer. The same id is used for the publish and subscribe sides so
    /// loop detection sees one consistent hop for this node. It must be non-zero:
    /// zero is the reserved `UNKNOWN` placeholder and is never encoded on the wire.
    pub async fn session_connect(
        wt_session: web_transport_iroh::Session,
        origin: Origin,
    ) -> Result<Self, Error> {
        let publish_prod = origin.produce();
        let subscribe_prod = origin.produce();
        let subscribe = subscribe_prod.consume();
        let client = moq_lite::Client::new()
            .with_publisher(publish_prod.consume())
            .with_subscriber(subscribe_prod);
        let (moq_session, driver) = client.connect(wt_session.clone()).await?;
        let driver = AbortOnDropHandle::new(spawn(driver));
        Ok(Self {
            publish: publish_prod,
            subscribe,
            wt_session,
            _moq_session: Arc::new(moq_session),
            _driver: Arc::new(driver),
            forwards: Forwards::default(),
        })
    }

    /// Accepts a MoQ session as the server (responder) over an existing WebTransport session.
    ///
    /// See [`session_connect`](Self::session_connect) for the role of `origin`.
    pub async fn session_accept(
        wt_session: web_transport_iroh::Session,
        origin: Origin,
    ) -> Result<Self, Error> {
        let publish_prod = origin.produce();
        let subscribe_prod = origin.produce();
        let subscribe = subscribe_prod.consume();
        let server = moq_lite::Server::new()
            .with_publisher(publish_prod.consume())
            .with_subscriber(subscribe_prod);
        let (moq_session, driver) = server.accept(wt_session.clone()).await?;
        let driver = AbortOnDropHandle::new(spawn(driver));
        Ok(Self {
            publish: publish_prod,
            subscribe,
            wt_session,
            _moq_session: Arc::new(moq_session),
            _driver: Arc::new(driver),
            forwards: Forwards::default(),
        })
    }

    /// Returns the remote peer's endpoint ID.
    pub fn remote_id(&self) -> EndpointId {
        self.wt_session.remote_id()
    }

    /// Returns a reference to the underlying QUIC connection.
    pub fn conn(&self) -> &Connection {
        self.wt_session.conn()
    }

    /// Subscribes to a named broadcast from the remote peer.
    ///
    /// Waits for the remote to announce the broadcast if not yet available.
    /// Returns when the session closes if the name is never announced.
    /// Callers that need a timeout should wrap this in `tokio::time::timeout`.
    pub async fn subscribe(&mut self, name: &str) -> Result<BroadcastConsumer, SubscribeError> {
        if let Some(reason) = self.conn().close_reason() {
            return Err(SessionError::from(reason).into());
        }
        match self.subscribe.announced_broadcast(name).await {
            None => Err(e!(SubscribeError::Closed)),
            Some(consumer) => Ok(consumer),
        }
    }

    /// Publishes a broadcast on this session, making it available to the remote peer.
    pub fn publish(
        &self,
        name: impl ToString,
        broadcast: BroadcastConsumer,
    ) -> Result<(), moq_lite::Error> {
        let name = name.to_string();
        let announcement = announce_broadcast(&self.publish, &name, broadcast)?;
        self.forwards
            .lock()
            .expect("forwards poisoned")
            .insert(name, announcement);
        Ok(())
    }

    /// Returns the origin producer for advanced publish operations.
    pub fn origin_producer(&self) -> &OriginProducer {
        &self.publish
    }

    /// Returns the origin consumer for advanced subscribe operations.
    pub fn origin_consumer(&self) -> &OriginConsumer {
        &self.subscribe
    }

    /// Closes the session with an error code and reason.
    pub fn close(&self, error_code: u32, reason: &[u8]) {
        self.wt_session.close(error_code, reason);
    }

    /// Waits until the session is closed by either side.
    pub async fn closed(&self) -> web_transport_iroh::SessionError {
        self.wt_session.closed().await
    }
}

enum ActorMessage {
    HandleSession {
        session: Box<MoqSession>,
    },
    LocalBroadcast {
        broadcast_name: BroadcastName,
        producer: BroadcastProducer,
    },
    Connect {
        remote: EndpointAddr,
        reply: oneshot::Sender<Result<MoqSession, Arc<AnyError>>>,
    },
    GetPublished {
        reply: oneshot::Sender<Vec<BroadcastName>>,
    },
}

type BroadcastName = String;
type PendingConnects = HashMap<EndpointId, Vec<oneshot::Sender<Result<MoqSession, Arc<AnyError>>>>>;

struct Actor {
    endpoint: Endpoint,
    shutdown_token: CancellationToken,
    incoming_session_tx: broadcast::Sender<MoqSession>,
    origin: Origin,
    publishing: HashMap<BroadcastName, BroadcastProducer>,
    publishing_closed_futs: FuturesUnordered<BoxFuture<BroadcastName>>,
    sessions: HashMap<EndpointId, MoqSession>,
    session_tasks: JoinSet<(EndpointId, Result<(), web_transport_iroh::SessionError>)>,
    pending_connects: PendingConnects,
    pending_connect_tasks: JoinSet<(EndpointId, Result<MoqSession, AnyError>)>,
}

impl Actor {
    pub(crate) fn new(
        endpoint: Endpoint,
        incoming_session_tx: broadcast::Sender<MoqSession>,
        origin: Origin,
    ) -> Self {
        Self {
            endpoint,
            shutdown_token: CancellationToken::new(),
            incoming_session_tx,
            origin,
            publishing: Default::default(),
            publishing_closed_futs: Default::default(),
            sessions: Default::default(),
            session_tasks: Default::default(),
            pending_connects: Default::default(),
            pending_connect_tasks: Default::default(),
        }
    }

    pub(crate) async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
        loop {
            tokio::select! {
                msg = inbox.recv() => {
                    match msg {
                        None => break,
                        Some(msg) => self.handle_message(msg)
                    }
                }
                Some(res) = self.session_tasks.join_next(), if !self.session_tasks.is_empty() => {
                    match res {
                        Ok((endpoint_id, res)) => {
                            info!(remote=%endpoint_id.fmt_short(), "session closed: {res:?}");
                            self.sessions.remove(&endpoint_id);
                        }
                        Err(err) => tracing::error!("session task panicked: {err}"),
                    }
                }
                Some(name) = self.publishing_closed_futs.next(), if !self.publishing_closed_futs.is_empty() => {
                    self.publishing.remove(&name);
                }
                Some(res) = self.pending_connect_tasks.join_next(), if !self.pending_connect_tasks.is_empty() => {
                    match res {
                        Err(err) => tracing::error!("connect task panicked: {err}"),
                        Ok((endpoint_id, Ok(session))) => {
                            info!(remote=%endpoint_id.fmt_short(), "connected");
                            self.handle_session(session);
                        }
                        Ok((endpoint_id, Err(err))) => {
                            info!(remote=%endpoint_id.fmt_short(), "connect failed: {err:#}");
                            let replies = self.pending_connects.remove(&endpoint_id).into_iter().flatten();
                            let err = Arc::new(err);
                            for reply in replies {
                                reply.send(Err(err.clone())).ok();
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_message(&mut self, msg: ActorMessage) {
        match msg {
            ActorMessage::HandleSession { session } => {
                info!(remote=%session.remote_id().fmt_short(), "accepted incoming connection");
                self.handle_session(*session);
            }
            ActorMessage::LocalBroadcast {
                broadcast_name: name,
                producer,
            } => self.handle_publish_broadcast(name, producer),
            ActorMessage::Connect { remote, reply } => self.handle_connect(remote, reply),
            ActorMessage::GetPublished { reply } => {
                let names = self.publishing.keys().cloned().collect();
                reply.send(names).ok();
            }
        }
    }

    fn handle_session(&mut self, session: MoqSession) {
        let remote = session.remote_id();
        for (name, producer) in self.publishing.iter() {
            if let Err(err) = session.publish(name.as_str(), producer.consume()) {
                warn!(%name, "failed to announce broadcast to new session: {err:#}");
            }
        }
        self.sessions.insert(remote, session.clone());
        // Notify incoming session subscribers (best-effort, ok if no receivers).
        self.incoming_session_tx.send(session.clone()).ok();
        for reply in self.pending_connects.remove(&remote).into_iter().flatten() {
            reply.send(Ok(session.clone())).ok();
        }

        let shutdown = self.shutdown_token.child_token();
        self.session_tasks.spawn(async move {
            let res = tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!(remote=%remote.fmt_short(), "closing session: cancelled");
                    session.close(0u32, b"cancelled");
                    Ok(())
                }
                result = session.closed() => match result {
                    SessionError::ConnectionError(ConnectionError::LocallyClosed) => Ok(()),
                    err => Err(err)
                },
            };
            debug!(remote=%remote.fmt_short(), "closing session: {res:?}");
            (remote, res)
        });
    }

    fn handle_publish_broadcast(&mut self, name: BroadcastName, producer: BroadcastProducer) {
        for session in self.sessions.values_mut() {
            if let Err(err) = session.publish(name.clone(), producer.consume()) {
                warn!(%name, "failed to announce broadcast to session: {err:#}");
            }
        }
        let consume = producer.consume();
        let closed_name = name.clone();
        self.publishing.insert(name, producer);
        self.publishing_closed_futs.push(Box::pin(async move {
            let closed = consume.closed();
            closed.await;
            closed_name
        }));
    }

    fn handle_connect(
        &mut self,
        remote: EndpointAddr,
        reply: oneshot::Sender<Result<MoqSession, Arc<AnyError>>>,
    ) {
        let remote_id = remote.id;
        if let Some(session) = self.sessions.get(&remote_id) {
            reply.send(Ok(session.clone())).ok();
            return;
        }
        match self.pending_connects.entry(remote_id) {
            hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().push(reply);
            }
            hash_map::Entry::Vacant(entry) => {
                let endpoint = self.endpoint.clone();
                let origin = self.origin;
                self.pending_connect_tasks.spawn(async move {
                    let res = MoqSession::connect(&endpoint, remote, origin)
                        .await
                        .map_err(Into::into);
                    (remote_id, res)
                });
                entry.insert(Default::default()).push(reply);
            }
        }
    }
}
