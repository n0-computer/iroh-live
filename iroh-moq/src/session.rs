//! Sessions with direct peers: dialing, admitting, and what a session offers.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use moq_net::{AsPath, origin};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tracing::{Instrument, debug, error, info, info_span, warn};

use crate::{
    ConnectOptions, Error, Grant, LinkId, LinkKind, LinkSample, OfferGuard, Publication,
    SessionRequest, Subscription,
    link::{self, LinkState},
    node::Shared,
    route,
    state::LinkEntry,
    transport,
};

/// Returns the moq hop id of the node with endpoint id `id`.
///
/// Derived rather than drawn at random per start, so a relay that saw this node
/// before recognizes its routes after a restart. An endpoint id is an ed25519
/// public key, whose bytes are already spread evenly, so the first eight are
/// used as they are. Truncated below 2^53, the bound moq documents for the
/// JavaScript clients, which read hop ids as numbers. Zero names nobody in moq,
/// so it maps to one.
pub(crate) fn hop_for(id: &EndpointId) -> moq_net::Hop {
    let bytes: [u8; 8] = id.as_bytes()[..8]
        .try_into()
        .expect("an endpoint id is 32 bytes");
    hop_from(u64::from_le_bytes(bytes))
}

/// Returns a hop from `value`, masked below 2^53 and never zero.
pub(crate) fn hop_from(value: u64) -> moq_net::Hop {
    moq_net::Hop::new((value & ((1u64 << 53) - 1)).max(1)).expect("non-zero and below 2^62")
}

/// The transport a MoQ session runs over.
///
/// web-transport-iroh implements the async transport interface, and moq-net
/// accepts only the poll one, so every session goes through moq-tokio's
/// adapter. It costs an allocation per operation and a copy per write; a
/// native poll implementation in web-transport-iroh would remove both.
pub(crate) type Transport = moq_tokio::transport::Session<web_transport_iroh::Session>;

/// Returns the instant a session's driver starts from.
///
/// From tokio's clock rather than `std`'s, because `moq_net::time::run` polls
/// the driver with tokio's, and a driver refuses time that moves backwards.
/// Under `tokio::time::pause` the two diverge, and seeding from `std` puts the
/// first poll behind the seed.
pub(crate) fn driver_now() -> std::time::Instant {
    tokio::time::Instant::now().into_std()
}

/// One session with one peer.
///
/// Cheap to clone; dropping the handles does not close it. It ends when either
/// side closes it, the connection fails, or the node shuts down.
#[derive(Clone)]
pub struct Session {
    pub(crate) inner: Arc<SessionInner>,
}

pub(crate) struct SessionInner {
    pub(crate) link: u64,
    pub(crate) remote: EndpointId,
    pub(crate) dialed: bool,
    pub(crate) grant: Grant,
    pub(crate) request: SessionRequest,
    pub(crate) connection: Connection,
    pub(crate) moq: moq_net::Session,
    pub(crate) ingest: origin::Producer,
    pub(crate) link_state: LinkState,
    pub(crate) shared: Weak<Shared>,
    /// Set by [`Session::close`], which closes asynchronously, so that a
    /// connect right after it dials anew rather than getting this session.
    pub(crate) closing: AtomicBool,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("remote", &self.inner.remote.fmt_short().to_string())
            .field("link", &self.inner.link)
            .field("dialed", &self.inner.dialed)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Session {}

impl Session {
    /// Returns the peer's endpoint id.
    pub fn remote_id(&self) -> EndpointId {
        self.inner.remote
    }

    /// Returns the id of the link this session is.
    ///
    /// The [`RouteInfo::via`](crate::RouteInfo::via) of every route that
    /// arrived over it, so a route can be matched to its session. Two sessions
    /// with one peer, one after the other, have different ids.
    pub fn link_id(&self) -> LinkId {
        LinkId(self.inner.link)
    }

    /// Reports whether this node dialed the session, rather than accepted it.
    ///
    /// Two peers that dial each other at once end up with one session of each
    /// kind, so this is what tells two sessions with one peer apart.
    pub fn dialed(&self) -> bool {
        self.inner.dialed
    }

    /// Returns what the peer may do on this node.
    pub fn grant(&self) -> Grant {
        self.inner.grant.clone()
    }

    /// Returns what the peer asked for when it opened the session.
    ///
    /// Empty for a session this node dialed.
    pub fn request(&self) -> &SessionRequest {
        &self.inner.request
    }

