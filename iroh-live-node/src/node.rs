//! The node: one route table, its publications, and its links.

use std::{
    collections::HashMap,
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
use moq_net::{Path, broadcast, origin};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{
    Admission, Audience, BroadcastTicket, ConnectOptions, Error, Grant, GrantFn, LinkId, OwnerFn,
    Publication, RelayConfig, RelayLink, Session, Subscription,
    actor::{Actor, ActorMessage},
    admission,
    handshake::{Hop, SessionParts},
    policy::{self, Authorization, DynPolicy, PeerRequest, Policy},
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

/// The configuration of a [`Node`].
#[derive(Clone, derive_more::Debug)]
pub struct NodeConfig {
    /// Decides who may hold a session with the node, and with which rights.
    ///
    /// The node asks it when it accepts a session, after the peer's MoQ
    /// SETUP, and before it dials a peer without [`ConnectOptions::grant`].
    /// A rejection closes an accepted session with the rejection's code, and
    /// fails a dial with [`Error::Rejected`]. Every session with one peer
    /// shares the authorization the first of them got. Set it with
    /// [`with_policy`](Self::with_policy).
    ///
    /// The node asks again about a live peer:
    ///
    /// - with [`Event::Revalidate`](policy::Event::Revalidate) when its
    ///   authorization is due for revalidation;
    /// - with [`Event::Reauthorize`](policy::Event::Reauthorize) when its
    ///   authorization expires, when the policy's [`changes`](Policy::changes)
    ///   stream yields, and when [`Node::reauthorize`] asks.
    ///
    /// A new authorization replaces the old one on every live session with
    /// the peer, and a rejection closes them.
    ///
    /// `None` builds a policy from [`grant`](Self::grant): every peer is
    /// admitted with the grant it returns, or with
    /// [`Grant::subscribe_only`] without one.
    pub policy: Option<Arc<dyn DynPolicy<EndpointId>>>,
    /// How long the node waits for each decision of its policy.
    ///
    /// A decision that takes longer is a rejection with
    /// [`moq_net::Error::Timeout`]. The default is [`DECIDE_TIMEOUT`]. Raise it
    /// when a person answers, as with a [`policy::Queue`].
    pub decide_timeout: Duration,
    /// How incoming sessions are admitted.
    pub admission: Admission,
    /// Returns the grant of a session with a peer, from its endpoint id.
    ///
    /// Used when [`policy`](Self::policy) is `None`, for accepted sessions
    /// and for dials without [`ConnectOptions::grant`].
    ///
    /// `None` is the secure default: [`Grant::subscribe_only`], so any peer may
    /// subscribe to anything but no peer may publish. Return
    /// [`Grant::everything`] to allow any peer to publish at any path.
    ///
    /// A grant's publish patterns decide which paths the peer may put into the
    /// route table. Let each peer publish only under paths that name it, or
    /// one peer can stand in for another.
    #[debug("{}", grant.is_some())]
    pub grant: Option<GrantFn>,
    /// A route table to use instead of a new one, such as a relay's cluster origin.
    ///
    /// The caller runs its driver. The node creates its publications in it,
    /// writes what peers announce into it, and serves its local view, which
    /// hides routes from handles tagged [`Link::Peer`](origin::Link::Peer) or
    /// [`Link::Upstream`](origin::Link::Upstream). Grants keep each peer to
    /// this node's own publications, whatever else the table holds. `None`
    /// creates a table under the hop derived from the endpoint id.
    #[debug("{}", table.is_some())]
    pub table: Option<origin::Producer>,
    /// Names the peer that owns a path, for [`Node::claim_path`].
    ///
    /// `None` uses the generic rule: a path is a peer's when exactly one of
    /// its segments is the peer's endpoint id. Set it for paths that name
    /// their publisher some other way, such as a relay's pull paths.
    #[debug("{}", owner.is_some())]
    pub owner: Option<OwnerFn>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            policy: None,
            decide_timeout: DECIDE_TIMEOUT,
            admission: Admission::default(),
            grant: None,
            table: None,
            owner: None,
        }
    }
}

