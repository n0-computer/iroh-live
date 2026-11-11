//! The actor that runs a node's sessions and dials.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use iroh::{EndpointAddr, EndpointId};
use n0_error::{AnyError, e};
use n0_future::{StreamExt, boxed::BoxStream, task::AbortOnDropHandle};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tracing::{Instrument, debug, error, info, info_span, warn};

use crate::{
    ConnectOptions, Error, LinkId, LinkKind, Session,
    admission::decide,
    handshake::{Hop, SessionParts, dial_session},
    link::{self, LinkState},
    node::Shared,
    policy::{Authorization, DynPolicy, PeerRequest, Rejection},
    reauth::{Finished, Reauth},
    session::SessionInner,
    state::LinkEntry,
};

/// The result of one re-decide: the peer, the generation it started at, and the decision.
type Redecided = (EndpointId, u64, Result<Authorization, Rejection>);

/// How long [`Node::shutdown`] gives a session to tell its peer it is closing.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

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

/// Runs the node's sessions and dials, and coalesces connects.
pub(crate) struct Actor {
    shared: Arc<Shared>,
    /// Every live session per peer, oldest first.
    ///
    /// A simultaneous dial leaves two. `connect` hands out the first, and the
    /// second takes over when the first ends.
    peers: HashMap<EndpointId, Vec<Session>>,
    sessions: JoinSet<moq_net::Error>,
    /// What each session task runs, so a task that panics is still cleaned up.
    session_ids: HashMap<tokio::task::Id, (LinkId, EndpointId)>,
    pending: HashMap<EndpointId, Vec<oneshot::Sender<Result<Session, Error>>>>,
    dials: JoinSet<Result<SessionParts, Error>>,
    dial_ids: HashMap<tokio::task::Id, EndpointId>,
    /// When each peer is decided again, and which decisions are current.
    reauth: Reauth,
    /// The re-decides in flight.
    decisions: JoinSet<Redecided>,
    /// What each re-decide task runs, so a task that panics still finishes.
    decision_ids: HashMap<tokio::task::Id, (EndpointId, u64)>,
    /// The policy's change stream, until it ends.
    changes: Option<BoxStream<()>>,
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Actor {
    /// Creates an actor for the node `shared`.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        let changes = DynPolicy::changes(&*shared.policy);
        Self {
            shared,
            peers: HashMap::new(),
            sessions: JoinSet::new(),
            session_ids: HashMap::new(),
            pending: HashMap::new(),
            dials: JoinSet::new(),
            dial_ids: HashMap::new(),
            reauth: Reauth::default(),
            decisions: JoinSet::new(),
            decision_ids: HashMap::new(),
            changes,
        }
    }

