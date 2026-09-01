//! MoQ transport layer over iroh.
//!
//! [`Moq`] binds an iroh [`Endpoint`] to a MoQ origin: broadcasts created with
//! [`Moq::publish`] are announced to every peer, and [`MoqSession`] reaches the
//! ones a peer announces back. An internal actor owns session lifetime, so a
//! second [`Moq::connect`] to a peer we already have a session with returns that
//! session rather than opening a second connection.

use std::{
    collections::{HashMap, hash_map},
    fmt,
    sync::Arc,
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{AlpnError, ConnectError, ConnectWithOptsError, ConnectingError, Connection},
    protocol::{AcceptError, ProtocolHandler},
};
use moq_net::{AsPath, Origin, broadcast, origin};
use n0_error::{AnyError, Result, e, stack_error};
use n0_future::task::{AbortOnDropHandle, JoinSet, spawn};
use tokio::sync::{broadcast as tokio_broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error_span, field, info, instrument};

/// The ALPN this node prefers, the newest MoQ version it speaks.
///
/// A peer that only speaks an older one still connects: [`ALPNS`] carries the
/// whole list and both the dial and the router registration offer all of it.
pub const ALPN: &[u8] = moq_net::ALPNS[0].as_bytes();

/// Every ALPN this node accepts, newest first, plus HTTP/3.
///
/// Register all of them on a [`Router`](iroh::protocol::Router) so a peer built
/// against a different moq release still finds a version in common. A single
/// hardcoded ALPN is an interop bug that only shows up once the two sides drift,
/// which is exactly when it is hardest to diagnose.
///
/// HTTP/3 is last because WebTransport over H3 needs framing that not every H3
/// endpoint supports, so it is the fallback rather than the preference.
pub fn alpns() -> Vec<&'static [u8]> {
    moq_net::ALPNS
        .iter()
        .map(|alpn| alpn.as_bytes())
        .chain(std::iter::once(web_transport_iroh::ALPN_H3.as_bytes()))
        .collect()
}

/// The route every locally published broadcast is created with: announced, so
/// peers discover it without asking for the path by name.
fn announced_route() -> broadcast::Route {
    broadcast::Route::new().with_announce(true)
}

