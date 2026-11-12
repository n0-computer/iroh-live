//! The node: one route table, its publications, and its links.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, RouterBuilder},
};
use moq_net::{Path, broadcast, origin, track};
use n0_error::{AnyError, e};
use n0_future::{Stream, StreamExt, stream, task::AbortOnDropHandle};
use n0_watcher::{Watchable, Watcher};
use serde::Serialize;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, trace, warn};

use crate::{
    Audience, BroadcastTicket, ConnectOptions, Direction, Error, Grant, Layout, LinkId,
    Publication, RelayConfig, RelayLink, Session, Subscription,
    actor::{Actor, ActorMessage},
    admission,
    handshake::{Hop, SessionParts},
    json,
    policy::{self, Authorization, DynPolicy, Policy},
    publish, reauth, relay, route,
    state::State,
};

/// How [`Node::subscribe`] reaches a path the route table has no route to yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Dials the publisher and waits for it to announce the path.
    ///
    /// A relay route that is in the table, or arrives during the dial, also
    /// resolves the path.
    Direct(EndpointId),
    /// Waits for an attached relay to route the path.
    Relays,
    /// Dials the publisher and waits on relays at the same time.
    Both(EndpointId),
}

/// How long the node waits for its policy by default.
pub const DECIDE_TIMEOUT: Duration = Duration::from_secs(10);

/// How many messages may wait for the node's actor.
///
/// Every message is a connect or a session to register. Sixteen absorb a
/// burst of dials, and a full inbox only makes the next sender wait.
const ACTOR_INBOX: usize = 16;

/// The configuration of a [`Node`].
#[derive(Clone, derive_more::Debug)]
pub struct NodeConfig {
    /// Decides who may hold a session with the node, and with which rights.
    ///
    /// The node asks it when it accepts a session, after the peer's MoQ
    /// SETUP, and before it dials a peer. A rejection closes an accepted
    /// session with the rejection's code, and fails a dial with
    /// [`Error::Rejected`]. Each session is decided on its own, with the
    /// request and token it opened with, so a peer's second session asks
    /// the policy again.
    ///
    /// The node asks again about a live session when its authorization
    /// expires and on [`Node::reauthorize`], with
    /// [`Event::Reauthorize`](policy::Event::Reauthorize). An expired
    /// session may do nothing until the new decision applies.
    ///
    /// `None` is [`policy::Open`] with [`Grant::subscribe_only`]: every peer
    /// may subscribe to anything and publish nothing.
    pub policy: Option<Arc<dyn DynPolicy<EndpointId>>>,
    /// How long the node waits for each decision of its policy.
    ///
    /// A decision that takes longer is a rejection with
    /// [`moq_net::Error::Timeout`]. The default is [`DECIDE_TIMEOUT`]. Raise it
    /// when a person answers, as with a [`policy::Queue`].
    ///
    /// When the node asks again about a live session, a timeout or a full
    /// queue ([`moq_net::Error::TooManyRequests`]) is no rejection: the
    /// session keeps its authorization, and the node asks again later.
    pub decide_timeout: Duration,
    /// A route table to use instead of a new one, such as a relay's cluster origin.
    ///
    /// The caller runs its driver. The node serves the table's local view,
    /// which hides routes from handles tagged [`Link::Peer`](origin::Link::Peer)
    /// or [`Link::Upstream`](origin::Link::Upstream). `None` creates a table
    /// under the hop derived from the endpoint id.
    #[debug("{}", table.is_some())]
    pub table: Option<origin::Producer>,
    /// The path layout that names the peer owning a path, for [`Node::claim_path`].
    ///
    /// `None` uses the generic rule: a path is a peer's when exactly one of
    /// its segments is the peer's endpoint id.
    #[debug("{}", layout.is_some())]
    pub layout: Option<Arc<dyn Layout<EndpointId>>>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            policy: None,
            decide_timeout: DECIDE_TIMEOUT,
            table: None,
            layout: None,
        }
    }
}