    /// Runs the actor until the node shuts down or every handle to it drops.
    pub(crate) async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
        let shutdown = self.shared.shutdown.clone();
        loop {
            let deadline = self.reauth.next_deadline();
            tokio::select! {
                () = shutdown.cancelled() => {
                    info!(sessions = self.sessions.len(), "shutting down");
                    break;
                }
                () = self.shared.reauth_notify.notified() => self.take_reauth_requests(),
                change = next_change(&mut self.changes) => match change {
                    Some(()) => {
                        debug!(peers = self.peers.len(), "the policy changed, deciding every peer again");
                        self.reauth.request_all();
                    }
                    None => {
                        debug!("the policy's change stream ended");
                        self.changes = None;
                    }
                },
                () = sleep_until(deadline) => {
                    self.reauth.expire(tokio::time::Instant::now().into_std());
                }
                Some(decided) = self.decisions.join_next_with_id(), if !self.decisions.is_empty() => {
                    self.redecided(decided);
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
                            let failure = e!(Error::Transport {
                                source: e!(moq_transport_iroh::Error::Connect {
                                    source: Arc::new(AnyError::from_display(&err))
                                })
                            });
                            self.dialed(id, Err(failure));
                        }
                    }
                }
            }
            self.start_decisions();
        }
        self.drain().await;
        // The rest happens in `Drop`, which also runs if this task panics.
    }

    /// Tears the node's state down and reports the shutdown done.
    ///
    /// Runs from `Drop`, so [`Node::shutdown`] returns even if the actor
    /// panicked.
    fn finish(&mut self) {
        // A panic may have poisoned the lock. Do not panic again while
        // unwinding.
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
            state.publications.clear();
            state.links.clear();
            state.clear_peers();
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

    /// Waits up to [`SHUTDOWN_GRACE`] for every session to flush its close.
    ///
    /// Aborting the tasks at once would leave peers to notice by timing out.
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

    /// Dispatches one request from the node's handles.
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

    /// Answers a connect from the live session with the peer, or dials one.
    ///
    /// Callers that ask while a dial is in flight wait on that dial.
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

    /// Returns the oldest session with `peer` that is not closing.
    ///
    /// A session stays listed until its task ends, a little after its
    /// connection, so the front of the list can be on its way out.
    fn live_session(&self, peer: &EndpointId) -> Option<Session> {
        self.peers.get(peer).and_then(|sessions| {
            sessions
                .iter()
                .find(|session| !session.is_closing())
                .cloned()
        })
    }

    /// Registers the session a dial produced, or fails the callers waiting on it.
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
                    reply.send(Err(err.clone())).ok();
                }
            }
        }
    }

    /// Takes the re-decides the application asked for.
    fn take_reauth_requests(&mut self) {
        let requests = std::mem::take(&mut *self.shared.reauth_requests.lock().expect("poisoned"));
        if requests.all {
            debug!(
                peers = self.peers.len(),
                "deciding every peer again, as asked"
            );
            self.reauth.request_all();
        }
        for peer in requests.peers {
            if self.reauth.request(&peer) {
                debug!(remote = %peer.fmt_short(), "deciding the peer again, as asked");
            }
        }
    }

    /// Starts the re-decides that wait, as far as the concurrency limit allows.
    fn start_decisions(&mut self) {
        while let Some(start) = self.reauth.start() {
            let mut request =
                PeerRequest::new(start.peer, start.request, start.dialed, start.event);
            let current = self
                .shared
                .state
                .lock()
                .expect("poisoned")
                .peer_authorization(&start.peer)
                .cloned();
            if let Some(current) = current {
                request = request.with_current(current);
            }
            let shared = self.shared.clone();
            let (peer, generation) = (start.peer, start.generation);
            let handle = self.decisions.spawn(
                async move {
                    let decision = decide(&shared, &request).await;
                    (peer, generation, decision)
                }
                .instrument(info_span!("redecide", remote = %peer.fmt_short(), generation)),
            );
            self.decision_ids.insert(handle.id(), (peer, generation));
        }
    }

    /// Applies a finished re-decide, unless a newer one replaced it.
    ///
    /// An authorization replaces the peer's on every session with it. A
    /// rejection takes the peer's rights at once and closes those sessions
    /// with the rejection's code.
    fn redecided(&mut self, decided: Result<(tokio::task::Id, Redecided), tokio::task::JoinError>) {
        let (peer, generation, decision) = match decided {
            Ok((id, decided)) => {
                self.decision_ids.remove(&id);
                decided
            }
            Err(err) => {
                let Some((peer, generation)) = self.decision_ids.remove(&err.id()) else {
                    return;
                };
                error!(remote = %peer.fmt_short(), %err, "re-decide task failed, rejecting the peer");
                (peer, generation, Err(Rejection::new("the policy failed")))
            }
        };
        match self.reauth.finish(&peer, generation) {
            Finished::Gone => {
                debug!(remote = %peer.fmt_short(), "the peer left before its decision")
            }
            Finished::Stale => {
                debug!(remote = %peer.fmt_short(), generation, "dropping a decision a newer one replaced");
            }
            Finished::Apply => match decision {
                Ok(authorization) => {
                    debug!(
                        remote = %peer.fmt_short(),
                        grant = ?authorization.grant,
                        expires = ?authorization.expires,
                        "the policy decided again",
                    );
                    self.reauth.decided(
                        &peer,
                        &authorization,
                        tokio::time::Instant::now().into_std(),
                        SystemTime::now(),
                    );
                    self.shared
                        .state
                        .lock()
                        .expect("poisoned")
                        .admit_peer(peer, authorization);
                }
                Err(rejection) => {
                    info!(
                        remote = %peer.fmt_short(),
                        reason = rejection.reason(),
                        code = %rejection.code(),
                        "the policy rejected the peer, closing its sessions",
                    );
                    self.reauth.remove(&peer);
                    // Taken at once, so a session that is still closing keeps
                    // nothing, and a new one is decided afresh.
                    self.shared
                        .state
                        .lock()
                        .expect("poisoned")
                        .remove_peer(&peer);
                    for session in self.peers.get(&peer).into_iter().flatten() {
                        session.close(rejection.code().clone());
                    }
                }
            },
        }
    }

    /// Starts running an established session and makes it reachable.
    fn register(&mut self, parts: SessionParts) -> Session {
        let (session, driver) = self.add_link(parts);
        self.add_peer_session(&session);
        let inner = &session.inner;
        info!(
            remote = %inner.remote.fmt_short(),
            link = %inner.link,
            dialed = inner.dialed,
            "session started",
        );
        self.spawn_session(session.clone(), driver);
        session
    }

    /// Adds an established session's link to the node's state.
    ///
    /// Returns the session and the driver that runs it.
    fn add_link(&mut self, parts: SessionParts) -> (Session, moq_net::Driver) {
        let SessionParts {
            remote,
            connection,
            dialed,
            authorization,
            request,
            moq,
            driver,
            auth,
        } = parts;
        let link_state = LinkState::default();
        let hop = Hop::of(&remote);
        self.reauth.admitted(
            remote,
            &authorization,
            request.clone(),
            dialed,
            tokio::time::Instant::now().into_std(),
            SystemTime::now(),
        );
        let mut state = self.shared.state.lock().expect("poisoned");
        let link = LinkId(state.next_id());
        let session = Session {
            inner: Arc::new(SessionInner {
                link,
                remote,
                dialed,
                request,
                connection,
                moq,
                hop,
                link_state: link_state.clone(),
                shared: Arc::downgrade(&self.shared),
                closing: Default::default(),
            }),
        };
        // The peer's authorization holds for every session with it, so this
        // session's admission sets it for older ones too.
        state.admit_peer(remote, authorization);
        state.add_link(
            link,
            LinkEntry {
                kind: LinkKind::Direct,
                remote: Some(remote),
                hop,
                grant: None,
                public: true,
                consume: true,
                auth,
                session: Some(session.clone()),
                link_state,
            },
        );
        (session, driver)
    }

    /// Adds a session to its peer's, and answers the dials that wait for the peer.
    fn add_peer_session(&mut self, session: &Session) {
        let remote = session.inner.remote;
        // Two peers that dial each other at once end up with two sessions
        // each. Both stay: the two sides see the collision at different
        // instants, so either may already have handed out the other's second.
        let sessions = self.peers.entry(remote).or_default();
        if !sessions.is_empty() {
            debug!(remote = %remote.fmt_short(), "simultaneous connect, serving the first session");
        }
        sessions.push(session.clone());
        self.publish_sessions();
        if let Some(serving) = self.live_session(&remote) {
            for reply in self.pending.remove(&remote).into_iter().flatten() {
                reply.send(Ok(serving.clone())).ok();
            }
        }
    }

    /// Spawns the task that runs `session` until it ends or the node shuts down.
    fn spawn_session(&mut self, session: Session, driver: moq_net::Driver) {
        let SessionInner {
            link,
            remote,
            ref connection,
            ref moq,
            ref link_state,
            ..
        } = *session.inner;
        let monitor = link::monitor_direct(
            moq.clone(),
            connection.clone(),
            link_state.clone(),
            link::SharedSampler::default(),
        );
        let moq = moq.clone();
        let cancel = self.shared.shutdown.child_token();
        let handle = self.sessions.spawn(
            async move {
                let _monitor = AbortOnDropHandle::new(tokio::spawn(monitor));
                let run = moq_net::time::run(driver);
                tokio::pin!(run);
                let ended = tokio::select! {
                    () = cancel.cancelled() => None,
                    err = &mut run => Some(err),
                };
                // A shutdown still runs the driver, so it can flush the close.
                let err = match ended {
                    Some(err) => err,
                    None => {
                        moq.abort(moq_net::Error::Cancel);
                        run.await
                    }
                };
                drop(session);
                err
            }
            .instrument(info_span!("session", remote = %remote.fmt_short(), %link)),
        );
        self.session_ids.insert(handle.id(), (link, remote));
    }

    /// Cleans up after a session task ends, or panics.
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
                info!(remote = %remote.fmt_short(), %link, "session closed");
            }
            Ok(err) => info!(remote = %remote.fmt_short(), %link, %err, "session ended"),
            Err(err) => error!(remote = %remote.fmt_short(), %link, %err, "session task failed"),
        }
        let mut last = false;
        if let Some(sessions) = self.peers.get_mut(&remote) {
            sessions.retain(|session| session.inner.link != link);
            if sessions.is_empty() {
                self.peers.remove(&remote);
                last = true;
            }
        }
        let mut state = self.shared.state.lock().expect("poisoned");
        state.remove_link(link);
        if last {
            state.remove_peer(&remote);
            self.reauth.remove(&remote);
        }
        drop(state);
        self.publish_sessions();
    }

    /// Publishes the current sessions to the watchers of [`Node::sessions`].
    fn publish_sessions(&self) {
        let sessions: Vec<Session> = self.peers.values().flatten().cloned().collect();
        self.shared.sessions.set(sessions).ok();
    }
}

/// Waits for the next item of the policy's change stream, or forever without one.
async fn next_change(changes: &mut Option<BoxStream<()>>) -> Option<()> {
    match changes {
        Some(changes) => changes.next().await,
        None => std::future::pending().await,
    }
}

/// Waits until `deadline`, or forever without one.
async fn sleep_until(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}
