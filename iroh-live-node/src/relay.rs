//! Relay links: staying attached to a moq relay.
//!
//! A relay link feeds what the relay announces into the route table, tagged
//! [`Link::Upstream`], so a route from a relay is never offered to another
//! link. moq-tokio has no iroh transport, so `iroh://` relays are dialed
//! through the node's own endpoint by [`iroh_uplink`], which drives moq's
//! connect policy, [`Reconnect`], as moq-tokio does for the other schemes.

use std::{
    pin::Pin,
    sync::{Arc, Weak},
    task::Poll,
    time::Duration,
};

use iroh::EndpointId;
use moq_net::{
    Reconnect, auth,
    auth::Token,
    origin::{self, Link},
    reconnect::{self, Action, Attempt, Verdict},
};
use n0_error::{AnyError, e};
use n0_future::{
    join_all,
    task::{AbortOnDropHandle, JoinSet},
};
use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};
use url::Url;

use crate::{
    Error, LinkId, Node,
    handshake::Hop,
    link::{self, LatestSample},
    node::{Shared, Tasks},
    rate::NodeRate,
    state::{LinkEntry, LinkRole, served},
    transport,
};

/// The cost a relay link adds to every route through it.
///
/// Higher than a direct link's cost of one, so a direct route wins while it
/// exists.
const RELAY_COST: u64 = 10;

/// How to attach to a relay.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RelayConfig {
    /// The relay to dial: `iroh://<endpoint-id>/<path>` or `https://host/<path>`.
    ///
    /// The path and query are the ones the session requests, such as a
    /// ticket the relay pulls. A `jwt` in the query is the connection
    /// credential, ahead of [`tokens`](Self::tokens).
    pub url: Url,
    /// The tokens the node presents to the relay, the first as the connection credential.
    ///
    /// Every dial presents them, in the MoQ SETUP and as `?jwt=` in the URL,
    /// since the SETUP of moq-lite-06 has no token field. Replace them on a
    /// live link through [`RelayLink::tokens`], before the old ones expire.
    pub tokens: Vec<Token>,
    /// Whether the relay's routes enter the node's route table.
    ///
    /// A node that only publishes through the relay should turn this off.
    /// Either way the relay is offered every publication whose audience is
    /// [`Granted`](crate::Audience::Granted), since a relay link's grant
    /// covers every path: publish to [`Peers`](crate::Audience::Peers) to
    /// keep a publication off it.
    pub consume: bool,
}

impl RelayConfig {
    /// Creates a config for the relay at `url`, consuming what the relay routes.
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self {
            url,
            tokens: Vec::new(),
            consume: true,
        }
    }

    /// Returns this config with `consume` saying whether the relay's routes enter the route table.
    ///
    /// See [`consume`](Self::consume).
    #[must_use]
    pub fn with_consume(mut self, consume: bool) -> Self {
        self.consume = consume;
        self
    }

    /// Creates a config for the relay with endpoint id `relay`, dialed over iroh.
    #[must_use]
    pub fn iroh(relay: EndpointId) -> Self {
        let url = format!("iroh://{relay}/")
            .parse()
            .expect("an endpoint id is a valid URL host");
        Self::new(url)
    }

    /// Presents `token` as the connection credential.
    ///
    /// # Examples
    ///
    /// ```
    /// use iroh_live_node::RelayConfig;
    ///
    /// let relay = iroh::SecretKey::generate().public();
    /// let token = "a.signed.jwt".parse().expect("not empty");
    /// let config = RelayConfig::iroh(relay).with_token(token);
    /// assert_eq!(config.tokens.len(), 1);
    /// ```
    #[must_use]
    pub fn with_token(mut self, token: Token) -> Self {
        self.tokens = vec![token];
        self
    }
}

/// The state of a relay link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RelayStatus {
    /// Dialing for the first time.
    #[default]
    Connecting,
    /// A session with the relay is up.
    Connected,
    /// The session dropped, and the link redials with backoff.
    Reconnecting,
    /// The link was detached, or the node shut down.
    Detached,
}

