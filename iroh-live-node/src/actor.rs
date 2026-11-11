//! The actor that runs a node's sessions and dials.

use std::{collections::HashMap, sync::Arc, time::Duration};

use iroh::{EndpointAddr, EndpointId};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tracing::{Instrument, debug, error, info, info_span, warn};

use crate::{
    ConnectOptions, Error, LinkId, LinkKind, Session,
    handshake::{Hop, SessionParts, dial_session},
    link::{self, LinkState},
    node::Shared,
    session::SessionInner,
    state::LinkEntry,
};

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
        }
    }

    /// Runs the actor until the node shuts down or every handle to it drops.
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
    fn add_link(&self, parts: SessionParts) -> (Session, moq_net::Driver) {
        let SessionParts {
            remote,
            connection,
            dialed,
            grant,
            request,
            moq,
            driver,
            auth,
        } = parts;
        let link_state = LinkState::default();
        let hop = Hop::of(&remote);
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
        // The peer's grant holds for every session with it, so this session's
        // admission sets it for older ones too.
        state.admit_peer(remote, grant);
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