    /// Offers `publication` on this session, within its grant.
    ///
    /// Dropping the guard withdraws the offer, unless the publication's
    /// audience admits this peer anyway.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotGranted`] if the session's grant does not cover
    /// the publication's path, and with [`Error::ShutDown`] once the node has
    /// shut down.
    pub fn offer(&self, publication: &Publication) -> Result<OfferGuard, Error> {
        let shared = self
            .inner
            .shared
            .upgrade()
            .ok_or_else(|| e!(Error::ShutDown))?;
        if !self
            .inner
            .grant
            .allows_subscribe(publication.path().as_str())
        {
            return Err(e!(Error::NotGranted {
                path: publication.path().to_owned()
            }));
        }
        Ok(OfferGuard::new(&shared, publication.id(), self.inner.link))
    }

    /// Resolves `path` through this session only, whatever the route table holds.
    ///
    /// For a path that means something on this session alone. Waits for the
    /// peer to announce the path; cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotAnnounced`] if the session ends before the peer
    /// announces the path.
    pub async fn subscribe(&self, path: impl AsPath) -> Result<Subscription, Error> {
        let path = path.as_path().to_owned();
        let ingest = self.inner.ingest.consume();
        let broadcast = tokio::select! {
            resolved = ingest.routed_broadcast(&path) => match resolved {
                Ok(broadcast) => broadcast,
                Err(moq_net::Error::Closed) => return Err(e!(Error::NotAnnounced { path })),
                Err(source) => return Err(e!(Error::Unresolved { path, source })),
            },
            _ = self.inner.moq.closed() => return Err(e!(Error::NotAnnounced { path })),
        };
        Ok(Subscription::new(
            path,
            ingest,
            broadcast,
            Some(self.inner.link),
            self.inner.shared.clone(),
        ))
    }

    /// Returns the link as this session's connection monitor last read it.
    pub fn link(&self) -> LinkSample {
        self.inner.link_state.get()
    }

    /// Returns the QUIC connection under the session.
    ///
    /// Integration point: follows iroh's versioning.
    pub fn connection(&self) -> &Connection {
        &self.inner.connection
    }

    /// Closes the session, and every subscription over it, in both directions.
    ///
    /// `reason` is logged here; the peer sees a clean close.
    pub fn close(&self, reason: &str) {
        info!(remote = %self.inner.remote.fmt_short(), reason, "closing session");
        self.inner.closing.store(true, Ordering::Release);
        self.inner.moq.abort(moq_net::Error::Cancel);
    }

    /// Reports whether the session is closed or on its way to closing.
    pub(crate) fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::Acquire) || self.inner.connection.close_reason().is_some()
    }

    /// Waits until the session ends, and returns why.
    ///
    /// A close either side asked for is [`Error::SessionClosed`] with
    /// [`moq_net::Error::Cancel`] as its source. Cancellation safe.
    pub async fn closed(&self) -> Error {
        let source = self.inner.moq.closed().await;
        e!(Error::SessionClosed { source })
    }
}

/// A session's two origins: what its peer is offered, and what it announces.
pub(crate) struct Origins {
    pub(crate) publish: origin::Producer,
    pub(crate) publish_driver: origin::Driver,
    pub(crate) ingest: origin::Producer,
    pub(crate) ingest_driver: origin::Driver,
}

impl Origins {
    /// Creates both origins under the node's identity.
    pub(crate) fn new(shared: &Shared) -> Self {
        let (publish, publish_driver) = origin::Producer::new(shared.table.config());
        let (ingest, ingest_driver) = origin::Producer::new(shared.table.config());
        Self {
            publish,
            publish_driver,
            ingest,
            ingest_driver,
        }
    }

    /// Runs both drivers for as long as the handles live, and returns the
    /// ingest origin.
    pub(crate) fn run(self) -> (origin::Producer, [AbortOnDropHandle<()>; 2]) {
        let run = |driver| {
            AbortOnDropHandle::new(tokio::spawn(async move {
                moq_net::time::run(driver).await;
            }))
        };
        let drivers = [run(self.publish_driver), run(self.ingest_driver)];
        (self.ingest, drivers)
    }

    /// Returns the ingest origin scoped to what `grant` lets the peer publish.
    ///
    /// `None` if it lets the peer publish nothing.
    pub(crate) fn subscriber(&self, grant: &Grant) -> Option<origin::Producer> {
        if grant.publish.is_empty() {
            return None;
        }
        match self.ingest.scope("", &grant.publish) {
            Ok(scoped) => Some(scoped),
            Err(err) => {
                warn!(%err, "the grant's publish patterns fit no path; ingesting nothing");
                None
            }
        }
    }
}