/// A relay this node stays attached to.
///
/// Cheap to clone. The link stays until [`detach`](Self::detach) or the node's
/// shutdown. Dropping the handles leaves it in place.
///
/// # Examples
///
/// ```no_run
/// # async fn run(node: iroh_live_node::Node, relay: iroh::EndpointId)
/// # -> Result<(), iroh_live_node::Error> {
/// use iroh_live_node::{RelayConfig, RelayStatus};
/// use n0_watcher::Watcher;
///
/// let link = node.attach_relay(RelayConfig::iroh(relay))?;
/// let mut status = link.status();
/// while status.get() != RelayStatus::Connected {
///     status.updated().await.ok();
/// }
/// link.detach().await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RelayLink {
    inner: Arc<RelayInner>,
}

/// What every clone of a [`RelayLink`] shares.
#[derive(derive_more::Debug)]
struct RelayInner {
    link: LinkId,
    url: Url,
    status: Watchable<RelayStatus>,
    #[debug(skip)]
    tokens: auth::Tokens,
    #[debug(skip)]
    uplink: Uplink,
    #[debug(skip)]
    shared: Weak<Shared>,
    #[debug(skip)]
    tasks: Weak<Tasks>,
}

impl RelayLink {
    /// Returns the relay's status, as it changes.
    #[must_use]
    pub fn status(&self) -> n0_watcher::Direct<RelayStatus> {
        self.inner.status.watch()
    }

    /// Returns this link's id, as [`ServingLink::id`](crate::ServingLink::id) names it.
    #[must_use]
    pub fn id(&self) -> LinkId {
        self.inner.link
    }

    /// Returns the tokens the link presents, seeded from [`RelayConfig::tokens`].
    ///
    /// [`Tokens::set`](auth::Tokens::set) replaces them for every later dial,
    /// and a relay that refused the old ones is dialed again at once. Set new
    /// ones before the old ones expire.
    #[must_use]
    pub fn tokens(&self) -> auth::Tokens {
        self.inner.tokens.clone()
    }

    /// Detaches from the relay.
    ///
    /// Closes the session, stops redialing and withdraws the relay's routes.
    /// Idempotent. Cancellation safe: the link's task finishes the close.
    pub async fn detach(&self) {
        info!(url = %self.inner.url, "detaching relay");
        match &self.inner.uplink {
            Uplink::Tokio(connection) => {
                connection.abort(moq_net::Error::Cancel);
                // An error only says the session was already gone.
                connection.closed().await.ok();
            }
            Uplink::Iroh { stop, .. } => stop.cancel(),
        }
        if let Some(tasks) = self.inner.tasks.upgrade()
            && let Some(relay) = tasks.remove_relay(self.inner.link)
        {
            // Let the task close its session before it is dropped, which
            // would abort it.
            relay.finish().await;
        }
        if let Some(shared) = self.inner.shared.upgrade() {
            shared
                .state
                .lock()
                .expect("poisoned")
                .remove_link(self.inner.link);
        }
        self.inner.status.set(RelayStatus::Detached).ok();
    }
}

/// A relay link's task, and the status it reports.
pub(crate) struct RelayTask {
    task: AbortOnDropHandle<()>,
    status: Watchable<RelayStatus>,
}

impl Tasks {
    /// Keeps a relay link's task running until the link detaches.
    fn insert_relay(&self, link: LinkId, task: RelayTask) {
        let mut relays = self.relays.lock().expect("poisoned");
        // Drop the handles of links whose client gave up.
        relays.retain(|_, relay| !relay.task.is_finished());
        relays.insert(link, task);
    }

    /// Takes a relay link's task out, to be finished.
    fn remove_relay(&self, link: LinkId) -> Option<RelayTask> {
        self.relays.lock().expect("poisoned").remove(&link)
    }

    /// Aborts every relay link's task, reporting each detached.
    ///
    /// An aborted task never reaches its own report, so this makes it.
    pub(crate) fn detach_relays(&self) {
        let relays = std::mem::take(&mut *self.relays.lock().expect("poisoned"));
        for (_, relay) in relays {
            drop(relay.task);
            relay.status.set(RelayStatus::Detached).ok();
        }
    }

    /// Waits for every relay link's task to close its session.
    ///
    /// The node's shutdown token must be cancelled already.
    pub(crate) async fn shutdown_relays(&self) {
        let relays = std::mem::take(&mut *self.relays.lock().expect("poisoned"));
        join_all(relays.into_values().map(RelayTask::finish)).await;
    }
}

