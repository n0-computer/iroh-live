//! The node: one route table, its publications, and its links.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use moq_net::{AsPath, Consume, broadcast, origin};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span};

use crate::{
    Admission, Audience, ConnectOptions, Error, Grant, GrantFn, Incoming, Publication, RelayConfig,
    RelayLink, RouteInfo, Session, Subscription,
    admission::{self, INCOMING_QUEUE},
    publish, relay, route,
    session::{Actor, ActorMessage, SessionParts, hop_for},
    state::State,
};

/// How [`Moq::subscribe`] reaches a path the route table has no route to yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Dials the publisher and waits for it to announce the path.
    ///
    /// Relays serve only if they already have a route.
    Direct(EndpointId),
    /// Waits for an attached relay to route the path.
    Relays,
    /// Dials the publisher and waits on relays at once.
    ///
    /// The route table serves the cheaper route.
    Both(EndpointId),
}

/// How a [`Moq`] node runs.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct MoqConfig {
    /// How incoming sessions are admitted.
    pub admission: Admission,
    /// Returns the grant of a session with a peer, from its endpoint id.
    ///
    /// Used under [`Admission::Open`], and for a dial without
    /// [`ConnectOptions::grant`]. `None` grants everything.
    ///
    /// The grant's publish patterns decide which paths a peer may put into
    /// the route table, which every subscriber on the node shares. A function
    /// that lets each peer publish only under paths naming it keeps one peer
    /// from standing in for another: `iroh-live` lets a peer publish under
    /// `live/<its id>/` only.
    pub grant: Option<GrantFn>,
    /// A route table to share with another server, instead of the node's own.
    ///
    /// A relay binary shares its cluster's. The node's hop is then the
    /// origin's, so every route it forwards carries one identity.
    ///
    /// Everything the table holds is then the other server's to serve, to
    /// whoever it serves: the node's own `Everyone` publications, every route
    /// an attached relay forwards, and every broadcast a direct peer announces
    /// to this node under a path that names it, including ones the peer
    /// offered to this node alone. The table cannot tell those apart, since a
    /// peer's audience is the peer's business. Share a table only with a
    /// server that serves no more widely than the peers of this node expect.
    pub origin: Option<origin::Producer>,
}

impl fmt::Debug for MoqConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqConfig")
            .field("admission", &self.admission)
            .field("grant", &self.grant.is_some())
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
/// let _publication = moq.publish("demo/camera", &broadcast, Audience::Everyone)?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Moq {
    pub(crate) shared: Arc<Shared>,
    /// Held for its drop, which aborts the actor once the last handle goes.
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
    grant: Option<GrantFn>,
    /// The route table.
    pub(crate) table: origin::Producer,
    pub(crate) state: Mutex<State>,
    pub(crate) sessions: Watchable<Vec<Session>>,
    pub(crate) actor: mpsc::Sender<ActorMessage>,
    pub(crate) incoming_tx: mpsc::Sender<Incoming>,
    pub(crate) incoming_rx: tokio::sync::Mutex<mpsc::Receiver<Incoming>>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) done: Watchable<bool>,
}

/// The node's tasks, held apart from [`Shared`] so the tasks can hold it.
///
/// Without a cycle: dropping the last [`Moq`] drops these, which ends the
/// tasks, which release the shared state.
pub(crate) struct Tasks {
    _actor: AbortOnDropHandle<()>,
    _table: Option<AbortOnDropHandle<()>>,
    /// Relay links, by link id.
    pub(crate) relays: Mutex<HashMap<u64, crate::relay::RelayTask>>,
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
            grant: config.grant,
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

    /// Publishes `broadcast` at `path` to `audience`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Duplicate`] if a live publication already has the
    /// path, [`Error::InvalidPath`] for an empty path or one with a `*`
    /// segment, and [`Error::ShutDown`] once the node has shut down.
    pub fn publish(
        &self,
        path: impl AsPath,
        broadcast: impl Consume<broadcast::Consumer>,
        audience: Audience,
    ) -> Result<Publication, Error> {
        publish::publish(
            &self.shared,
            path.as_path().to_owned(),
            broadcast.consume(),
            audience,
        )
    }