impl NodeConfig {
    /// Returns this configuration with `policy` deciding who may hold a session.
    #[must_use]
    pub fn with_policy(mut self, policy: impl Policy<EndpointId>) -> Self {
        self.policy = Some(Arc::new(policy));
        self
    }
}

/// A MoQ node on an iroh endpoint.
///
/// Cheap to clone, and [`shutdown`](Self::shutdown) ends it for every clone.
/// Mount it on a [`Router`](iroh::protocol::Router) with [`mount`](Self::mount).
///
/// # Examples
///
/// Publish a broadcast and serve it to whoever connects:
///
/// ```no_run
/// use iroh::{Endpoint, endpoint::presets, protocol::Router};
/// use iroh_live_node::{Audience, Node, NodeConfig};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = Endpoint::bind(presets::Minimal).await?;
/// let moq = Node::new(endpoint.clone(), NodeConfig::default());
/// let _router = moq.mount(Router::builder(endpoint)).spawn();
///
/// let publication = moq.publish("demo/camera", Audience::Granted)?;
/// // write tracks into the broadcast in the route table:
/// let _track = publication.broadcast().create_track("video", None)?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, derive_more::Debug)]
#[debug("Node({}, hop {})", shared.id.fmt_short(), shared.table.hop())]
pub struct Node {
    pub(crate) shared: Arc<Shared>,
    /// Aborts the node's tasks when the last handle drops.
    pub(crate) tasks: Arc<Tasks>,
}

/// What every handle of a node shares.
pub(crate) struct Shared {
    pub(crate) endpoint: Endpoint,
    pub(crate) id: EndpointId,
    pub(crate) policy: Arc<dyn DynPolicy<EndpointId>>,
    pub(crate) decide_timeout: Duration,
    /// The re-decides the application asked for, until the actor takes them.
    pub(crate) reauth_requests: Mutex<reauth::Requests>,
    /// Wakes the actor for new [`reauth_requests`](Self::reauth_requests).
    pub(crate) reauth_notify: Notify,
    pub(crate) layout: Option<Arc<dyn Layout<EndpointId>>>,
    pub(crate) table: origin::Producer,
    /// The node's publications and links.
    ///
    /// Lock order: this lock first, then a subscription's `routes`. Never
    /// await while holding either.
    pub(crate) state: Mutex<State>,
    pub(crate) sessions: Watchable<Vec<Session>>,
    pub(crate) actor: mpsc::Sender<ActorMessage>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) done: Watchable<bool>,
    /// Set by [`Node::mount`], or by the first session the node accepts.
    mounted: AtomicBool,
    /// Set once a publish has warned that no peer can connect.
    warned_unreachable: AtomicBool,
}

/// The node's tasks, kept apart from [`Shared`] so the tasks can hold it.
pub(crate) struct Tasks {
    /// Cancelled on drop, so [`Node::closed`] resolves when the last handle goes.
    shutdown: CancellationToken,
    _actor: AbortOnDropHandle<()>,
    /// Runs the route table's driver, unless the caller supplied the table.
    _table: Option<AbortOnDropHandle<()>>,
    /// Relay links, by link id.
    pub(crate) relays: Mutex<HashMap<LinkId, crate::relay::RelayTask>>,
}