impl RelayTask {
    /// Waits for the task to end, then reports the link detached.
    ///
    /// Aborts the task if it outlives the shutdown grace.
    async fn finish(mut self) {
        if tokio::time::timeout(crate::actor::SHUTDOWN_GRACE, &mut self.task)
            .await
            .is_err()
        {
            warn!("relay task did not stop in time, aborting");
        }
        self.status.set(RelayStatus::Detached).ok();
    }
}

/// Attaches `node` to a relay, for [`Node::attach_relay`].
pub(crate) fn attach(node: &Node, config: RelayConfig) -> Result<RelayLink, Error> {
    let shared = &node.shared;
    if shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let iroh_relay = iroh_relay(&config.url)?;
    // A relay that declares a hop of its own is matched by kind instead.
    let hop = Hop::of(&iroh_relay.unwrap_or_else(|| iroh::SecretKey::generate().public()));
    let subscriber = config
        .consume
        .then(|| shared.table.clone().with_link(Link::Upstream));
    let (uplink, tokens) = uplink(shared, &config, iroh_relay, &hop, subscriber.clone())?;
    let latest_sample = LatestSample::default();
    let link = register(
        shared,
        &uplink,
        LinkEntry {
            role: LinkRole::Relay {
                consume: config.consume,
            },
            hop,
            auth: tokens.handle(),
            latest_sample: latest_sample.clone(),
        },
    )?;
    let url = config.url.clone();
    info!(%url, consume = config.consume, "attaching relay");
    let status = Watchable::new(RelayStatus::Connecting);
    let task = spawn_uplink(
        &uplink,
        LinkParts {
            shared: shared.clone(),
            config,
            tokens: tokens.clone(),
            subscriber,
            link,
            latest_sample,
            status: status.clone(),
        },
    );
    node.tasks.insert_relay(
        link,
        RelayTask {
            task: AbortOnDropHandle::new(task),
            status: status.clone(),
        },
    );
    // A shutdown that ran between the check above and the insert found no
    // task to stop, so stop it here instead.
    if shared.shutdown.is_cancelled() {
        node.tasks.detach_relays();
        return Err(e!(Error::ShutDown));
    }
    Ok(RelayLink {
        inner: Arc::new(RelayInner {
            link,
            url,
            status,
            tokens,
            uplink,
            shared: Arc::downgrade(shared),
            tasks: Arc::downgrade(&node.tasks),
        }),
    })
}

/// Returns the endpoint id an `iroh://` relay URL names, or `None` for another scheme.
fn iroh_relay(url: &Url) -> Result<Option<EndpointId>, Error> {
    match url.scheme() {
        "iroh" => url
            .host_str()
            .and_then(|host| host.parse::<EndpointId>().ok())
            .map(Some)
            .ok_or_else(|| {
                e!(Error::InvalidRelayUrl {
                    url: Box::new(url.clone()),
                    reason: "the host is not an endpoint id",
                })
            }),
        "https" | "http" | "moqt" | "moql" => Ok(None),
        _ => Err(e!(Error::InvalidRelayUrl {
            url: Box::new(url.clone()),
            reason: "unsupported scheme, use iroh:// or https://",
        })),
    }
}

/// Builds the uplink to a relay, and the tokens its sessions present.
///
/// A moq-tokio connection starts dialing at once and keeps its own tokens
/// across redials. The tokens' handle is the authorization every session of
/// the link shares.
fn uplink(
    shared: &Arc<Shared>,
    config: &RelayConfig,
    iroh_relay: Option<EndpointId>,
    hop: &Hop,
    subscriber: Option<origin::Producer>,
) -> Result<(Uplink, auth::Tokens), Error> {
    if let Some(relay) = iroh_relay {
        let uplink = Uplink::Iroh {
            relay,
            stop: CancellationToken::new(),
        };
        return Ok((uplink, auth::Tokens::new(config.tokens.clone())));
    }
    let mut tokio_config = moq_tokio::client::Config::default();
    tokio_config.connect.tokens = config.tokens.clone();
    let mut client = tokio_config
        .init()
        .map_err(|err| {
            e!(Error::Relay {
                source: Arc::new(AnyError::from_std(err))
            })
        })?
        .with_publisher(served(&shared.table))
        .with_peer_hop(hop.id())
        .with_cost(RELAY_COST);
    if let Some(subscriber) = subscriber {
        client = client.with_subscriber(subscriber);
    }
    let connection = client.connect(config.url.clone());
    let tokens = connection.tokens();
    Ok((Uplink::Tokio(Box::new(connection)), tokens))
}

