//! The node: one route table, its publications, and its links.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use moq_net::{AsPath, Consume, Path, PathOwned, broadcast, origin};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, info_span, warn};

use crate::{
    Admission, Audience, ConnectOptions, Error, Grant, Incoming, LinkKind, Publication, RouteInfo,
    Session, SessionRequest, Subscription,
    link::{self, LinkState},
    path::{LIVE, hop_for, legacy_name, live_path, publisher_of},
    publish::peers_task,
    route,
    session::{SessionInner, SessionParts, Transport, accept_transport, dial_session, driver_now},
    state::{self, LinkEntry, PubEntry, State},
};

/// How long [`Moq::shutdown`] gives a session to tell its peer it is closing.
///
/// A close is one packet and needs no answer, so this is a round trip's grace
/// and not a negotiation.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// How long a direct subscribe waits before it tries the older layout's name.
///
/// The publisher-named path goes first; the bare name a node on the older path
/// layout publishes is tried once this has passed.
///
/// A current peer announces both, straight after the session opens, so the
/// named path wins well inside this. An older peer announces only the bare
/// name, and waiting first keeps it from winning a race against a current one.
const LEGACY_GRACE: Duration = Duration::from_secs(2);

/// How many incoming sessions may wait for [`Moq::accept`] at once.
///
/// Past this the protocol handler holds further ones back.
const INCOMING_QUEUE: usize = 16;

/// How [`Moq::subscribe`] reaches a path the route table has no route to yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reach {
    /// Dials the publisher the path names.
    ///
    /// Relays serve only if they already have a route.
    Direct,
    /// Waits for an attached relay to route the path.
    Relays,
    /// Dials directly and waits on relays at once.
    ///
    /// The route table serves the cheaper route.
    #[default]
    Both,
}

/// How a [`Moq`] node runs.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct MoqConfig {
    /// How incoming sessions are admitted.
    pub admission: Admission,
    /// How callers that do not choose reach a path with no route yet.
    ///
    /// [`Live::subscribe`] and rooms use it.
    ///
    /// [`Live::subscribe`]: https://docs.rs/iroh-live
    pub reach: Reach,
    /// A route table to share with another server, instead of the node's own.
    ///
    /// A relay binary shares its cluster's.
    /// The node's hop is then the origin's, so every route it forwards carries
    /// one identity.
    pub origin: Option<origin::Producer>,
}

impl fmt::Debug for MoqConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqConfig")
            .field("admission", &self.admission)
            .field("reach", &self.reach)
            .field("origin", &self.origin.as_ref().map(|origin| origin.hop()))
            .finish()
    }
}

impl MoqConfig {
    /// Sets how incoming sessions are admitted.
    pub fn with_admission(mut self, admission: Admission) -> Self {
        self.admission = admission;
        self
    }

    /// Sets the default reach.
    pub fn with_reach(mut self, reach: Reach) -> Self {
        self.reach = reach;
        self
    }

    /// Shares `origin` as the route table.
    pub fn with_origin(mut self, origin: origin::Producer) -> Self {
        self.origin = Some(origin);
        self
    }
}

/// The node's moq endpoint.
///
/// Owns one route table fed by every link, the node's publications, and its
/// sessions. Cheap to clone; [`shutdown`](Self::shutdown) ends it for every
/// clone. Implements [`ProtocolHandler`]: mount it under every ALPN in
/// [`alpns`](crate::alpns).
///
/// # Examples
///
/// Publish a broadcast and serve it to whoever connects:
///
/// ```no_run
/// use iroh::{Endpoint, endpoint::presets, protocol::Router};
/// use iroh_moq::{Audience, Moq, MoqConfig};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = Endpoint::bind(presets::Minimal).await?;
/// let moq = Moq::new(endpoint.clone(), MoqConfig::default());
/// let mut router = Router::builder(endpoint);
/// for alpn in iroh_moq::alpns() {
///     router = router.accept(alpn, moq.clone());
/// }
/// let _router = router.spawn();
///
/// let broadcast = moq_net::broadcast::Info::new().produce();
/// // write tracks into `broadcast`, then:
/// let publication = moq.publish("camera", &broadcast, Audience::Everyone)?;
/// println!("{}", publication.ticket().expect("a live path"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Moq {
    pub(crate) shared: Arc<Shared>,
    tasks: Arc<Tasks>,
}