impl Drop for Tasks {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Node {
    /// Creates a node on `endpoint`.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn new(endpoint: Endpoint, config: NodeConfig) -> Self {
        let id = endpoint.id();
        let (table, table_task) = match config.table {
            Some(table) => (table, None),
            None => {
                let (table, driver) = origin::Producer::new(origin::Config::new(Hop::of(&id).id()));
                let task = tokio::spawn(
                    async move {
                        let err = moq_net::time::run(driver).await;
                        debug!(error = %err, "route table finished");
                    }
                    .instrument(info_span!("route_table")),
                );
                (table, Some(AbortOnDropHandle::new(task)))
            }
        };
        let policy = config
            .policy
            .unwrap_or_else(|| Arc::new(policy::Open(Grant::subscribe_only())));
        let (actor_tx, actor_rx) = mpsc::channel(ACTOR_INBOX);
        let shared = Arc::new(Shared {
            endpoint,
            id,
            policy,
            decide_timeout: config.decide_timeout,
            reauth_requests: Mutex::new(reauth::Requests::default()),
            reauth_notify: Notify::new(),
            layout: config.layout,
            table,
            state: Mutex::new(State::default()),
            sessions: Watchable::new(Vec::new()),
            actor: actor_tx,
            shutdown: CancellationToken::new(),
            done: Watchable::new(false),
            mounted: AtomicBool::new(false),
            warned_unreachable: AtomicBool::new(false),
        });
        info!(id = %id.fmt_short(), hop = %shared.table.hop(), "moq node started");
        let shutdown = shared.shutdown.clone();
        let actor = Actor::new(shared.clone());
        let actor_task = tokio::spawn(
            actor
                .run(actor_rx)
                .instrument(info_span!("moq", id = %id.fmt_short())),
        );
        Self {
            shared,
            tasks: Arc::new(Tasks {
                shutdown,
                _actor: AbortOnDropHandle::new(actor_task),
                _table: table_task,
                relays: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Mounts the node on `router` under every ALPN in [`alpns`](crate::alpns).
    ///
    /// Without a router, or an accept loop that hands connections to the
    /// node, the endpoint refuses every incoming connection.
    #[must_use]
    pub fn mount(&self, mut router: RouterBuilder) -> RouterBuilder {
        debug_assert_eq!(
            router.endpoint().id(),
            self.shared.id,
            "the router runs on another endpoint"
        );
        self.shared.mounted.store(true, Ordering::Relaxed);
        for alpn in crate::alpns() {
            router = router.accept(alpn, self.clone());
        }
        router
    }

    /// Returns the endpoint id of the node.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.shared.id
    }

    /// Returns the authorization the policy gave the newest session with `peer` that holds one.
    ///
    /// The node decides each session on its own, so two sessions with one
    /// peer may hold different authorizations; [`Session::grant`] reads one
    /// session's. Ticket claims widen the grant on top of it, which
    /// [`Session::grant`] includes. An expired authorization stays here until
    /// the policy decides again, while [`Session::grant`] already grants
    /// nothing. `None` once the peer has no session, or none that holds an
    /// authorization.
    #[must_use]
    pub fn grant_for(&self, peer: EndpointId) -> Option<Authorization> {
        self.shared
            .state
            .lock()
            .expect("poisoned")
            .newest_authorization(&peer)
            .cloned()
    }

    /// Asks the policy again about every live session with `peer`.
    ///
    /// Call it when state the policy reads changes, such as a
    /// [`filter`](Policy::filter) predicate. The decisions run in the
    /// background: a new authorization replaces a session's, and a
    /// rejection closes that session with its code.
    ///
    /// A session of any peer whose first decision is still running when
    /// this is called is decided again once it registers, and holds nothing
    /// until then, so it never keeps rights from before the call.
    pub fn reauthorize(&self, peer: EndpointId) {
        self.shared.state.lock().expect("poisoned").reauthorized += 1;
        self.shared
            .reauth_requests
            .lock()
            .expect("poisoned")
            .peers
            .insert(peer);
        self.shared.reauth_notify.notify_one();
    }

    /// Asks the policy again about every live session, as [`reauthorize`](Self::reauthorize) does for one peer.
    pub fn reauthorize_all(&self) {
        self.shared.state.lock().expect("poisoned").reauthorized += 1;
        self.shared.reauth_requests.lock().expect("poisoned").all = true;
        self.shared.reauth_notify.notify_one();
    }

    /// Returns a future that resolves when the node shuts down.
    ///
    /// Resolves once [`shutdown`](Self::shutdown) starts, or once the last
    /// handle drops. Cancellation safe.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        self.shared.shutdown.clone().cancelled_owned()
    }

    /// Returns the endpoint the node runs on.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    /// Creates a broadcast at `path` in the route table, for `audience`.
    ///
    /// Fill it through [`Publication::broadcast`]. moq's limits on a grant
    /// bound how many publications one peer can see, as
    /// [`moq_ext_access::visible_paths`] describes.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Duplicate`] if another publication has the path,
    /// [`Error::Moq`] if `path` does not parse, [`Error::InvalidPath`] for an
    /// empty path or one with a `*` segment, and [`Error::ShutDown`] once the
    /// node has shut down.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn publish(
        &self,
        path: impl TryInto<Path, Error: Into<moq_net::Error>>,
        audience: Audience,
    ) -> Result<Publication, Error> {
        let path = crate::error::to_path(path)?;
        self.warn_if_unreachable(&path);
        let (publication, ()) =
            publish::publish(&self.shared, path, audience, |_| Ok::<_, Infallible>(()))?;
        Ok(publication)
    }