/// Registers a relay link in the node's state, and returns its id.
///
/// Fails, and stops the uplink, if the node closed meanwhile.
fn register(shared: &Shared, uplink: &Uplink, entry: LinkEntry) -> Result<LinkId, Error> {
    let mut state = shared.state.lock().expect("poisoned");
    if state.closed {
        if let Uplink::Tokio(connection) = uplink {
            connection.abort(moq_net::Error::Cancel);
        }
        return Err(e!(Error::ShutDown));
    }
    let link = LinkId(state.next_id());
    state.add_link(link, entry);
    Ok(link)
}

/// What a relay link's uplink task owns.
struct LinkParts {
    shared: Arc<Shared>,
    config: RelayConfig,
    /// The tokens the link's sessions present, and the authorization they share.
    tokens: auth::Tokens,
    /// Where the relay's announcements go, if the link consumes them.
    subscriber: Option<origin::Producer>,
    link: LinkId,
    latest_sample: LatestSample,
    status: Watchable<RelayStatus>,
}

/// Spawns the task that runs a relay link's uplink.
fn spawn_uplink(uplink: &Uplink, parts: LinkParts) -> tokio::task::JoinHandle<()> {
    let span = info_span!("relay", url = %parts.config.url, link = %parts.link);
    match uplink {
        Uplink::Iroh { relay, stop } => {
            tokio::spawn(iroh_uplink(parts, *relay, stop.clone()).instrument(span))
        }
        Uplink::Tokio(connection) => {
            let run = tokio_uplink(
                parts.shared,
                moq_tokio::Connection::clone(connection),
                parts.link,
                parts.latest_sample,
                parts.status,
            );
            tokio::spawn(run.instrument(span))
        }
    }
}

/// How a relay link reaches its relay.
enum Uplink {
    /// A moq-tokio client connection, which redials itself.
    Tokio(Box<moq_tokio::Connection>),
    /// An iroh dial loop to the relay with endpoint id `relay`, stopped by cancelling `stop`.
    Iroh {
        relay: EndpointId,
        stop: CancellationToken,
    },
}

/// The first pause before redialing a relay.
const REDIAL_MIN: Duration = Duration::from_secs(1);

/// How long a session must last to reset the redial pause to [`REDIAL_MIN`].
const STABLE_SESSION: Duration = Duration::from_secs(10);

/// The longest pause between redials.
const REDIAL_MAX: Duration = Duration::from_secs(30);

/// Runs a moq-tokio relay link until its client gives up.
async fn tokio_uplink(
    shared: Arc<Shared>,
    connection: moq_tokio::Connection,
    link: LinkId,
    latest_sample: LatestSample,
    status: Watchable<RelayStatus>,
) {
    let monitor = link::monitor_relay(connection.monitor(), latest_sample);
    let mut watch = connection.clone();
    // Aborts the connection for every clone when the task ends, so a
    // `RelayLink` handle held past shutdown does not keep redialing.
    let _stop = StopOnDrop(connection);
    let _monitor = AbortOnDropHandle::new(tokio::spawn(monitor));
    loop {
        let update = tokio::select! {
            // The connection aborts as the task ends.
            _ = shared.shutdown.cancelled() => break,
            update = watch.status() => update,
        };
        match update {
            Ok(moq_tokio::Status::Connected | moq_tokio::Status::Migrating) => {
                info!("relay connected");
                status.set(RelayStatus::Connected).ok();
            }
            Ok(moq_tokio::Status::Disconnected) => {
                warn!("relay session dropped, redialing");
                status.set(RelayStatus::Reconnecting).ok();
            }
            Ok(other) => debug!(?other, "relay status"),
            // A detach aborts the connection, which is no failure.
            Err(moq_tokio::Error::Stopped) => {
                debug!("relay link stopped");
                break;
            }
            Err(err) => {
                warn!(error = %err, "relay link ended");
                break;
            }
        }
    }
    status.set(RelayStatus::Detached).ok();
    shared.state.lock().expect("poisoned").remove_link(link);
}