#[stack_error(derive, add_meta, from_sources)]
#[allow(private_interfaces, reason = "trait impl uses private types")]
pub enum Error {
    #[error(transparent)]
    Connect(ConnectError),
    #[error(transparent)]
    Connecting(ConnectingError),
    #[error(transparent)]
    ConnectWithOpts(ConnectWithOptsError),
    #[error(transparent)]
    Alpn(AlpnError),
    #[error(transparent)]
    Moq(#[error(source, std_err)] moq_net::Error),
    #[error(transparent)]
    Client(#[error(source, std_err)] web_transport_iroh::ClientError),
    #[error(transparent)]
    Server(#[error(source, std_err)] web_transport_iroh::ServerError),
    #[error("the peer negotiated an ALPN this build does not speak: {alpn}")]
    UnsupportedAlpn {
        /// What the peer chose.
        alpn: String,
    },
    #[error("failed to dial the peer")]
    Dial {
        /// Why the dial failed. Shared because several callers can coalesce
        /// onto one dial, and each of them wants the reason.
        #[error(source, std_err)]
        source: Arc<Self>,
    },
    #[error("internal consistency error")]
    InternalConsistencyError(#[error(source)] LiveActorDiedError),
}

#[stack_error(derive, add_meta, from_sources)]
pub enum SubscribeError {
    #[error("broadcast was never announced")]
    NotAnnounced,
}

#[stack_error(derive)]
#[error("live actor died")]
struct LiveActorDiedError;

impl From<mpsc::error::SendError<ActorMessage>> for LiveActorDiedError {
    fn from(_value: mpsc::error::SendError<ActorMessage>) -> Self {
        Self
    }
}

/// MoQ transport for one iroh endpoint.
///
/// Owns the node's publish origin and an actor that handles connection
/// lifecycle. Cheap to clone; every clone shares the same origin and actor.
#[derive(Clone)]
pub struct Moq {
    tx: mpsc::Sender<ActorMessage>,
    incoming_session_tx: tokio_broadcast::Sender<MoqSession>,
    shutdown_token: CancellationToken,
    origin: origin::Producer,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl fmt::Debug for Moq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Moq").finish_non_exhaustive()
    }
}

impl Moq {
    /// Creates a new MoQ transport bound to the given endpoint.
    pub fn new(endpoint: Endpoint) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let (incoming_session_tx, _) = tokio_broadcast::channel(16);
        // One non-zero origin id identifies this node in broadcast announce hop
        // chains. It is created once here and shared across every session this
        // node opens or accepts, matching the per-node identity that relays use
        // for loop detection and shortest-path routing.
        let origin = Origin::random().produce();
        let actor = Actor::new(endpoint, incoming_session_tx.clone(), origin.clone());
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
            origin: self.origin.clone(),
        }
    }

    /// Creates a broadcast at `path`, announced to every peer.
    ///
    /// Write media through the returned producer, and end it with
    /// [`finish`](broadcast::Producer::finish) so subscribers see a clean close
    /// rather than a dropped broadcast. Peers reach it with
    /// [`MoqSession::subscribe`] under the same path.
    ///
    /// # Errors
    ///
    /// Fails if a broadcast already exists at `path`.
    pub fn publish(&self, path: impl AsPath) -> Result<broadcast::Producer, Error> {
        Ok(self.origin.create_broadcast(path, announced_route())?)
    }

    /// Returns the origin every published broadcast is created on.
    ///
    /// For callers that need more than [`publish`](Self::publish) offers, such
    /// as an unannounced broadcast reachable only by exact path.
    pub fn origin(&self) -> &origin::Producer {
        &self.origin
    }

    /// Connects to a remote peer and returns a [`MoqSession`] for publish/subscribe.
    ///
    /// Connections are deduplicated: two calls for the same peer share one
    /// session, and concurrent calls coalesce onto a single dial.
    pub async fn connect(&self, remote: impl Into<EndpointAddr>) -> Result<MoqSession, Error> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Connect {
                remote: remote.into(),
                reply,
            })
            .await
            .map_err(|_| LiveActorDiedError)?;
        // The error is shared because several callers can coalesce onto one
        // dial, and each of them wants the reason it failed.
        reply_rx
            .await
            .map_err(|_| LiveActorDiedError)?
            .map_err(|source| e!(Error::Dial { source }))
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
#[derive(Clone)]
pub struct MoqProtocolHandler {
    tx: mpsc::Sender<ActorMessage>,
    origin: origin::Producer,
}

impl fmt::Debug for MoqProtocolHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqProtocolHandler").finish_non_exhaustive()
    }
}