    /// Resolves `path` in the route table.
    ///
    /// If no route exists yet, it reaches out as `reach` says. Cancellation
    /// safe: a dial it started continues for other callers.
    ///
    /// The route table holds what direct peers announce within their grants,
    /// routes an attached relay forwards, and this node's own publications for
    /// [`Audience::Everyone`]. With grants that keep each peer to paths naming
    /// it (see [`MoqConfig::grant`]), a path resolves to its publisher's
    /// broadcast, directly or through a relay, and never to one a third peer
    /// announced there. A relay is trusted with every path it forwards, so
    /// attach only relays whose admission keeps each publisher to its own
    /// paths (moq-relay's tokens do). This node's `Peers` and `Manual`
    /// publications are not in the table, which may be shared with a cluster;
    /// read those from the broadcast itself.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NoRoute`] if `reach` allows no way to reach the
    /// path, [`Error::Connect`] if the publisher cannot be dialed and no relay
    /// can stand in, [`Error::NotAnnounced`] if the publisher's session ends
    /// before it announces the path, and [`Error::ShutDown`] once the node has
    /// shut down.
    pub async fn subscribe(&self, path: impl AsPath, reach: Reach) -> Result<Subscription, Error> {
        route::subscribe(self, path.as_path().to_owned(), reach).await
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
    /// session usually closes it right after this returns, and
    /// [`Session::closed`] reports why.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Connect`] if the dial fails, [`Error::Refused`] if
    /// a peer on an older protocol version refuses during the handshake, and
    /// [`Error::ShutDown`] once the node has shut down.
    pub async fn connect(&self, peer: impl Into<EndpointAddr>) -> Result<Session, Error> {
        self.connect_with(peer, ConnectOptions::default()).await
    }

    /// Dials `peer` with a token, a price, or a grant.
    ///
    /// For a peer that admits manually. An existing session with the peer is
    /// returned as it is, whatever the options, and so is a dial already in
    /// flight to it: a call made while another dials joins that dial, and its
    /// own token, cost and grant go unused. Cancellation safe, as
    /// [`connect`](Self::connect).
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
    ///
    /// Direct sessions only; a relay link reports through its own status.
    pub fn sessions(&self) -> n0_watcher::Direct<Vec<Session>> {
        self.shared.sessions.watch()
    }

    /// Waits for the next session waiting to be admitted.
    ///
    /// Yields only under [`Admission::Manual`]. Returns `None` once the node
    /// shuts down. A session that waited longer than a few seconds for this is
    /// rejected rather than returned, since its peer has likely given up.
    /// Cancellation safe.
    pub async fn accept(&self) -> Option<Incoming> {
        admission::next(&self.shared).await
    }

    /// Returns how many incoming sessions wait for [`accept`](Self::accept).
    ///
    /// For an application that watches its accept loop keep up. Always zero
    /// under [`Admission::Open`].
    pub fn waiting_for_admission(&self) -> usize {
        let tx = &self.shared.incoming_tx;
        tx.max_capacity() - tx.capacity()
    }

    /// Stays attached to the moq relay at `config.url`, redialing with backoff.
    ///
    /// `iroh://` URLs go through this node's endpoint. Routes the relay
    /// announces enter the route table at the link's cost, and what
    /// `config.offer` names is published into the relay.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Relay`] if the client cannot be set up (an
    /// unsupported URL scheme, say), and [`Error::ShutDown`] once the node has
    /// shut down.
    pub fn attach_relay(&self, config: RelayConfig) -> Result<RelayLink, Error> {
        relay::attach(self, config)
    }

    /// Shuts the node down for every clone.
    ///
    /// Every session closes, relays detach, publications are withdrawn,
    /// sessions still waiting for admission are rejected, and
    /// [`connect`](Self::connect), [`publish`](Self::publish) and
    /// [`attach_relay`](Self::attach_relay) fail from here on. Waits for
    /// sessions to tell their peers, within a short grace, so it is safe to
    /// close the endpoint after this returns. Idempotent; not cancellation
    /// safe, call it again to finish.
    pub async fn shutdown(&self) {
        // Closed before anything else, so a publication or a relay that races
        // the shutdown fails rather than being added and then cleared.
        self.shared.state.lock().expect("poisoned").closed = true;
        self.shared.shutdown.cancel();
        self.tasks.detach_relays();
        let mut done = self.shared.done.watch();
        while !done.get() {
            if done.updated().await.is_err() {
                break;
            }
        }
    }

    /// Returns the node's tasks, for a handle that holds them weakly.
    pub(crate) fn tasks(&self) -> &Arc<Tasks> {
        &self.tasks
    }
}

impl ProtocolHandler for Moq {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        admission::accept(&self.shared, connection)
            .await
            .map_err(AnyError::from)?;
        Ok(())
    }

    /// Shuts the node down with the router, as [`Moq::shutdown`] does.
    async fn shutdown(&self) {
        // The inherent method, which takes precedence over this one.
        Self::shutdown(self).await;
    }
}

impl Shared {
    /// Returns the grant [`MoqConfig::grant`] gives `peer`.
    pub(crate) fn grant_for(&self, peer: EndpointId) -> Grant {
        self.grant
            .as_ref()
            .map_or_else(Grant::everything, |grant| grant(peer))
    }

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