/// Returns moq's connect policy for an iroh relay link.
///
/// The link redials forever with backoff from [`REDIAL_MIN`] to
/// [`REDIAL_MAX`]. An endpoint id names one node, so a GOAWAY that names no
/// new address keeps the drained session until the relay closes it.
fn policy_config() -> reconnect::Config {
    let mut config = reconnect::Config::default();
    config.backoff.initial = REDIAL_MIN;
    config.backoff.max = Some(REDIAL_MAX);
    config.backoff.healthy = STABLE_SESSION;
    config.restart = reconnect::Restart::Wait;
    config
}

/// Runs an `iroh://` relay link until it is stopped.
///
/// moq's connect policy ends when its first walk reaches no session, and when
/// the relay refuses tokens that did not change since. This starts it again
/// after a pause instead, so the link stays attached to a relay that is down
/// at attach time. The pause doubles up to [`REDIAL_MAX`], and resets when a
/// run established a session.
async fn iroh_uplink(parts: LinkParts, relay: EndpointId, stop: CancellationToken) {
    let LinkParts {
        shared,
        config,
        tokens,
        subscriber,
        link,
        latest_sample,
        status,
    } = parts;
    let mut client = moq_net::Client::new()
        .with_publisher(served(&shared.table))
        .with_peer_hop(Hop::of(&relay).id())
        .with_cost(RELAY_COST);
    if let Some(subscriber) = subscriber {
        client = client.with_subscriber(subscriber);
    }
    if let Some(rate) = &shared.rate {
        client = client.with_allocator(rate.allocator());
    }
    let mut uplink = IrohUplink {
        rate: shared.rate.clone(),
        endpoint: shared.endpoint.clone(),
        client,
        latest_sample,
        sampler: link::SharedSampler::default(),
        status: status.clone(),
        monitor: None,
        drivers: JoinSet::new(),
        established: false,
    };
    let mut pause = REDIAL_MIN;
    loop {
        let mut policy =
            Reconnect::new(policy_config(), vec![config.url.clone()], kio::clock::now())
                .expect("one target and a nonzero healthy threshold")
                .with_tokens(tokens.clone());
        let ended = tokio::select! {
            _ = stop.cancelled() => None,
            _ = shared.shutdown.cancelled() => None,
            ended = uplink.run(&mut policy) => Some(ended),
        };
        let Some(ended) = ended else {
            uplink.close(&policy).await;
            break;
        };
        if std::mem::take(&mut uplink.established) {
            pause = REDIAL_MIN;
        }
        // Equal jitter, half the pause plus a random half, so relays that
        // dropped together do not redial together.
        let jittered = pause.mul_f64(jitter_factor());
        match ended {
            Ok(()) => info!(pause = ?jittered, "relay link ended, starting over"),
            Err(err) => warn!(error = %err, pause = ?jittered, "relay link gave up, starting over"),
        }
        status.set(RelayStatus::Reconnecting).ok();
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = shared.shutdown.cancelled() => break,
            _ = tokio::time::sleep(jittered) => {}
        }
        pause = (pause * 2).min(REDIAL_MAX);
    }
    uplink.offline();
    status.set(RelayStatus::Detached).ok();
    shared.state.lock().expect("poisoned").remove_link(link);
}

/// Why a dial or a session of an iroh relay link failed.
#[derive(Debug, Clone, derive_more::Display, derive_more::From)]
enum LinkFailure {
    /// The MoQ handshake failed, or the session closed with this error.
    Moq(moq_net::Error),
    /// moq's connect policy decided on a dial or a redirect.
    Policy(reconnect::Failure),
    /// The relay could not be reached, or a redirect named no iroh relay.
    Dial(Error),
}

/// A session with the relay, as a dial hands it over.
struct Dialed {
    session: moq_net::Session,
    driver: moq_net::Driver,
    connection: iroh::endpoint::Connection,
}

/// A dial in flight, with the verdict on its failure.
type Dialing = Pin<Box<dyn Future<Output = Result<Dialed, (LinkFailure, Verdict)>> + Send>>;