impl fmt::Debug for Moq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Moq")
            .field("id", &self.shared.id.fmt_short().to_string())
            .field("hop", &self.shared.hop)
            .finish_non_exhaustive()
    }
}

/// What every handle of a node shares.
pub(crate) struct Shared {
    pub(crate) endpoint: Endpoint,
    pub(crate) id: EndpointId,
    pub(crate) hop: moq_net::Hop,
    pub(crate) admission: Admission,
    pub(crate) reach: Reach,
    /// The route table.
    pub(crate) table: origin::Producer,
    pub(crate) state: Mutex<State>,
    pub(crate) sessions: Watchable<Vec<Session>>,
    actor: mpsc::Sender<ActorMessage>,
    incoming_tx: mpsc::Sender<Incoming>,
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<Incoming>>,
    pub(crate) shutdown: CancellationToken,
    done: Watchable<bool>,
}

/// The node's tasks, held apart from [`Shared`] so the tasks can hold it.
///
/// Without a cycle: dropping the last [`Moq`] drops these, which ends the
/// tasks, which release the shared state.
pub(crate) struct Tasks {
    _actor: AbortOnDropHandle<()>,
    _table: Option<AbortOnDropHandle<()>>,
    /// Relay links, by link id.
    relays: Mutex<HashMap<u64, AbortOnDropHandle<()>>>,
}

impl Tasks {
    /// Keeps a relay link's task running until the link detaches.
    #[cfg(feature = "relay-links")]
    pub(crate) fn insert_relay(&self, link: u64, task: AbortOnDropHandle<()>) {
        self.relays.lock().expect("poisoned").insert(link, task);
    }

    /// Stops a relay link's task.
    #[cfg(feature = "relay-links")]
    pub(crate) fn remove_relay(&self, link: u64) {
        self.relays.lock().expect("poisoned").remove(&link);
    }
}