/// Everything a session needs to run, produced by a dial or an admission.
pub(crate) struct SessionParts {
    pub(crate) remote: EndpointId,
    pub(crate) connection: Connection,
    pub(crate) dialed: bool,
    pub(crate) grant: Grant,
    pub(crate) request: SessionRequest,
    pub(crate) moq: moq_net::Session,
    pub(crate) driver: moq_net::Driver<Transport>,
    pub(crate) origins: Origins,
}

/// Dials `remote` and completes the MoQ handshake as the client.
pub(crate) async fn dial_session(
    shared: Arc<Shared>,
    remote: EndpointAddr,
    options: ConnectOptions,
) -> Result<SessionParts, Error> {
    let remote_id = remote.id;
    let grant = options
        .grant
        .clone()
        .unwrap_or_else(|| shared.grant_for(remote_id));
    let transport = transport::dial_with(&shared.endpoint, remote, &options).await?;
    let connection = transport.conn().clone();
    let origins = Origins::new(&shared);
    let mut client = moq_net::Client::new()
        .with_publisher(origins.publish.consume())
        .with_peer_hop(hop_for(&remote_id));
    if let Some(subscriber) = origins.subscriber(&grant) {
        client = client.with_subscriber(subscriber);
    }
    if let Some(cost) = options.cost {
        client = client.with_cost(cost);
    }
    // Raw QUIC carries the token in the setup path; an H3 session already put
    // it in its CONNECT URL and must not send one.
    if !transport.is_h3()
        && let Some(path) = options.setup_path()
    {
        client = client.with_path(path);
    }
    let (moq, driver) = client
        .connect(driver_now(), Transport::new(transport.session))
        .await
        .map_err(|source| match source {
            moq_net::Error::Unauthorized | moq_net::Error::App(_) => e!(Error::Refused { source }),
            source => e!(Error::Moq { source }),
        })?;
    Ok(SessionParts {
        remote: remote_id,
        connection,
        dialed: true,
        grant,
        request: SessionRequest::default(),
        moq,
        driver,
        origins,
    })
}

/// How long [`Moq::shutdown`] gives a session to tell its peer it is closing.
///
/// A close is one packet and needs no answer, so this is a round trip's grace
/// and not a negotiation.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Returns a copy of `err` for one of several callers waiting on one dial.
fn share(err: &Error) -> Error {
    match err {
        Error::Connect { source, .. } => e!(Error::Connect {
            source: source.clone()
        }),
        Error::UnsupportedAlpn { alpn, .. } => e!(Error::UnsupportedAlpn { alpn: alpn.clone() }),
        Error::Moq { source, .. } => e!(Error::Moq {
            source: source.clone()
        }),
        Error::Refused { source, .. } => e!(Error::Refused {
            source: source.clone()
        }),
        Error::ShutDown { .. } => e!(Error::ShutDown),
        other => e!(Error::Connect {
            source: Arc::new(AnyError::from_display(other))
        }),
    }
}

/// What the node asks of its actor.
pub(crate) enum ActorMessage {
    Connect {
        remote: EndpointAddr,
        options: ConnectOptions,
        reply: oneshot::Sender<Result<Session, Error>>,
    },
    Register {
        parts: Box<SessionParts>,
        reply: oneshot::Sender<Result<Session, Error>>,
    },
}