/// What an iroh relay link keeps across the runs of its connect policy.
struct IrohUplink {
    /// The node's rate control, which each dial's transport follows.
    rate: Option<Arc<NodeRate>>,
    endpoint: iroh::Endpoint,
    /// The client every dial starts from, before the policy adds the URL and tokens.
    client: moq_net::Client,
    latest_sample: LatestSample,
    sampler: link::SharedSampler,
    status: Watchable<RelayStatus>,
    /// Reads the live session into `latest_sample`.
    monitor: Option<AbortOnDropHandle<()>>,
    /// The drivers of the live session and of one draining after a GOAWAY.
    drivers: JoinSet<()>,
    /// Whether the current run of the policy established a session.
    established: bool,
}

impl IrohUplink {
    /// Drives `policy` until it ends, dialing each attempt it asks for.
    async fn run(&mut self, policy: &mut Reconnect<Url, LinkFailure>) -> Result<(), LinkFailure> {
        let mut dial: Option<Dialing> = None;
        let mut timer: Option<kio::clock::Sleep> = None;
        loop {
            let action = kio::wait(|waiter| {
                loop {
                    if let Some(pending) = dial.as_mut()
                        && let Poll::Ready(result) = waiter.poll_future(pending.as_mut())
                    {
                        dial = None;
                        match result {
                            Ok(dialed) => self.connected(policy, dialed),
                            Err((err, verdict)) => policy.failed(kio::clock::now(), err, verdict),
                        }
                    }
                    let at = match policy.poll(kio::clock::now(), waiter) {
                        Action::Wait(Some(at)) => at,
                        Action::Wait(None) => return Poll::Pending,
                        action => return Poll::Ready(action),
                    };
                    let sleep = timer.get_or_insert_with(|| kio::clock::sleep_until(at));
                    if sleep.deadline() != at {
                        sleep.reset(at);
                    }
                    if waiter.poll_future(Pin::new(sleep)).is_pending() {
                        return Poll::Pending;
                    }
                }
            })
            .await;
            match action {
                Action::Dial(attempt) => {
                    debug!("dialing the relay");
                    dial = Some(Box::pin(dial_relay(
                        self.endpoint.clone(),
                        self.client.clone(),
                        self.rate.clone(),
                        attempt,
                    )));
                }
                // The policy counted the timeout already. Dropping the dial cancels it.
                Action::Abandon => dial = None,
                Action::Migrating => info!("the relay sent a GOAWAY, migrating"),
                Action::Retired => debug!("the drained relay session ended"),
                Action::Offline(err) => {
                    match err {
                        Some(err) => warn!(error = %err, "relay session dropped, redialing"),
                        None => warn!("relay session dropped, redialing"),
                    }
                    self.offline();
                    self.status.set(RelayStatus::Reconnecting).ok();
                }
                Action::Retrying(err) => {
                    warn!(error = %err, "relay dial failed, redialing");
                    self.status.set(RelayStatus::Reconnecting).ok();
                }
                // Only a link that reports a fallback transport is asked to probe.
                Action::Probe(_) | Action::Wait(_) => {}
                Action::Done(result) => return result,
            }
        }
    }

    /// Hands `dialed` to the policy, and starts its driver and its monitor.
    fn connected(&mut self, policy: &mut Reconnect<Url, LinkFailure>, dialed: Dialed) {
        let Dialed {
            session,
            driver,
            connection,
        } = dialed;
        info!("relay connected");
        self.established = true;
        while self.drivers.try_join_next().is_some() {}
        self.drivers.spawn(async move {
            moq_net::time::run(driver).await;
        });
        // A new session is a new path, even if QUIC numbers it alike.
        self.sampler.lock().expect("poisoned").new_session();
        self.monitor = Some(AbortOnDropHandle::new(tokio::spawn(link::monitor_direct(
            session.clone(),
            connection,
            self.latest_sample.clone(),
            self.sampler.clone(),
        ))));
        policy.connected(kio::clock::now(), session);
        self.status.set(RelayStatus::Connected).ok();
    }

    /// Stops reading the session that ended, and clears its last reading.
    fn offline(&mut self) {
        // The monitor must not overwrite the cleared reading.
        self.monitor = None;
        let generation = self.sampler.lock().expect("poisoned").path_generation();
        self.latest_sample.clear(generation);
    }