    /// Publishes as [`publish`](Self::publish) does, filling the broadcast before it is announced.
    ///
    /// `fill` runs once, before anyone can see the broadcast, and its result
    /// comes back with the publication. Create the tracks a reader needs
    /// first there, such as a catalog. It runs before the node takes its
    /// lock, so it may call back into the node. If the path turns out to be
    /// taken, the broadcast is closed and what `fill` returned is dropped.
    ///
    /// # Errors
    ///
    /// As [`publish`](Self::publish), and [`Error::Fill`] if `fill` fails,
    /// which closes the broadcast. `fill` may fail with any error that boxes
    /// into a `std::error::Error`, an `anyhow::Error` included.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn publish_with<T, E>(
        &self,
        path: impl TryInto<Path, Error: Into<moq_net::Error>>,
        audience: Audience,
        fill: impl FnOnce(&broadcast::Producer) -> Result<T, E>,
    ) -> Result<(Publication, T), Error>
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let path = crate::error::to_path(path)?;
        self.warn_if_unreachable(&path);
        publish::publish(&self.shared, path, audience, fill)
    }

    /// Publishes a broadcast at `path` whose one track `name` holds a latest-value JSON track.
    ///
    /// For state such as a setting, a status or a result: each
    /// [`update`](json::snapshot::Producer::update) replaces the value, and
    /// a reader that subscribes late gets the current one at once. The track
    /// exists before the broadcast is announced, so no reader misses it.
    /// Readers open it with [`Subscription::value_track`]. To put the track
    /// next to others, create it in [`publish_with`](Self::publish_with)'s
    /// fill and pass it to [`json::snapshot::Producer::new`].
    ///
    /// # Errors
    ///
    /// As [`publish_with`](Self::publish_with).
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn run(node: iroh_live_node::Node) -> Result<(), Box<dyn std::error::Error>> {
    /// use iroh_live_node::Audience;
    ///
    /// let (_publication, mut latency) =
    ///     node.publish_value::<u64>("live/control", Audience::Granted, "latency")?;
    /// latency.update(&150)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn publish_value<T: Serialize>(
        &self,
        path: impl TryInto<Path, Error: Into<moq_net::Error>>,
        audience: Audience,
        name: &str,
    ) -> Result<(Publication, json::snapshot::Producer<T>), Error> {
        self.publish_with(path, audience, |broadcast| {
            let track = broadcast.create_track(name, track::Info::default())?;
            Ok::<_, moq_net::Error>(json::snapshot::Producer::new(
                track,
                json::snapshot::Config::default(),
            ))
        })
    }

    /// Warns once if no peer can connect to the node to read `path`.
    ///
    /// Forgetting to mount the node fails silently otherwise: dialers get an
    /// ALPN error, and the publisher logs nothing.
    fn warn_if_unreachable(&self, path: &Path) {
        if self.shared.mounted.load(Ordering::Relaxed)
            || !self.tasks.relays.lock().expect("poisoned").is_empty()
            || self.shared.warned_unreachable.swap(true, Ordering::Relaxed)
        {
            return;
        }
        warn!(
            %path,
            "publishing on a node that is neither mounted on a router nor attached to a relay: \
             only peers this node dials can read it"
        );
    }

    /// Resolves `path` in the route table.
    ///
    /// If no route exists yet, reaches out as `reach` says. Cancellation safe:
    /// a dial this call started continues for other callers.
    ///
    /// The [crate docs](crate#trust-and-grants) explain the trust model.
    ///
    /// # Errors
    ///
    /// Fails with:
    ///
    /// - [`Error::Moq`] or [`Error::InvalidPath`] for a path that cannot be
    ///   published, as [`publish`](Self::publish) does.
    /// - [`Error::NoRoute`] if `reach` allows no way to reach the path.
    /// - [`Error::Transport`] if the publisher cannot be dialed and no relay can
    ///   stand in.
    /// - [`Error::NotGranted`] if the publisher's grant does not let it
    ///   publish the path and no relay can stand in.
    /// - [`Error::NotAnnounced`] if the publisher's session ends before it
    ///   announces the path.
    /// - [`Error::Unresolved`] if the route table refuses the path.
    /// - [`Error::ShutDown`] once the node has shut down.
    pub async fn subscribe(
        &self,
        path: impl TryInto<Path, Error: Into<moq_net::Error>>,
        reach: Reach,
    ) -> Result<Subscription, Error> {
        route::subscribe(self, crate::error::to_path(path)?, reach).await
    }

    /// Resolves the broadcast `ticket` names, whatever grants this node has.
    ///
    /// The node first claims the path for the ticket's peer with
    /// [`claim_path`](Self::claim_path), and the subscription holds the claim
    /// for as long as it lives. Then this is [`subscribe`](Self::subscribe)
    /// with [`Reach::Both`]. When the last subscription of a path drops, the
    /// grant narrows again, and moq ends what local readers read there.
    ///
    /// # Errors
    ///
    /// As [`claim_path`](Self::claim_path), and as [`subscribe`](Self::subscribe).
    pub async fn subscribe_ticket(&self, ticket: &BroadcastTicket) -> Result<Subscription, Error> {
        route::subscribe_ticket(self, ticket).await
    }

    /// Returns the route table.
    ///
    /// It holds what [`subscribe`](Self::subscribe) resolves in, and its
    /// [`announced`](origin::Consumer::announced) stream lists the paths.
    #[must_use]
    pub fn origin(&self) -> origin::Consumer {
        self.shared.table.consume()
    }

    /// Returns the session with `peer`, dialing if there is none.
    ///
    /// Concurrent calls share one dial, which continues if this future is
    /// dropped. The dialer's half of the handshake completes before the peer
    /// decides whether to admit it, so a peer that refuses usually closes the
    /// session right after this returns. [`Session::closed`] then says why.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Rejected`] if this node's policy rejects the peer,
    /// [`Error::Transport`] if the dial fails, [`Error::Refused`] if a peer on
    /// an older protocol version refuses during the handshake, and
    /// [`Error::ShutDown`] once the node has shut down. A handshake that takes
    /// longer than [`HANDSHAKE_TIMEOUT`](crate::HANDSHAKE_TIMEOUT) fails with
    /// [`Error::Moq`].
    pub async fn connect(&self, peer: impl Into<EndpointAddr>) -> Result<Session, Error> {
        self.connect_with(peer, ConnectOptions::default()).await
    }

    /// Dials `peer` with a token, a path, a cost, or iroh's connect options.
    ///
    /// An existing session with the peer, or a dial in flight to it, is
    /// returned as it is, and the options go unused. A token then reaches
    /// the peer only if you close that session first.
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
                peer: peer.into(),
                options,
                reply,
            })
            .await
            .map_err(|_| e!(Error::ShutDown))?;
        reply_rx.await.map_err(|_| e!(Error::ShutDown))?
    }

    /// Returns the open direct sessions, dialed and accepted, as they change.
    #[must_use]
    pub fn sessions(&self) -> n0_watcher::Direct<Vec<Session>> {
        self.shared.sessions.watch()
    }

    /// Returns a stream of the sessions this node accepts.
    ///
    /// Yields every accepted session open when it is called, then each new
    /// one as it opens, each once, and ends when the node shuts down. It
    /// leaves out the sessions this node dialed, which
    /// [`sessions`](Self::sessions) lists too. It reads the same list, so a
    /// session that opens and closes between two polls may be skipped.
    ///
    /// # Examples
    ///
    /// Serve each peer that connects in a task of its own:
    ///
    /// ```no_run
    /// use n0_future::StreamExt;
    ///
    /// # async fn run(node: iroh_live_node::Node) {
    /// let mut accepted = node.accepted();
    /// while let Some(session) = accepted.next().await {
    ///     tokio::spawn(async move {
    ///         // Serve the peer, until the session ends.
    ///         let reason = session.closed().await;
    ///         println!("{} left: {reason:#}", session.peer());
    ///     });
    /// }
    /// # }
    /// ```
    pub fn accepted(&self) -> impl Stream<Item = Session> + Send + Unpin + 'static {
        struct State {
            lists: n0_watcher::Stream<n0_watcher::Direct<Vec<Session>>>,
            shutdown: CancellationToken,
            /// The links of the accepted sessions already yielded and still listed.
            seen: HashSet<LinkId>,
            /// Accepted sessions not yielded yet.
            pending: VecDeque<Session>,
        }
        let state = State {
            lists: self.shared.sessions.watch().stream(),
            shutdown: self.shared.shutdown.clone(),
            seen: HashSet::new(),
            pending: VecDeque::new(),
        };
        Box::pin(stream::unfold(state, |mut state| async move {
            loop {
                if let Some(session) = state.pending.pop_front() {
                    return Some((session, state));
                }
                let sessions = tokio::select! {
                    sessions = state.lists.next() => sessions?,
                    () = state.shutdown.cancelled() => return None,
                };
                let listed: HashSet<LinkId> =
                    sessions.iter().map(|session| session.inner.link).collect();
                state.seen.retain(|link| listed.contains(link));
                for session in sessions {
                    if session.direction() == Direction::Accepted
                        && state.seen.insert(session.inner.link)
                    {
                        trace!(
                            peer = %session.peer().fmt_short(),
                            link = %session.inner.link,
                            "yielding accepted session"
                        );
                        state.pending.push_back(session);
                    }
                }
            }
        }))
    }

    /// Attaches to the moq relay at `config.url`, redialing it with backoff.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidRelayUrl`] for an unsupported URL scheme or
    /// an `iroh://` URL without an endpoint id, [`Error::Relay`] if the client
    /// cannot be set up, and [`Error::ShutDown`] once the node has shut down.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn attach_relay(&self, config: RelayConfig) -> Result<RelayLink, Error> {
        relay::attach(self, config)
    }

    /// Shuts the node down for every clone.
    ///
    /// Closes every session, detaches relays and withdraws publications.
    /// Gives sessions a short grace to tell their peers, so the endpoint can
    /// be closed once this returns. Idempotent. Not cancellation safe: call it
    /// again to finish.
    pub async fn shutdown(&self) {
        // Set first, so a racing publish or attach fails instead of being
        // added and then cleared.
        self.shared.state.lock().expect("poisoned").closed = true;
        self.shared.shutdown.cancel();
        self.tasks.shutdown_relays().await;
        let mut done = self.shared.done.watch();
        while !done.get() {
            if done.updated().await.is_err() {
                break;
            }
        }
    }
}

impl ProtocolHandler for Node {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // Covers an accept loop of the caller's own, which skips `mount`.
        self.shared.mounted.store(true, Ordering::Relaxed);
        admission::accept(&self.shared, connection)
            .await
            .map_err(AnyError::from)?;
        Ok(())
    }

    /// Shuts the node down with the router, as [`Node::shutdown`] does.
    async fn shutdown(&self) {
        // Resolves to the inherent method.
        Self::shutdown(self).await;
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