/// Owns session lifecycle: dials, coalesced connects, and the session tasks.
pub(crate) struct Actor {
    shared: Arc<Shared>,
    /// Every live session per peer, oldest first.
    ///
    /// Normally one; a simultaneous dial leaves two, and the first is the one
    /// `connect` hands out. Keeping the second rather than dropping it is what
    /// lets it be promoted when the first ends.
    peers: HashMap<EndpointId, Vec<Session>>,
    sessions: JoinSet<moq_net::Error>,
    /// What each session task runs, so a task that panics is still cleaned up.
    session_ids: HashMap<tokio::task::Id, (u64, EndpointId)>,
    pending: HashMap<EndpointId, Vec<oneshot::Sender<Result<Session, Error>>>>,
    dials: JoinSet<Result<SessionParts, Error>>,
    dial_ids: HashMap<tokio::task::Id, EndpointId>,
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Actor {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            peers: HashMap::new(),
            sessions: JoinSet::new(),
            session_ids: HashMap::new(),
            pending: HashMap::new(),
            dials: JoinSet::new(),
            dial_ids: HashMap::new(),
        }
    }

    pub(crate) async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
        let shutdown = self.shared.shutdown.clone();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => {
                    info!(sessions = self.sessions.len(), "shutting down");
                    break;
                }
                message = inbox.recv() => match message {
                    Some(message) => self.handle(message),
                    None => break,
                },
                Some(ended) = self.sessions.join_next_with_id(), if !self.sessions.is_empty() => {
                    let (id, result) = match ended {
                        Ok((id, err)) => (id, Ok(err)),
                        Err(err) => (err.id(), Err(err)),
                    };
                    self.session_ended(id, result);
                }
                Some(dialed) = self.dials.join_next_with_id(), if !self.dials.is_empty() => {
                    match dialed {
                        Ok((id, result)) => self.dialed(id, result),
                        Err(err) => {
                            let id = err.id();
                            error!(%err, "dial task failed");
                            let failure = e!(Error::Connect {
                                source: Arc::new(AnyError::from_display(&err))
                            });
                            self.dialed(id, Err(failure));
                        }
                    }
                }
            }
        }
        self.drain().await;
        // The rest happens in `Drop`, which also runs if this task panics.
    }

    /// Tears the node's state down and reports the shutdown done.
    ///
    /// In `Drop` so that it also runs when the actor panics: otherwise
    /// [`Moq::shutdown`] would wait forever for `done`.
    fn finish(&mut self) {
        // A lock poisoned by the panic that brought us here must not turn this
        // into a second panic during unwinding.
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
            state.publications.clear();
            state.links.clear();
        }
        self.shared.sessions.set(Vec::new()).ok();
        // Anyone still waiting on a dial learns it will not come.
        for (_, replies) in self.pending.drain() {
            for reply in replies {
                reply.send(Err(e!(Error::ShutDown))).ok();
            }
        }
        self.shared.done.set(true).ok();
    }

    /// Waits for every session to flush its close, within [`SHUTDOWN_GRACE`].
    ///
    /// Dropping the tasks instead would abort them mid-flush and leave peers to
    /// notice by timing out. Sessions that outlive the wait are aborted, because
    /// a peer that stopped reading must not hold the shutdown open.
    async fn drain(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let drained = tokio::time::timeout(SHUTDOWN_GRACE, async {
            while self.sessions.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            warn!(
                remaining = self.sessions.len(),
                grace = ?SHUTDOWN_GRACE,
                "sessions did not close in time, aborting them",
            );
        }
    }

    fn handle(&mut self, message: ActorMessage) {
        match message {
            ActorMessage::Connect {
                remote,
                options,
                reply,
            } => self.connect(remote, options, reply),
            ActorMessage::Register { parts, reply } => {
                let session = self.register(*parts);
                reply.send(Ok(session)).ok();
            }
        }
    }

    fn connect(
        &mut self,
        remote: EndpointAddr,
        options: ConnectOptions,
        reply: oneshot::Sender<Result<Session, Error>>,
    ) {
        let id = remote.id;
        if self.shared.shutdown.is_cancelled() {
            reply.send(Err(e!(Error::ShutDown))).ok();
            return;
        }
        if let Some(session) = self.live_session(&id) {
            reply.send(Ok(session)).ok();
            return;
        }
        let waiting = self.pending.entry(id).or_default();
        waiting.push(reply);
        if waiting.len() > 1 {
            return;
        }
        info!(remote = %id.fmt_short(), "dialing");
        let handle = self.dials.spawn(
            dial_session(self.shared.clone(), remote, options)
                .instrument(info_span!("dial", remote = %id.fmt_short())),
        );
        self.dial_ids.insert(handle.id(), id);
    }

    /// Returns the oldest session with `peer` that is neither closed nor
    /// closing.
    ///
    /// A session stays listed until its task lands, a scheduling hop after the
    /// connection went, so the front of the list can be one on its way out.
    fn live_session(&self, peer: &EndpointId) -> Option<Session> {
        self.peers.get(peer).and_then(|sessions| {
            sessions
                .iter()
                .find(|session| !session.is_closing())
                .cloned()
        })
    }

    fn dialed(&mut self, id: tokio::task::Id, result: Result<SessionParts, Error>) {
        let Some(remote) = self.dial_ids.remove(&id) else {
            return;
        };
        match result {
            Ok(parts) => {
                info!(remote = %remote.fmt_short(), "connected");
                self.register(parts);
            }
            Err(err) => {
                info!(remote = %remote.fmt_short(), %err, "dial failed");
                for reply in self.pending.remove(&remote).into_iter().flatten() {
                    reply.send(Err(share(&err))).ok();
                }
            }
        }
    }

    /// Starts running an established session and makes it reachable.
    fn register(&mut self, parts: SessionParts) -> Session {
        let SessionParts {
            remote,
            connection,
            dialed,
            grant,
            request,
            moq,
            driver,
            origins,
        } = parts;
        let link_state = LinkState::default();
        let mut state = self.shared.state.lock().expect("poisoned");
        let link = state.next_id();
        let session = Session {
            inner: Arc::new(SessionInner {
                link,
                remote,
                dialed,
                grant: grant.clone(),
                request,
                connection: connection.clone(),
                moq: moq.clone(),
                ingest: origins.ingest.clone(),
                link_state: link_state.clone(),
                shared: Arc::downgrade(&self.shared),
                closing: Default::default(),
            }),
        };
        state.add_link(
            link,
            LinkEntry {
                kind: LinkKind::Direct,
                remote: Some(remote),
                grant,
                publish: origins.publish.clone(),
                public: true,
                consume: true,
                offers: HashMap::new(),
                announced: Default::default(),
                session: Some(session.clone()),
                link_state: link_state.clone(),
            },
        );
        drop(state);

        // Two peers that dial each other at once each end up with two
        // sessions. Both are kept and driven; the oldest is the one `connect`
        // hands out. Closing the loser is tempting and wrong: the two sides
        // see the collision at different instants, so one may already have
        // handed the other's loser to a caller.
        let sessions = self.peers.entry(remote).or_default();
        if !sessions.is_empty() {
            debug!(remote = %remote.fmt_short(), "simultaneous connect; serving the first session");
        }
        sessions.push(session.clone());
        self.publish_sessions();
        if let Some(serving) = self.live_session(&remote) {
            for reply in self.pending.remove(&remote).into_iter().flatten() {
                reply.send(Ok(serving.clone())).ok();
            }
        }

        info!(remote = %remote.fmt_short(), link, dialed, "session started");
        let shared = self.shared.clone();
        let cancel = self.shared.shutdown.child_token();
        let task_session = session.clone();
        let handle = self.sessions.spawn(
            async move {
                let (ingest, _drivers) = origins.run();
                let _bridge = AbortOnDropHandle::new(tokio::spawn(
                    route::bridge(shared, link, ingest, false).in_current_span(),
                ));
                let _monitor = AbortOnDropHandle::new(tokio::spawn(link::monitor(
                    link::Source::Direct(moq.clone(), connection),
                    link_state,
                    cancel.child_token(),
                )));
                let run = moq_net::time::run(driver);
                tokio::pin!(run);
                let err = tokio::select! {
                    _ = cancel.cancelled() => {
                        moq.abort(moq_net::Error::Cancel);
                        (&mut run).await
                    }
                    err = &mut run => err,
                };
                drop(task_session);
                err
            }
            .instrument(info_span!("session", remote = %remote.fmt_short(), link)),
        );
        self.session_ids.insert(handle.id(), (link, remote));
        session
    }

    fn session_ended(
        &mut self,
        id: tokio::task::Id,
        result: Result<moq_net::Error, tokio::task::JoinError>,
    ) {
        let Some((link, remote)) = self.session_ids.remove(&id) else {
            return;
        };
        match result {
            Ok(moq_net::Error::Closed | moq_net::Error::Cancel) => {
                info!(remote = %remote.fmt_short(), link, "session closed");
            }
            Ok(err) => info!(remote = %remote.fmt_short(), link, %err, "session ended"),
            Err(err) => error!(remote = %remote.fmt_short(), link, %err, "session task failed"),
        }
        if let Some(sessions) = self.peers.get_mut(&remote) {
            sessions.retain(|session| session.inner.link != link);
            if sessions.is_empty() {
                self.peers.remove(&remote);
            }
        }
        self.shared
            .state
            .lock()
            .expect("poisoned")
            .remove_link(link);
        self.publish_sessions();
    }

    fn publish_sessions(&self) {
        let sessions: Vec<Session> = self.peers.values().flatten().cloned().collect();
        self.shared.sessions.set(sessions).ok();
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn the_hop_is_stable_and_in_range() {
        let id = SecretKey::generate().public();
        let hop = hop_for(&id);
        assert_eq!(hop, hop_for(&id), "the same id gives the same hop");
        assert!(hop.id() > 0 && hop.id() < 1 << 53, "{}", hop.id());
        assert_ne!(
            hop,
            hop_for(&SecretKey::generate().public()),
            "two ids give two hops"
        );
    }
}