impl MoqProtocolHandler {
    async fn handle_connection(&self, connection: Connection) -> Result<(), Error> {
        let alpn = String::from_utf8_lossy(connection.alpn()).into_owned();
        info!(remote = %connection.remote_id().fmt_short(), %alpn, "accepted");
        let transport = accept_transport(connection, &alpn).await?;
        let (session, driver) = MoqSession::accept(transport, &self.origin).await?;
        self.tx
            .send(ActorMessage::HandleSession {
                session: Box::new(session),
                driver: Box::new(driver),
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

/// Completes the client half of the WebTransport handshake the negotiated ALPN
/// calls for.
async fn connect_transport(
    connection: Connection,
    alpn: &str,
) -> Result<web_transport_iroh::Session, Error> {
    if alpn == web_transport_iroh::ALPN_H3 {
        // The CONNECT target only has to identify the endpoint; iroh already
        // dialed a specific peer, so the host is the one it dialed.
        let url: url::Url = format!("https://{}/", connection.remote_id())
            .parse()
            .expect("an endpoint id is a valid host");
        let mut request = web_transport_proto::ConnectRequest::new(url);
        for alpn in moq_net::ALPNS {
            request = request.with_protocol(alpn.to_string());
        }
        return Ok(web_transport_iroh::Session::connect_h3(connection, request).await?);
    }
    if !moq_net::ALPNS.contains(&alpn) {
        return Err(e!(Error::UnsupportedAlpn {
            alpn: alpn.to_string(),
        }));
    }
    Ok(web_transport_iroh::Session::raw(connection))
}

/// Completes the server half of the WebTransport handshake the negotiated ALPN
/// calls for.
///
/// Raw QUIC carries the MoQ stream directly; H3 has to answer a CONNECT first.
async fn accept_transport(
    connection: Connection,
    alpn: &str,
) -> Result<web_transport_iroh::Session, Error> {
    if alpn == web_transport_iroh::ALPN_H3 {
        let request = web_transport_iroh::H3Request::accept(connection).await?;
        let mut response = web_transport_proto::ConnectResponse::OK;
        if let Some(protocol) = request.protocols.first() {
            response = response.with_protocol(protocol);
        }
        return Ok(request.respond(response).await?);
    }
    // The handler is public and mountable on any ALPN, so an unknown one is a
    // named error rather than a raw session that will fail to parse a setup.
    if !moq_net::ALPNS.contains(&alpn) {
        return Err(e!(Error::UnsupportedAlpn {
            alpn: alpn.to_string(),
        }));
    }
    Ok(web_transport_iroh::Session::raw(connection))
}

/// Sessions accepted from remote peers, in arrival order.
///
/// A session is already established and already usable by the peer by the time
/// it appears here: the MoQ handshake completes in the protocol handler, before
/// the application sees anything. This reports what happened, it does not gate
/// it. An application that needs to refuse a peer does so at the iroh layer,
/// before the ALPN is accepted.
#[derive(Debug)]
pub struct IncomingSessionStream {
    rx: tokio_broadcast::Receiver<MoqSession>,
}

impl IncomingSessionStream {
    /// Returns the next accepted session, or `None` once the transport shuts
    /// down.
    pub async fn next(&mut self) -> Option<MoqSession> {
        loop {
            match self.rx.recv().await {
                Ok(session) => return Some(session),
                Err(tokio_broadcast::error::RecvError::Lagged(n)) => {
                    info!("incoming session stream lagged, skipped {n} sessions");
                    continue;
                }
                Err(tokio_broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// MoQ session with a remote peer.
///
/// Reaches the broadcasts that peer announces. Everything this node publishes
/// travels over it without further wiring, because every session shares the
/// node origin. Created via [`Moq::connect`] or from an [`IncomingSession`].
#[derive(Clone)]
pub struct MoqSession {
    connection: Connection,
    session: moq_net::Session,
    subscribe: origin::Consumer,
    /// Whether this node dialed, as opposed to accepted.
    dialed: bool,
}

impl fmt::Debug for MoqSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqSession")
            .field("remote_id", &self.connection.remote_id())
            .finish_non_exhaustive()
    }
}

impl MoqSession {
    /// Dials `remote_addr` and completes the MoQ handshake as the client.
    ///
    /// Returns the session together with its protocol driver, which the caller
    /// must poll to completion or the session makes no progress.
    #[instrument(skip_all, fields(remote=field::Empty))]
    pub async fn connect(
        endpoint: &Endpoint,
        remote_addr: impl Into<EndpointAddr>,
        origin: &origin::Producer,
    ) -> Result<(Self, moq_net::Driver), Error> {
        let addr = remote_addr.into();
        tracing::Span::current().record("remote", field::display(addr.id.fmt_short()));
        // Offer every version rather than only the newest, so a peer built
        // against an older moq release still finds one in common.
        let others: Vec<Vec<u8>> = alpns()[1..].iter().map(|alpn| alpn.to_vec()).collect();
        let options = iroh::endpoint::ConnectOptions::new().with_additional_alpns(others);
        let mut connecting = endpoint.connect_with_opts(addr, ALPN, options).await?;
        let alpn = String::from_utf8_lossy(&connecting.alpn().await?).into_owned();
        let connection = connecting.await?;
        debug!(%alpn, "negotiated");
        let transport = connect_transport(connection.clone(), &alpn).await?;
        let subscribe = origin.info().produce();
        let (session, driver) = moq_net::Client::new()
            .with_publisher(origin.consume())
            .with_subscriber(subscribe.clone())
            .connect(transport)
            .await?;
        Ok((Self::new(connection, session, &subscribe, true), driver))
    }

    /// Completes the MoQ handshake as the server over an accepted connection.
    ///
    /// See [`connect`](Self::connect) for the driver contract.
    pub async fn accept(
        transport: web_transport_iroh::Session,
        origin: &origin::Producer,
    ) -> Result<(Self, moq_net::Driver), Error> {
        let connection = transport.conn().clone();
        let subscribe = origin.info().produce();
        let (session, driver) = moq_net::Server::new()
            .with_publisher(origin.consume())
            .with_subscriber(subscribe.clone())
            .accept(transport)
            .await?;
        Ok((Self::new(connection, session, &subscribe, false), driver))
    }

    fn new(
        connection: Connection,
        session: moq_net::Session,
        subscribe: &origin::Producer,
        dialed: bool,
    ) -> Self {
        Self {
            connection,
            session,
            subscribe: subscribe.consume(),
            dialed,
        }
    }

    /// Returns the remote peer's endpoint ID.
    pub fn remote_id(&self) -> EndpointId {
        self.connection.remote_id()
    }

    /// Returns a reference to the underlying QUIC connection.
    pub fn conn(&self) -> &Connection {
        &self.connection
    }

    /// Returns the MoQ session, for its stats and bandwidth estimates.
    pub fn session(&self) -> &moq_net::Session {
        &self.session
    }

    /// Reports whether this node dialed the connection, rather than accepting
    /// it.
    ///
    /// Two peers that dial at once end up with one of each, so this is what
    /// tells two sessions to the same peer apart.
    pub fn dialed(&self) -> bool {
        self.dialed
    }

    /// Subscribes to a broadcast the remote peer announces at `path`.
    ///
    /// Waits for the announce if it has not arrived yet, and reports
    /// [`SubscribeError::NotAnnounced`] when the session ends first. Callers
    /// that need a deadline wrap this in `tokio::time::timeout`.
    pub async fn subscribe(
        &self,
        path: impl AsPath,
    ) -> Result<broadcast::Consumer, SubscribeError> {
        match self.subscribe.announced_broadcast(path).await {
            Some(consumer) => Ok(consumer),
            None => Err(e!(SubscribeError::NotAnnounced)),
        }
    }

    /// Returns the origin carrying everything the remote peer announces.
    pub fn announced(&self) -> &origin::Consumer {
        &self.subscribe
    }

    /// Closes the session with the given reason.
    pub fn close(&self, err: moq_net::Error) {
        self.session.abort(err);
    }

    /// Waits until the session is closed by either side.
    pub async fn closed(&self) -> moq_net::Error {
        self.session.closed().await
    }
}

enum ActorMessage {
    HandleSession {
        session: Box<MoqSession>,
        driver: Box<moq_net::Driver>,
    },
    Connect {
        remote: EndpointAddr,
        reply: oneshot::Sender<Result<MoqSession, Arc<Error>>>,
    },
}

type PendingConnects = HashMap<EndpointId, Vec<oneshot::Sender<Result<MoqSession, Arc<Error>>>>>;
type ConnectResult = (EndpointId, Result<(MoqSession, moq_net::Driver), Error>);

struct Actor {
    endpoint: Endpoint,
    shutdown_token: CancellationToken,
    incoming_session_tx: tokio_broadcast::Sender<MoqSession>,
    origin: origin::Producer,
    sessions: HashMap<EndpointId, MoqSession>,
    /// Which session generation currently owns each peer's slot, so a task
    /// ending after its successor arrived does not evict the live one.
    generations: HashMap<EndpointId, u64>,
    generation: u64,
    session_tasks: JoinSet<(EndpointId, u64, Result<(), moq_net::Error>)>,
    pending_connects: PendingConnects,
    pending_connect_tasks: JoinSet<ConnectResult>,
}

impl Actor {
    fn new(
        endpoint: Endpoint,
        incoming_session_tx: tokio_broadcast::Sender<MoqSession>,
        origin: origin::Producer,
    ) -> Self {
        Self {
            endpoint,
            shutdown_token: CancellationToken::new(),
            incoming_session_tx,
            origin,
            sessions: Default::default(),
            generations: Default::default(),
            generation: 0,
            session_tasks: Default::default(),
            pending_connects: Default::default(),
            pending_connect_tasks: Default::default(),
        }
    }

    async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
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
                        Ok((endpoint_id, generation, res)) => {
                            info!(remote=%endpoint_id.fmt_short(), "session closed: {res:?}");
                            // Only evict if this is still the session in the
                            // slot; a replacement may already have taken it.
                            if self.generations.get(&endpoint_id) == Some(&generation) {
                                self.sessions.remove(&endpoint_id);
                                self.generations.remove(&endpoint_id);
                            }
                        }
                        Err(err) => tracing::error!("session task panicked: {err}"),
                    }
                }
                Some(res) = self.pending_connect_tasks.join_next(), if !self.pending_connect_tasks.is_empty() => {
                    match res {
                        Err(err) => tracing::error!("connect task panicked: {err}"),
                        Ok((endpoint_id, Ok((session, driver)))) => {
                            info!(remote=%endpoint_id.fmt_short(), "connected");
                            self.handle_session(session, driver);
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
            ActorMessage::HandleSession { session, driver } => {
                info!(remote=%session.remote_id().fmt_short(), "accepted incoming connection");
                self.handle_session(*session, *driver);
            }
            ActorMessage::Connect { remote, reply } => self.handle_connect(remote, reply),
        }
    }

    fn handle_session(&mut self, session: MoqSession, driver: moq_net::Driver) {
        let remote = session.remote_id();

        // Two peers that dial each other at the same time each end up with two
        // connections: one they dialed and one they accepted. Keep the first to
        // arrive as the one `connect` hands out, and leave the second running.
        //
        // Closing the second is tempting and wrong. The two sides do not see
        // the collision at the same instant, so by the time one closes its
        // loser the other may already have handed that same connection to a
        // caller, whose subscribe then dies. A deterministic tie-break does not
        // help, because the handles are given out before the collision is even
        // visible. The second connection costs one idle QUIC connection until
        // either peer goes away, which is the cheaper of the two failures.
        if self.sessions.contains_key(&remote) {
            debug!(
                remote = %remote.fmt_short(),
                "simultaneous connect; keeping the first session",
            );
            let existing = self.sessions[&remote].clone();
            for reply in self.pending_connects.remove(&remote).into_iter().flatten() {
                reply.send(Ok(existing.clone())).ok();
            }
            // Still drive it: the peer may be using this one, and an undriven
            // session stalls rather than closing.
            self.spawn_driver(remote, session, driver);
            return;
        }

        // The generation tells a session task's exit apart from a later
        // session's entry, so a task that ends after its successor took the
        // slot does not evict the live one.
        self.generation += 1;
        self.generations.insert(remote, self.generation);
        self.sessions.insert(remote, session.clone());
        // Notify incoming session subscribers (best-effort, ok if no receivers).
        self.incoming_session_tx.send(session.clone()).ok();
        for reply in self.pending_connects.remove(&remote).into_iter().flatten() {
            reply.send(Ok(session.clone())).ok();
        }

        self.spawn_driver(remote, session, driver);
    }

    /// Runs a session's protocol driver until the session ends.
    fn spawn_driver(&mut self, remote: EndpointId, session: MoqSession, driver: moq_net::Driver) {
        // The driver runs the protocol; without it the session makes no
        // progress. Both handles are held here, so the session ends when this
        // task does: on shutdown, or when the peer closes.
        let generation = self.generation;
        let shutdown = self.shutdown_token.child_token();
        self.session_tasks.spawn(async move {
            tokio::pin!(driver);
            let res = tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!(remote=%remote.fmt_short(), "closing session: cancelled");
                    session.close(moq_net::Error::Cancel);
                    (&mut driver).await
                }
                result = &mut driver => result,
            };
            // A local close is how shutdown ends, not a failure to report.
            let res = match res {
                Err(moq_net::Error::Cancel) => Ok(()),
                other => other,
            };
            debug!(remote=%remote.fmt_short(), "session ended: {res:?}");
            (remote, generation, res)
        });
    }

    fn handle_connect(
        &mut self,
        remote: EndpointAddr,
        reply: oneshot::Sender<Result<MoqSession, Arc<Error>>>,
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
                let origin = self.origin.clone();
                self.pending_connect_tasks.spawn(async move {
                    let res = MoqSession::connect(&endpoint, remote, &origin).await;
                    (remote_id, res)
                });
                entry.insert(Default::default()).push(reply);
            }
        }
    }
}