impl NodeConfig {
    /// Returns this configuration with `policy` deciding who may hold a session.
    ///
    /// See [`NodeConfig::policy`].
    #[must_use]
    pub fn with_policy(mut self, policy: impl Policy<EndpointId>) -> Self {
        self.policy = Some(Arc::new(policy));
        self
    }
}

/// A MoQ node on an iroh endpoint.
///
/// Holds the route table, the node's publications and its sessions. Cheap to
/// clone, and [`shutdown`](Self::shutdown) ends it for every clone. Mount it
/// on a [`Router`](iroh::protocol::Router) with [`mount`](Self::mount).
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
/// let publication = moq.publish("demo/camera", Audience::Everyone)?;
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
    /// Decides sessions, built from the configured grant if none was given.
    pub(crate) policy: Arc<dyn DynPolicy<EndpointId>>,
    /// How long a decision of [`policy`](Self::policy) may take.
    pub(crate) decide_timeout: Duration,
    /// The re-decides the application asked for, until the actor takes them.
    pub(crate) reauth_requests: Mutex<reauth::Requests>,
    /// Wakes the actor for new [`reauth_requests`](Self::reauth_requests).
    pub(crate) reauth_notify: Notify,
    pub(crate) owner: Option<OwnerFn>,
    /// The route table.
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
///
/// Dropping the last [`Node`] drops these, which ends the tasks and releases
/// the shared state.
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
    /// Must be called within a tokio runtime.
    pub fn new(endpoint: Endpoint, config: NodeConfig) -> Self {
        let id = endpoint.id();
        let (table, table_task) = match config.table {
            Some(table) => (table, None),
            None => {
                let (table, driver) = origin::Producer::new(origin::Config::new(Hop::of(&id).id()));
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
        let policy = config
            .policy
            .unwrap_or_else(|| legacy_policy(config.grant.clone()));
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let shared = Arc::new(Shared {
            endpoint,
            id,
            policy,
            decide_timeout: config.decide_timeout,
            reauth_requests: Mutex::new(reauth::Requests::default()),
            reauth_notify: Notify::new(),
            owner: config.owner,
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
    /// Without a router, or an accept loop of its own that hands connections
    /// to the node, the endpoint refuses every incoming connection. The node
    /// then serves only the sessions it dials and its attached relays.
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
    pub fn id(&self) -> EndpointId {
        self.shared.id
    }

    /// Returns the authorization the policy gave `peer`, while it holds a session.
    ///
    /// Every session with the peer shares it. `None` while the peer has no
    /// live session: the policy decides when one opens. Ticket claims widen
    /// the grant on top of it, which [`Session::grant`] includes.
    pub fn grant_for(&self, peer: EndpointId) -> Option<Authorization> {
        self.shared
            .state
            .lock()
            .expect("poisoned")
            .peer_authorization(&peer)
            .cloned()
    }

    /// Asks the policy again about `peer`, for every live session with it.
    ///
    /// Call it when state the policy reads changes and its
    /// [`changes`](Policy::changes) stream does not cover it, such as a
    /// [`filter`](Policy::filter) predicate. The decision runs in the
    /// background: a new authorization replaces the old one on every session
    /// with the peer, and a rejection closes them with its code. Does nothing
    /// for a peer without a live session.
    pub fn reauthorize(&self, peer: EndpointId) {
        if self.grant_for(peer).is_none() {
            return;
        }
        self.shared
            .reauth_requests
            .lock()
            .expect("poisoned")
            .peers
            .insert(peer);
        self.shared.reauth_notify.notify_one();
    }

    /// Asks the policy again about every peer with a live session.
    ///
    /// As [`reauthorize`](Self::reauthorize), for every peer.
    pub fn reauthorize_all(&self) {
        self.shared.reauth_requests.lock().expect("poisoned").all = true;
        self.shared.reauth_notify.notify_one();
    }

    /// Returns a future that resolves when the node shuts down.
    ///
    /// Resolves once [`shutdown`](Self::shutdown) starts, or once the last
    /// handle drops. Owns what it needs, so a service can hold it in a task.
    /// Cancellation safe.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        self.shared.shutdown.clone().cancelled_owned()
    }

    /// Returns the endpoint the node runs on.
    pub fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    /// Creates a broadcast at `path` in the route table, for `audience`.
    ///
    /// Fill it through [`Publication::broadcast`]. Peers read that broadcast
    /// itself, never a copy: every session serves the route table, and its
    /// grant lets the peer see exactly the publications whose audience admits
    /// it. Changing the audience re-authorizes every live session at once.
    ///
    /// A grant lists each visible publication's path, so moq's limits on a
    /// grant bound how many publications one peer can see, as
    /// [`moq_ext_access::visible_paths`] describes.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Duplicate`] if another publication has the path,
    /// [`Error::Moq`] if `path` does not parse, [`Error::InvalidPath`] for an
    /// empty path or one with a `*` segment, and [`Error::ShutDown`] once the
    /// node has shut down.
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
    /// `fill` runs once, with the broadcast before anyone can see it, and its
    /// result comes back with the publication. Create the tracks a reader
    /// needs first there, such as a catalog, so no reader finds the broadcast
    /// without them. It runs before the node takes its lock, so it may call
    /// back into the node. If the path turns out to be taken, the broadcast
    /// is closed and what `fill` returned is dropped.
    ///
    /// # Errors
    ///
    /// As [`publish`](Self::publish), and [`Error::Fill`] if `fill` fails,
    /// which closes the broadcast. `fill` may fail with any error that boxes
    /// into a `std::error::Error`, an `anyhow::Error` included.
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
    /// The table holds what direct peers announce within their grants, routes
    /// from attached relays, and every publication of this node, whatever
    /// its audience. With grants that keep each peer to paths that name it,
    /// a path resolves to its publisher's broadcast and never to a third
    /// peer's. A relay is trusted with every path it forwards, so attach only
    /// relays that keep each publisher to its own paths, as moq-relay's tokens
    /// do. The [crate docs](crate#trust-and-grants) explain the trust model.
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
    /// with [`Reach::Both`]. A viewer therefore needs no grant of its own
    /// to watch a ticket. When the last subscription of a path drops, the
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
    /// It holds what [`subscribe`](Self::subscribe) resolves in: what direct
    /// peers announce within their grants, routes from attached relays, and
    /// every publication of this node, whatever its audience. Its
    /// [`announced`](origin::Consumer::announced) stream lists those paths as
    /// they come and go.
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

    /// Dials `peer` with a token, a cost, or a grant.
    ///
    /// An existing session with the peer, or a dial in flight to it, is
    /// returned as it is, and the options go unused. To be sure of a session's
    /// grant, call [`Session::set_grant`] on the session this returns.
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

    /// Returns the open direct sessions, dialed and accepted, as they change.
    pub fn sessions(&self) -> n0_watcher::Direct<Vec<Session>> {
        self.shared.sessions.watch()
    }

    /// Attaches to the moq relay at `config.url`, redialing it with backoff.
    ///
    /// `iroh://` URLs are dialed through this node's endpoint.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Relay`] if the client cannot be set up, for example
    /// for an unsupported URL scheme, and [`Error::ShutDown`] once the node
    /// has shut down.
    pub fn attach_relay(&self, config: RelayConfig) -> Result<RelayLink, Error> {
        relay::attach(self, config)
    }

    /// Shuts the node down for every clone.
    ///
    /// Closes every session, detaches relays and withdraws publications.
    /// Afterwards [`connect`](Self::connect), [`publish`](Self::publish) and
    /// [`attach_relay`](Self::attach_relay) fail, and incoming sessions are
    /// refused, also those that wait for the policy. Gives sessions a short grace
    /// to tell their peers, so the endpoint can be closed once this returns.
    /// Idempotent. Not cancellation safe: call it again to finish.
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

/// Returns the policy that stands in for a configuration without one.
///
/// It admits every peer with the grant `grant` gives it, and with
/// [`Grant::subscribe_only`] without a grant function.
fn legacy_policy(grant: Option<GrantFn>) -> Arc<dyn DynPolicy<EndpointId>> {
    match grant {
        Some(grant) => Arc::new(policy::from_fn(move |request: &PeerRequest| {
            Ok(grant(*request.peer()).into())
        })),
        None => Arc::new(policy::Open(Grant::subscribe_only())),
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
