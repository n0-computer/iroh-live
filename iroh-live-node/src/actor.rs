//! The actor that runs a node's sessions and dials.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use iroh::{EndpointAddr, EndpointId};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tracing::{Instrument, debug, error, info, info_span, warn};

use crate::{
    ConnectOptions, Error, LinkId, LinkKind, Session, SessionRequest,
    admission::decide,
    handshake::{Hop, SessionParts, dial_session},
    link::{self, LinkState},
    node::Shared,
    policy::{Authorization, Event, PeerRequest, Rejection},
    reauth::Reauth,
    session::SessionInner,
    state::{LinkEntry, Registered},
};

/// What one re-decide asked: the peer and the epoch it started at.
type Asked = (EndpointId, u64);

/// The result of one re-decide: what it asked, and the decision.
type Redecided = (Asked, Result<Authorization, Rejection>);

/// How long [`Node::shutdown`] gives a session to tell its peer it is closing.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// What the node asks of its actor.
pub(crate) enum ActorMessage {
    Connect {
        peer: EndpointAddr,
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
    /// What each re-decide task asked, so a task that panics still finishes.
    decision_ids: HashMap<tokio::task::Id, Asked>,
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Actor {
    /// Creates an actor for the node `shared`.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
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
                () = sleep_until(deadline) => self.deadline_passed(),
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
                            error!(error = %err, "dial task failed");
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
                peer,
                options,
                reply,
            } => self.connect(peer, options, reply),
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
        peer: EndpointAddr,
        options: ConnectOptions,
        reply: oneshot::Sender<Result<Session, Error>>,
    ) {
        let id = peer.id;
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
        info!(peer = %id.fmt_short(), "dialing");
        let handle = self.dials.spawn(
            dial_session(self.shared.clone(), peer, options)
                .instrument(info_span!("dial", peer = %id.fmt_short())),
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
        let Some(peer) = self.dial_ids.remove(&id) else {
            return;
        };
        match result {
            Ok(parts) => {
                info!(peer = %peer.fmt_short(), "connected");
                self.register(parts);
            }
            Err(err) => {
                info!(peer = %peer.fmt_short(), error = %err, "dial failed");
                for reply in self.pending.remove(&peer).into_iter().flatten() {
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
                debug!(peer = %peer.fmt_short(), "deciding the peer again, as asked");
            }
        }
    }

    /// Takes the rights of the peers whose authorization expired, and asks the policy again.
    fn deadline_passed(&mut self) {
        let expired = self.reauth.expire(tokio::time::Instant::now().into_std());
        if expired.is_empty() {
            return;
        }
        let mut state = self.shared.state.lock().expect("poisoned");
        for peer in expired {
            info!(peer = %peer.fmt_short(), "the peer's authorization expired, narrowing it until the policy decides again");
            state.lapse(&peer);
        }
    }

    /// Starts the re-decides that wait, as far as the concurrency limit allows.
    fn start_decisions(&mut self) {
        while let Some(peer) = self.reauth.start() {
            let (request, dialed) = self.replayed_request(&peer);
            let started = self.shared.state.lock().expect("poisoned").next_epoch();
            let mut request = PeerRequest::new(peer, request, Event::Reauthorize);
            if dialed {
                request = request.with_dialed();
            }
            let shared = self.shared.clone();
            let asked = (peer, started);
            let handle = self.decisions.spawn(
                async move {
                    let decision = decide(&shared, &request).await;
                    (asked, decision)
                }
                .instrument(info_span!("redecide", peer = %peer.fmt_short(), started)),
            );
            self.decision_ids.insert(handle.id(), asked);
        }
    }

    /// Returns the request a re-decide of `peer` replays, and whether this node dialed it.
    ///
    /// That is the newest session's that carries a token, else the newest
    /// session's. A peer's sessions share one authorization, so the token
    /// that grants the most recent rights is the one to verify again. A dial
    /// carries no token, so a dialed session never hides an accepted
    /// session's token.
    fn replayed_request(&self, peer: &EndpointId) -> (SessionRequest, bool) {
        let sessions = self.peers.get(peer).map(Vec::as_slice).unwrap_or_default();
        sessions
            .iter()
            .rev()
            .find(|session| session.inner.request.token().is_some())
            .or_else(|| sessions.last())
            .map(|session| (session.inner.request.clone(), session.inner.dialed))
            .unwrap_or_default()
    }

    /// Applies a finished re-decide, unless a newer decision already applied.
    ///
    /// An authorization replaces the peer's on every session with it. A
    /// rejection takes the peer's rights at once and closes those sessions
    /// with the rejection's code. A decision that timed out or found a full
    /// queue keeps the peer's authorization and runs again later.
    fn redecided(&mut self, decided: Result<(tokio::task::Id, Redecided), tokio::task::JoinError>) {
        let ((peer, started), decision) = match decided {
            Ok((id, decided)) => {
                self.decision_ids.remove(&id);
                decided
            }
            Err(err) => {
                let Some(asked) = self.decision_ids.remove(&err.id()) else {
                    return;
                };
                error!(peer = %asked.0.fmt_short(), error = %err, "re-decide task failed, rejecting the peer");
                (asked, Err(Rejection::new("the policy failed")))
            }
        };
        if !self.reauth.finish(&peer) {
            debug!(peer = %peer.fmt_short(), "the peer left before its decision");
            return;
        }
        if let Err(rejection) = &decision
            && undecided(rejection)
        {
            let wait = self
                .reauth
                .retry(&peer, tokio::time::Instant::now().into_std());
            warn!(
                peer = %peer.fmt_short(),
                reason = rejection.reason(),
                ?wait,
                "the policy did not decide, keeping the peer's authorization and asking again later",
            );
            return;
        }
        let applied = self.shared.state.lock().expect("poisoned").decided(
            &peer,
            started,
            decision.as_ref().ok().cloned(),
        );
        if !applied {
            debug!(peer = %peer.fmt_short(), started, "dropping a decision a newer one replaced");
            return;
        }
        match decision {
            Ok(authorization) => {
                debug!(
                    peer = %peer.fmt_short(),
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
            }
            Err(rejection) => {
                info!(
                    peer = %peer.fmt_short(),
                    reason = rejection.reason(),
                    code = %rejection.code(),
                    "the policy rejected the peer, closing its sessions",
                );
                // The state took the peer's rights already, so a session that
                // is still closing keeps nothing, and a pending one that was
                // decided earlier does not undo the rejection.
                self.reauth.remove(&peer);
                for session in self.peers.get(&peer).into_iter().flatten() {
                    session.close(rejection.code().clone());
                }
            }
        }
    }

    /// Starts running an established session and makes it reachable.
    fn register(&mut self, parts: SessionParts) -> Session {
        let (session, driver) = self.add_link(parts);
        self.add_peer_session(&session);
        let inner = &session.inner;
        info!(
            peer = %inner.peer.fmt_short(),
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
            peer,
            connection,
            dialed,
            admitted,
            request,
            moq,
            driver,
            auth,
        } = parts;
        let link_state = LinkState::default();
        let hop = Hop::of(&peer);
        // A token no other session with the peer brought may grant more than
        // the authorization this session reuses, so the policy sees it.
        let new_token = !admitted.fresh
            && request.token().is_some_and(|token| {
                !self
                    .peers
                    .get(&peer)
                    .into_iter()
                    .flatten()
                    .any(|session| session.inner.request.token() == Some(token))
            });
        let authorization = admitted.authorization.clone();
        let mut state = self.shared.state.lock().expect("poisoned");
        let link = LinkId(state.next_id());
        let session = Session {
            inner: Arc::new(SessionInner {
                link,
                peer,
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
        // The peer's authorization holds for every session with it. This
        // session's applies only if no newer decision or trigger came since.
        let registered = admitted
            .pending
            .register(&mut state, authorization, admitted.decided);
        state.add_link(
            link,
            LinkEntry {
                kind: LinkKind::Direct,
                peer: Some(peer),
                hop,
                grant: None,
                consume: true,
                auth,
                session: Some(session.clone()),
                link_state,
            },
        );
        drop(state);
        self.reauth.register(peer);
        match registered {
            Registered::Applied => self.reauth.decided(
                &peer,
                &admitted.authorization,
                tokio::time::Instant::now().into_std(),
                SystemTime::now(),
            ),
            Registered::Kept => {}
            Registered::Stale => {
                debug!(peer = %peer.fmt_short(), "the session's authorization went stale before it registered, deciding again");
                self.reauth.request(&peer);
            }
        }
        if new_token && registered != Registered::Stale {
            debug!(peer = %peer.fmt_short(), "the session brought a new token, deciding again");
            self.reauth.request(&peer);
        }
        (session, driver)
    }

    /// Adds a session to its peer's, and answers the dials that wait for the peer.
    fn add_peer_session(&mut self, session: &Session) {
        let peer = session.inner.peer;
        // Two peers that dial each other at once end up with two sessions
        // each. Both stay: the two sides see the collision at different
        // instants, so either may already have handed out the other's second.
        let sessions = self.peers.entry(peer).or_default();
        if !sessions.is_empty() {
            debug!(peer = %peer.fmt_short(), "simultaneous connect, serving the first session");
        }
        sessions.push(session.clone());
        self.publish_sessions();
        if let Some(serving) = self.live_session(&peer) {
            for reply in self.pending.remove(&peer).into_iter().flatten() {
                reply.send(Ok(serving.clone())).ok();
            }
        }
    }

    /// Spawns the task that runs `session` until it ends or the node shuts down.
    fn spawn_session(&mut self, session: Session, driver: moq_net::Driver) {
        let SessionInner {
            link,
            peer,
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
            .instrument(info_span!("session", peer = %peer.fmt_short(), %link)),
        );
        self.session_ids.insert(handle.id(), (link, peer));
    }

    /// Cleans up after a session task ends, or panics.
    fn session_ended(
        &mut self,
        id: tokio::task::Id,
        result: Result<moq_net::Error, tokio::task::JoinError>,
    ) {
        let Some((link, peer)) = self.session_ids.remove(&id) else {
            return;
        };
        match result {
            Ok(moq_net::Error::Closed | moq_net::Error::Cancel) => {
                info!(peer = %peer.fmt_short(), %link, "session closed");
            }
            Ok(err) => info!(peer = %peer.fmt_short(), %link, error = %err, "session ended"),
            Err(err) => {
                error!(peer = %peer.fmt_short(), %link, error = %err, "session task failed")
            }
        }
        let mut last = false;
        if let Some(sessions) = self.peers.get_mut(&peer) {
            sessions.retain(|session| session.inner.link != link);
            if sessions.is_empty() {
                self.peers.remove(&peer);
                last = true;
            }
        }
        let mut state = self.shared.state.lock().expect("poisoned");
        state.remove_link(link);
        if last {
            state.peer_left(&peer);
            self.reauth.remove(&peer);
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

/// Reports whether a re-decide's `rejection` only means the policy could not decide now.
///
/// A timeout and a full queue say nothing about the peer, so the peer keeps
/// its authorization and is asked again. An expiry still narrows it on time.
fn undecided(rejection: &Rejection) -> bool {
    matches!(
        rejection.code(),
        moq_net::Error::Timeout | moq_net::Error::TooManyRequests
    )
}

/// Waits until `deadline`, or forever without one.
async fn sleep_until(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}