impl Moq {
    /// Creates the node for `endpoint`.
    ///
    /// Must be called within a tokio runtime. Accepts nothing until mounted on
    /// a [`Router`](iroh::protocol::Router) under every ALPN in
    /// [`alpns`](crate::alpns).
    pub fn new(endpoint: Endpoint, config: MoqConfig) -> Self {
        let id = endpoint.id();
        let (table, table_task) = match config.origin {
            Some(origin) => (origin, None),
            None => {
                let (table, driver) = origin::Producer::new(origin::Config::new(hop_for(&id)));
                let task = tokio::spawn(
                    async move {
                        let err = moq_net::time::run(driver).await;
                        debug!(%err, "route table finished");
                    }
                    .instrument(info_span!("route_table")),
                );
                (table, Some(AbortOnDropHandle::new(task)))
            }
        };
        let hop = table.hop();
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_QUEUE);
        let shared = Arc::new(Shared {
            endpoint,
            id,
            hop,
            admission: config.admission,
            reach: config.reach,
            table,
            state: Mutex::new(State::default()),
            sessions: Watchable::new(Vec::new()),
            actor: actor_tx,
            incoming_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            shutdown: CancellationToken::new(),
            done: Watchable::new(false),
        });
        info!(id = %id.fmt_short(), %hop, admission = ?config.admission, "moq node started");
        let actor = Actor::new(shared.clone());
        let actor_task = tokio::spawn(
            actor
                .run(actor_rx)
                .instrument(info_span!("moq", id = %id.fmt_short())),
        );
        Self {
            shared,
            tasks: Arc::new(Tasks {
                _actor: AbortOnDropHandle::new(actor_task),
                _table: table_task,
                relays: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Returns the endpoint the node runs on.
    pub fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    /// Returns the reach [`MoqConfig::reach`] set.
    ///
    /// For callers that do not choose their own.
    pub fn reach(&self) -> Reach {
        self.shared.reach
    }

    /// Publishes `broadcast` as `live/<this node's id>/<name>` to `audience`.
    ///
    /// For one release the broadcast is also offered to direct sessions at the
    /// bare `name`, which is where nodes on the older path layout look for it.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Duplicate`] if something is already published at the
    /// path, [`Error::InvalidPath`] for an empty name or one with a `*`
    /// segment, and [`Error::ShutDown`] once the node has shut down.
    pub fn publish(
        &self,
        name: &str,
        broadcast: impl Consume<broadcast::Consumer>,
        audience: Audience,
    ) -> Result<Publication, Error> {
        let legacy = Path::new(name).to_owned();
        if legacy.is_empty() {
            return Err(e!(Error::InvalidPath {
                path: name.to_owned()
            }));
        }
        let path = live_path(self.shared.id, name);
        self.publish_inner(path, Some(legacy), broadcast.consume(), audience)
    }

    /// Publishes `broadcast` at an explicit path.
    ///
    /// For namespaces other than `live/`: rooms publish at
    /// `rooms/<topic>/<id>/<name>`.
    ///
    /// # Errors
    ///
    /// As [`publish`](Self::publish).
    pub fn publish_at(
        &self,
        path: impl AsPath,
        broadcast: impl Consume<broadcast::Consumer>,
        audience: Audience,
    ) -> Result<Publication, Error> {
        self.publish_inner(
            path.as_path().to_owned(),
            None,
            broadcast.consume(),
            audience,
        )
    }

    /// Publishes at `path`, also answering `legacy` on direct sessions.
    ///
    /// For the rooms crate's one release of the older room layout; everything
    /// else uses [`publish`](Self::publish) or [`publish_at`](Self::publish_at).
    #[doc(hidden)]
    pub fn publish_at_with_legacy(
        &self,
        path: impl AsPath,
        legacy: impl AsPath,
        broadcast: impl Consume<broadcast::Consumer>,
        audience: Audience,
    ) -> Result<Publication, Error> {
        self.publish_inner(
            path.as_path().to_owned(),
            Some(legacy.as_path().to_owned()),
            broadcast.consume(),
            audience,
        )
    }

    fn publish_inner(
        &self,
        path: PathOwned,
        legacy: Option<PathOwned>,
        broadcast: broadcast::Consumer,
        audience: Audience,
    ) -> Result<Publication, Error> {
        check_path(&path)?;
        let weak = Arc::downgrade(&self.shared);
        let mut state = self.shared.state.lock().expect("poisoned");
        if state.closed {
            return Err(e!(Error::ShutDown));
        }
        if state.publication_at(&path).is_some() {
            return Err(e!(Error::Duplicate { path }));
        }
        let id = state.next_id();
        let closed_task = {
            let broadcast = broadcast.clone();
            let weak = weak.clone();
            let path = path.clone();
            tokio::spawn(async move {
                broadcast.closed().await;
                let Some(shared) = weak.upgrade() else { return };
                let removed = shared
                    .state
                    .lock()
                    .expect("poisoned")
                    .remove_publication(id);
                if removed.is_some() {
                    info!(%path, "broadcast ended, unpublished");
                }
            })
        };
        let kind = state::audience_kind(&audience);
        let local = matches!(audience, Audience::Everyone)
            .then(|| state::serve(&self.shared.table, &path, &broadcast))
            .flatten();
        info!(%path, ?audience, "published");
        state.add_publication(
            id,
            PubEntry {
                path: path.clone(),
                legacy,
                broadcast,
                audience: kind,
                manual: HashMap::new(),
                local,
                peers_task: peers_task(&audience, id, &weak),
                _closed_task: Some(AbortOnDropHandle::new(closed_task)),
            },
        );
        Ok(Publication::new(id, path, weak))
    }

    /// Resolves `path` in the route table.
    ///
    /// If no route exists yet, it reaches out as `reach` says. A direct reach
    /// dials the publisher the path names (`live/<id>/...` or
    /// `rooms/<topic>/<id>/...`) and waits for it to announce the path. For a
    /// `live/` path it also tries the bare name a node on the older path layout
    /// publishes, if the publisher announces nothing under `live/<id>/`.
    /// Cancellation safe: a dial it started continues for other callers.
    ///
    /// The route table holds routes a direct peer announces to its own
    /// broadcasts, routes an attached relay forwards, and this node's own
    /// publications for [`Audience::Everyone`]. A path naming a publisher
    /// therefore resolves to that publisher's broadcast, directly or through a
    /// relay, and never to one a third peer announced under its name. A relay
    /// is trusted with every path it forwards, so attach only relays whose
    /// admission keeps each publisher to its own paths (moq-relay's tokens do).
    /// This node's `Peers` and `Manual` publications are not in the table, which
    /// may be shared with a cluster; read those from the broadcast itself.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NoRoute`] if `reach` allows no way to reach the
    /// path, [`Error::Connect`] if the publisher cannot be dialed and no relay
    /// can stand in, [`Error::NotAnnounced`] if the publisher's session ends
    /// before it announces the path, and [`Error::ShutDown`] once the node has
    /// shut down.
    pub async fn subscribe(&self, path: impl AsPath, reach: Reach) -> Result<Subscription, Error> {
        let path = path.as_path().to_owned();
        check_path(&path)?;
        if self.shared.shutdown.is_cancelled() {
            return Err(e!(Error::ShutDown));
        }
        let table = self.shared.table.consume();
        // A route the table already knows answers at once.
        if let Ok(broadcast) = table.request_broadcast(&path).await {
            debug!(%path, "resolved through an existing route");
            return Ok(self.subscription(path, table, broadcast));
        }

        let publisher = publisher_of(&path).filter(|peer| *peer != self.shared.id);
        let relays = matches!(reach, Reach::Relays | Reach::Both) && self.has_relays();
        let dial = match reach {
            Reach::Direct | Reach::Both => publisher,
            Reach::Relays => None,
        };
        let Some(peer) = dial else {
            if !relays {
                return Err(e!(Error::NoRoute { path }));
            }
            debug!(%path, "waiting for a relay to route the path");
            let resolved = table.routed_broadcast(&path).await;
            return self.resolved(path, table, resolved);
        };

        debug!(%path, peer = %peer.fmt_short(), relays, "dialing the publisher");
        let routed = {
            let (table, path) = (table.clone(), path.clone());
            async move { table.routed_broadcast(&path).await }
        };
        tokio::pin!(routed);
        let session = tokio::select! {
            resolved = &mut routed => return self.resolved(path, table, resolved),
            connected = self.connect(peer) => match connected {
                Ok(session) => session,
                Err(err) if relays => {
                    info!(%path, %err, "publisher unreachable, waiting for a relay");
                    let resolved = routed.await;
                    return self.resolved(path, table, resolved);
                }
                Err(err) => return Err(err),
            },
        };

        // The session ending is the end of the wait unless a relay can still
        // bring the path, and for a `live/` path the older layout's bare name
        // is worth a try once the named path has had its chance.
        let legacy = legacy_name(&path);
        let fallback = async {
            let Some(name) = legacy else {
                return Err(session.closed().await);
            };
            tokio::time::sleep(LEGACY_GRACE).await;
            let bare = session.subscribe(name).await?;
            // A peer on the current layout announces its named paths before
            // their bare aliases, so if it announces any, the bare name is
            // the alias of a named path the table is about to route, and the
            // table's route is the one that can fail over to a relay.
            let named = Path::new(&format!("{LIVE}/{peer}")).to_owned();
            if session.announces_under(&named) {
                debug!(%path, "the publisher uses the current layout, ignoring its bare name");
                return Err(session.closed().await);
            }
            Ok(bare)
        };
        tokio::select! {
            resolved = &mut routed => self.resolved(path, table, resolved),
            fallback = fallback => match fallback {
                Ok(subscription) => {
                    info!(
                        %path,
                        legacy = %subscription.path(),
                        "the publisher uses the older path layout",
                    );
                    Ok(subscription)
                }
                Err(_) if relays => {
                    let resolved = routed.await;
                    self.resolved(path, table, resolved)
                }
                Err(_) => Err(e!(Error::NotAnnounced { path })),
            },
        }
    }

    fn resolved(
        &self,
        path: PathOwned,
        table: origin::Consumer,
        resolved: Result<broadcast::Consumer, moq_net::Error>,
    ) -> Result<Subscription, Error> {
        match resolved {
            Ok(broadcast) => Ok(self.subscription(path, table, broadcast)),
            Err(moq_net::Error::Closed) => Err(e!(Error::ShutDown)),
            Err(source) => Err(e!(Error::Unresolved { path, source })),
        }
    }

    fn subscription(
        &self,
        path: PathOwned,
        table: origin::Consumer,
        broadcast: broadcast::Consumer,
    ) -> Subscription {
        Subscription::new(path, table, broadcast, None, Arc::downgrade(&self.shared))
    }

    /// Returns every route to `path`, and which one serves, as they change.
    pub fn routes(&self, path: impl AsPath) -> n0_watcher::Direct<Vec<RouteInfo>> {
        self.shared
            .state
            .lock()
            .expect("poisoned")
            .watch_routes(path.as_path().to_owned())
    }

    /// Returns the session with `peer`, dialing if there is none.
    ///
    /// Concurrent calls share one dial. Cancellation safe: an abandoned dial
    /// still completes for the callers that stayed.
    ///
    /// From moq-lite-05 on, the dialer's half of the handshake completes before
    /// the peer has decided whether to admit it, so a peer that refuses the
    /// session closes it right after this returns; [`Session::closed`] reports
    /// why.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Connect`] if the dial fails, [`Error::Refused`] if
    /// the peer does not admit the session, and [`Error::ShutDown`] once the
    /// node has shut down.
    pub async fn connect(&self, peer: impl Into<EndpointAddr>) -> Result<Session, Error> {
        self.connect_with(peer, ConnectOptions::default()).await
    }

    /// Dials `peer` with a token, a price, or a grant.
    ///
    /// For a peer that admits manually.
    /// An existing session with the peer is returned as it is, whatever the
    /// options.
    ///
    /// # Errors
    ///
    /// As [`connect`](Self::connect).
    pub async fn connect_with(
        &self,
        peer: impl Into<EndpointAddr>,
        options: ConnectOptions,
    ) -> Result<Session, Error> {
        if self.shared.shutdown.is_cancelled() {
            return Err(e!(Error::ShutDown));
        }
        let (reply, reply_rx) = oneshot::channel();
        self.shared
            .actor
            .send(ActorMessage::Connect {
                remote: peer.into(),
                options,
                reply,
            })
            .await
            .map_err(|_| e!(Error::ShutDown))?;
        reply_rx.await.map_err(|_| e!(Error::ShutDown))?
    }

    /// Returns the open sessions, dialed and accepted alike, as they change.
    pub fn sessions(&self) -> n0_watcher::Direct<Vec<Session>> {
        self.shared.sessions.watch()
    }

    /// Waits for the next session waiting to be admitted.
    ///
    /// Yields only under [`Admission::Manual`]. Returns `None` once the node
    /// shuts down. Cancellation safe.
    pub async fn accept(&self) -> Option<Incoming> {
        let mut incoming = self.shared.incoming_rx.lock().await;
        tokio::select! {
            incoming = incoming.recv() => incoming,
            _ = self.shared.shutdown.cancelled() => None,
        }
    }

    /// Shuts the node down for every clone.
    ///
    /// Every session closes, relays detach, publications are withdrawn, and
    /// [`connect`](Self::connect) and [`publish`](Self::publish) fail from here
    /// on.
    /// Waits for sessions to tell their peers, within a short grace, so it is
    /// safe to close the endpoint after this returns. Idempotent; not
    /// cancellation safe, call it again to finish.
    pub async fn shutdown(&self) {
        self.shared.shutdown.cancel();
        self.tasks.relays.lock().expect("poisoned").clear();
        let mut done = self.shared.done.watch();
        while !done.get() {
            if done.updated().await.is_err() {
                break;
            }
        }
    }

    fn has_relays(&self) -> bool {
        self.shared
            .state
            .lock()
            .expect("poisoned")
            .links
            .values()
            .any(|link| link.kind == LinkKind::Relay)
    }

    /// Returns the node's tasks, for a handle that holds them weakly.
    #[cfg(feature = "relay-links")]
    pub(crate) fn tasks(&self) -> &Arc<Tasks> {
        &self.tasks
    }

    /// Admits or queues one incoming connection.
    async fn handle_connection(&self, connection: Connection) -> Result<(), Error> {
        if self.shared.shutdown.is_cancelled() {
            return Err(e!(Error::ShutDown));
        }
        let remote = connection.remote_id();
        let (transport, h3) = accept_transport(connection.clone()).await?;
        let handshake = moq_net::Server::new()
            .accept_request(driver_now(), Transport::new(transport))
            .await
            .map_err(|source| e!(Error::Moq { source }))?;
        let request = match h3 {
            Some((target, headers)) => SessionRequest::new(&target, headers, handshake.role()),
            None => SessionRequest::new(handshake.path(), Vec::new(), handshake.role()),
        };
        debug!(remote = %remote.fmt_short(), path = request.path(), "session requested");
        let incoming = Incoming {
            remote,
            request,
            connection,
            handshake,
            shared: Arc::downgrade(&self.shared),
        };
        match self.shared.admission {
            Admission::Open => {
                incoming.admit(Grant::everything()).await?;
            }
            Admission::Manual => {
                self.shared
                    .incoming_tx
                    .send(incoming)
                    .await
                    .map_err(|_| e!(Error::ShutDown))?;
            }
        }
        Ok(())
    }
}

impl ProtocolHandler for Moq {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.handle_connection(connection)
            .await
            .map_err(AnyError::from)?;
        Ok(())
    }
}

impl Shared {
    /// Hands an established session to the actor, which runs it.
    pub(crate) async fn register(&self, parts: SessionParts) -> Result<Session, Error> {
        let (reply, reply_rx) = oneshot::channel();
        self.actor
            .send(ActorMessage::Register {
                parts: Box::new(parts),
                reply,
            })
            .await
            .map_err(|_| e!(Error::ShutDown))?;
        reply_rx.await.map_err(|_| e!(Error::ShutDown))?
    }
}

/// Refuses a path that is empty or holds a segment only a pattern can spell.
fn check_path(path: &Path<'_>) -> Result<(), Error> {
    if path.is_empty() || path.parts().any(|part| part == "*" || part == "**") {
        return Err(e!(Error::InvalidPath {
            path: path.as_str().to_owned()
        }));
    }
    Ok(())
}

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

enum ActorMessage {
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
struct Actor {
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

impl Actor {
    fn new(shared: Arc<Shared>) -> Self {
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

    async fn run(mut self, mut inbox: mpsc::Receiver<ActorMessage>) {
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
        let mut state = self.shared.state.lock().expect("poisoned");
        state.closed = true;
        state.publications.clear();
        state.links.clear();
        drop(state);
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

    /// Returns the oldest session with `peer` whose connection is still open.
    ///
    /// A session stays listed until its task lands, a scheduling hop after the
    /// connection went, so the front of the list can be one on its way out.
    fn live_session(&self, peer: &EndpointId) -> Option<Session> {
        self.peers.get(peer).and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session.inner.connection.close_reason().is_none())
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
            }),
        };
        state.add_link(
            link,
            LinkEntry {
                kind: LinkKind::Direct,
                remote: Some(remote),
                grant,
                publish: origins.publish.clone(),
                legacy: true,
                public: true,
                offers: HashMap::new(),
                announced: Default::default(),
                session: Some(session.clone()),
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
                let delivery = moq.recv_bandwidth();
                let crate::session::Origins {
                    publish_driver,
                    ingest,
                    ingest_driver,
                    ..
                } = origins;
                let _publish = AbortOnDropHandle::new(tokio::spawn(async move {
                    moq_net::time::run(publish_driver).await;
                }));
                let _ingest = AbortOnDropHandle::new(tokio::spawn(async move {
                    moq_net::time::run(ingest_driver).await;
                }));
                let _bridge = AbortOnDropHandle::new(tokio::spawn(
                    route::bridge(shared, link, ingest, Some(remote)).in_current_span(),
                ));
                let _monitor = AbortOnDropHandle::new(tokio::spawn(link::monitor(
                    connection,
                    delivery,
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
