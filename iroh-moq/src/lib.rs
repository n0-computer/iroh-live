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
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer};
use n0_error::{AnyError, Result, StdResultExt, anyerr, e, stack_error};
use n0_future::{
    FuturesUnordered, StreamExt,
    boxed::BoxFuture,
    task::{AbortOnDropHandle, JoinSet, spawn},
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, field, info, instrument};
use web_transport_iroh::SessionError;

// TODO: Use export from moq_lite after next update
/// The ALPN protocol identifier for MoQ-lite connections.
pub const ALPN: &[u8] = b"moq-lite-03";

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
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl Moq {
    /// Creates a new MoQ transport bound to the given endpoint.
    pub fn new(endpoint: Endpoint) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let (incoming_session_tx, _) = broadcast::channel(16);
        let actor = Actor::new(endpoint, incoming_session_tx.clone());
        let shutdown_token = actor.shutdown_token.clone();
        let actor_task =
            spawn(async move { actor.run(rx).await }.instrument(error_span!("LiveActor")));
        Self {
            shutdown_token,
            tx,
            incoming_session_tx,
            _actor_handle: Arc::new(AbortOnDropHandle::new(actor_task)),
        }
    }

    /// Returns a protocol handler for accepting incoming connections via a [`Router`](iroh::protocol::Router).
    pub fn protocol_handler(&self) -> MoqProtocolHandler {
        MoqProtocolHandler {
            tx: self.tx.clone(),
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
}

impl MoqProtocolHandler {
    async fn handle_connection(&self, connection: Connection) -> Result<(), Error> {
        info!(remote = %connection.remote_id().fmt_short(), "accepted");
        let session = web_transport_iroh::Session::raw(connection);
        let session = MoqSession::session_accept(session).await?;
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
    publish: OriginProducer,
    subscribe: OriginConsumer,
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
    ) -> Result<Self, Error> {
        let addr = remote_addr.into();
        tracing::Span::current().record("remote", field::display(addr.id.fmt_short()));
        let connection = endpoint.connect(addr, ALPN).await?;
        let wt_session = web_transport_iroh::Session::raw(connection);
        Self::session_connect(wt_session).await
    }

    /// Establishes a MoQ session as the client (initiator) over an existing WebTransport session.
    pub async fn session_connect(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
        let publish_prod = OriginProducer::new();
        let subscribe_prod = OriginProducer::new();
        let subscribe = subscribe_prod.consume();
        let client = moq_lite::Client::new()
            .with_publish(publish_prod.consume())
            .with_consume(subscribe_prod);
        let moq_session = client.connect(wt_session.clone()).await?;
        Ok(Self {
            publish: publish_prod,
            subscribe,
            wt_session,
            _moq_session: Arc::new(moq_session),
        })
    }

    /// Accepts a MoQ session as the server (responder) over an existing WebTransport session.
    pub async fn session_accept(wt_session: web_transport_iroh::Session) -> Result<Self, Error> {
        let publish_prod = OriginProducer::new();
        let subscribe_prod = OriginProducer::new();
        let subscribe = subscribe_prod.consume();
        let server = moq_lite::Server::new()
            .with_publish(publish_prod.consume())
            .with_consume(subscribe_prod);
        let moq_session = server.accept(wt_session.clone()).await?;
        Ok(Self {
            publish: publish_prod,
            subscribe,
            wt_session,
            _moq_session: Arc::new(moq_session),
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
        if let Some(consumer) = self.subscribe.consume_broadcast(name) {
            return Ok(consumer);
        }
        loop {
            let res = tokio::select! {
                res = self.subscribe.announced() => res,
                reason = self.wt_session.closed() => {
                    return Err(reason.into())
                }
            };
            let (path, consumer) = res.ok_or_else(|| e!(SubscribeError::NotAnnounced))?;
            debug!("peer announced broadcast: {path}");
            if path.as_str() == name {
                return consumer.ok_or_else(|| e!(SubscribeError::Closed));
            }
        }
    }

    /// Publishes a broadcast on this session, making it available to the remote peer.
    pub fn publish(&self, name: impl ToString, broadcast: BroadcastConsumer) {
        self.publish.publish_broadcast(name.to_string(), broadcast);
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
    ) -> Self {
        Self {
            endpoint,
            shutdown_token: CancellationToken::new(),
            incoming_session_tx,
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
                            self.handle_incoming_session(session);
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
            ActorMessage::HandleSession { session: msg } => self.handle_incoming_session(*msg),
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

    fn handle_incoming_session(&mut self, session: MoqSession) {
        let remote = session.remote_id();
        info!(remote=%remote.fmt_short(), "accepted incoming connection");
        for (name, producer) in self.publishing.iter() {
            session.publish(name.as_str(), producer.consume());
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
            session
                .publish
                .publish_broadcast(name.clone(), producer.consume());
        }
        let consume = producer.consume();
        self.publishing.insert(name.clone(), producer);
        self.publishing_closed_futs.push(Box::pin(async move {
            let closed = consume.closed();
            closed.await;
            name
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
                self.pending_connect_tasks.spawn(async move {
                    let res = MoqSession::connect(&endpoint, remote)
                        .await
                        .map_err(Into::into);
                    (remote_id, res)
                });
                entry.insert(Default::default()).push(reply);
            }
        }
    }
}
