//! The node: one route table, its publications, and its links.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, RouterBuilder},
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
    RelayLink, Session, Subscription,
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
    /// A relay route that is in the table, or arrives during the dial, also
    /// resolves the path.
    Direct(EndpointId),
    /// Waits for an attached relay to route the path.
    Relays,
    /// Dials the publisher and waits on relays at the same time.
    Both(EndpointId),
}

/// How a [`Moq`] node runs.
#[derive(Clone, Default, derive_more::Debug)]
pub struct MoqConfig {
    /// How incoming sessions are admitted.
    pub admission: Admission,
    /// Returns the grant of a session with a peer, from its endpoint id.
    ///
    /// Used for sessions admitted under [`Admission::Open`], and for dials
    /// without [`ConnectOptions::grant`]. `None` grants everything: any peer
    /// may publish at any path.
    ///
    /// A grant's publish patterns decide which paths the peer may put into the
    /// route table. Let each peer publish only under paths that name it, or
    /// one peer can stand in for another.
    #[debug("{}", grant.is_some())]
    pub grant: Option<GrantFn>,
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
/// use iroh_moq::{Audience, Moq, MoqConfig};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let endpoint = Endpoint::bind(presets::Minimal).await?;
/// let moq = Moq::new(endpoint.clone(), MoqConfig::default());
/// let _router = moq.mount(Router::builder(endpoint)).spawn();
///
/// let broadcast = moq_net::broadcast::Info::new().produce();
/// // write tracks into `broadcast`, then:
/// let _publication = moq.publish("demo/camera", &broadcast, Audience::Everyone)?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, derive_more::Debug)]
#[debug("Moq({}, hop {})", shared.id.fmt_short(), shared.table.hop())]
pub struct Moq {
    pub(crate) shared: Arc<Shared>,
    /// Aborts the node's tasks when the last handle drops.
    pub(crate) tasks: Arc<Tasks>,
}

/// What every handle of a node shares.
pub(crate) struct Shared {
    pub(crate) endpoint: Endpoint,
    pub(crate) id: EndpointId,
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

/// The node's tasks, kept apart from [`Shared`] so the tasks can hold it.
///
/// Dropping the last [`Moq`] drops these, which ends the tasks and releases
/// the shared state.
pub(crate) struct Tasks {
    _actor: AbortOnDropHandle<()>,
    _table: AbortOnDropHandle<()>,
    /// Relay links, by link id.
    pub(crate) relays: Mutex<HashMap<u64, crate::relay::RelayTask>>,
}

impl Moq {
    /// Creates a node on `endpoint`.
    ///
    /// Must be called within a tokio runtime.
    pub fn new(endpoint: Endpoint, config: MoqConfig) -> Self {
        let id = endpoint.id();
        let (table, driver) = origin::Producer::new(origin::Config::new(hop_for(&id)));
        let table_task = tokio::spawn(
            async move {
                let err = moq_net::time::run(driver).await;
                debug!(%err, "route table finished");
            }
            .instrument(info_span!("route_table")),
        );
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_QUEUE);
        let shared = Arc::new(Shared {
            endpoint,
            id,
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
        info!(id = %id.fmt_short(), hop = %shared.table.hop(), admission = ?config.admission, "moq node started");
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
                _table: AbortOnDropHandle::new(table_task),
                relays: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Mounts the node on `router` under every ALPN in [`alpns`](crate::alpns).
    pub fn mount(&self, mut router: RouterBuilder) -> RouterBuilder {
        for alpn in crate::alpns() {
            router = router.accept(alpn, self.clone());
        }
        router
    }

    /// Returns the endpoint the node runs on.
    pub fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    /// Publishes `broadcast` at `path` to `audience`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Duplicate`] if another publication has the path,
    /// [`Error::InvalidPath`] for an empty path or one with a `*` segment, and
    /// [`Error::ShutDown`] once the node has shut down.
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
    /// If no route exists yet, reaches out as `reach` says.
    ///
    /// The table holds what direct peers announce within their grants, routes
    /// from attached relays, and this node's [`Audience::Everyone`]
    /// publications. With grants that keep each peer to paths that name it,
    /// a path resolves to its publisher's broadcast and never to a third
    /// peer's. A relay is trusted with every path it forwards, so attach only
    /// relays that keep each publisher to its own paths, as moq-relay's tokens
    /// do.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NoRoute`] if `reach` allows no way to reach the
    /// path, [`Error::Connect`] if the publisher cannot be dialed and no relay
    /// can stand in, [`Error::NotGranted`] if the publisher's grant does not
    /// let it publish the path and no relay can stand in,
    /// [`Error::NotAnnounced`] if the publisher's session ends before it
    /// announces the path, and [`Error::ShutDown`] once the node has shut
    /// down.
    pub async fn subscribe(&self, path: impl AsPath, reach: Reach) -> Result<Subscription, Error> {
        route::subscribe(self, path.as_path().to_owned(), reach).await
    }

    /// Returns the route table.
    ///
    /// It holds what [`subscribe`](Self::subscribe) resolves in: what direct
    /// peers announce within their grants, routes from attached relays, and
    /// this node's [`Audience::Everyone`] publications. Its
    /// [`announced`](origin::Consumer::announced) stream lists those paths as
    /// they come and go.
    pub fn origin(&self) -> origin::Consumer {
        self.shared.table.consume()
    }

    /// Returns the session with `peer`, dialing if there is none.
    ///
    /// Concurrent calls share one dial. The dialer's half of the handshake
    /// completes before the peer decides whether to admit it, so a peer that
    /// refuses usually closes the session right after this returns.
    /// [`Session::closed`] then says why.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Connect`] if the dial fails, [`Error::Refused`] if
    /// a peer on an older protocol version refuses during the handshake, and
    /// [`Error::ShutDown`] once the node has shut down.
    pub async fn connect(&self, peer: impl Into<EndpointAddr>) -> Result<Session, Error> {
        self.connect_with(peer, ConnectOptions::default()).await
    }

    /// Dials `peer` with a token, a cost, or a grant.
    ///
    /// An existing session with the peer, or a dial in flight to it, is
    /// returned as it is, and the options go unused.
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

    /// Waits for the next session that needs admission.
    ///
    /// Yields only under [`Admission::Manual`]. Returns `None` once the node
    /// shuts down.
    pub async fn accept(&self) -> Option<Incoming> {
        admission::next(&self.shared).await
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
    /// [`attach_relay`](Self::attach_relay) fail, and
    /// [`accept`](Self::accept) returns `None`. Gives sessions a short grace
    /// to tell their peers, so the endpoint can be closed once this returns.
    /// Idempotent. Not cancellation safe: call it again to finish.
    pub async fn shutdown(&self) {
        // Set first, so a racing publish or attach fails instead of being
        // added and then cleared.
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
        // Resolves to the inherent method.
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