    /// Closes the live session and one draining after a GOAWAY, letting each finish.
    async fn close(&self, policy: &Reconnect<Url, LinkFailure>) {
        let sessions = policy
            .session()
            .into_iter()
            .chain(policy.predecessor().map(|(session, _)| session))
            .cloned();
        // An error only says the session was gone already.
        join_all(sessions.map(|session| async move { session.close().await.ok() })).await;
    }
}

/// Dials the relay `attempt` names and completes the MoQ handshake.
async fn dial_relay(
    endpoint: iroh::Endpoint,
    client: moq_net::Client,
    rate: Option<Arc<NodeRate>>,
    attempt: Attempt<Url>,
) -> Result<Dialed, (LinkFailure, Verdict)> {
    let unusable = |err: Error| (LinkFailure::Dial(err), Verdict::Unusable);
    let relay = iroh_relay(attempt.target())
        .map_err(unusable)?
        .ok_or_else(|| {
            unusable(e!(Error::InvalidRelayUrl {
                url: Box::new(attempt.target().clone()),
                reason: "a GOAWAY moved an iroh link to another scheme",
            }))
        })?;
    let retry = |err: transport::Error| (LinkFailure::Dial(err.into()), Verdict::Retry);
    let transport = match &rate {
        Some(rate) => {
            let connection = endpoint
                .connect(relay, crate::ALPN)
                .await
                .map_err(|err| retry(transport::Error::connect(err)))?;
            let transport =
                transport::Transport::with_rate(connection, rate.transport()).map_err(retry)?;
            rate.register(transport.rate());
            transport
        }
        None => transport::Transport::connect(&endpoint, relay)
            .await
            .map_err(retry)?,
    };
    let connection = transport.connection().clone();
    let (session, driver) = attempt
        .client(&client)
        .connect(kio::clock::now(), transport)
        .await
        .map_err(|err| {
            let verdict = Verdict::from(&err);
            (LinkFailure::Moq(err), verdict)
        })?;
    Ok(Dialed {
        session,
        driver,
        connection,
    })
}

/// Returns a factor in `[0.5, 1)` from the clock, random enough to spread redials.
fn jitter_factor() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    0.5 + f64::from(nanos % 1_000_000) / 2_000_000.0
}

/// A guard that aborts a moq-tokio connection when dropped.
struct StopOnDrop(moq_tokio::Connection);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.abort(moq_net::Error::Cancel);
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn iroh_config_names_the_endpoint_id() {
        let id = SecretKey::generate().public();
        let config = RelayConfig::iroh(id);
        assert_eq!(config.url.scheme(), "iroh");
        assert_eq!(config.url.host_str(), Some(id.to_string().as_str()));
    }

    #[test]
    fn an_iroh_url_names_its_relay_by_endpoint_id() {
        let id = SecretKey::generate().public();
        let url: Url = format!("iroh://{id}/live").parse().expect("url");
        assert_eq!(iroh_relay(&url).expect("valid"), Some(id));
        for other in [
            "https://relay.example/",
            "http://relay.example/",
            "moqt://relay.example/",
        ] {
            let url: Url = other.parse().expect("url");
            assert_eq!(iroh_relay(&url).expect(other), None, "{other}");
        }
    }

    #[test]
    fn an_iroh_url_without_an_endpoint_id_is_invalid() {
        let url: Url = "iroh://relay.example/".parse().expect("url");
        let err = iroh_relay(&url).expect_err("no endpoint id");
        assert!(
            matches!(err, Error::InvalidRelayUrl { reason, .. } if reason.contains("endpoint id")),
            "{err:#}"
        );
    }

    #[test]
    fn an_unsupported_scheme_is_invalid() {
        let url: Url = "ftp://relay.example/".parse().expect("url");
        let err = iroh_relay(&url).expect_err("unsupported");
        assert!(
            matches!(err, Error::InvalidRelayUrl { reason, .. } if reason.contains("scheme")),
            "{err:#}"
        );
    }

    #[test]
    fn the_iroh_policy_redials_forever_and_waits_out_a_restart() {
        let config = policy_config();
        assert_eq!(config.backoff.timeout, None, "gives up after a while");
        assert_eq!(config.backoff.initial, REDIAL_MIN);
        assert_eq!(config.backoff.max, Some(REDIAL_MAX));
        assert_eq!(config.backoff.healthy, STABLE_SESSION);
        assert_eq!(config.restart, reconnect::Restart::Wait);
        assert!(!config.once);
    }
}
