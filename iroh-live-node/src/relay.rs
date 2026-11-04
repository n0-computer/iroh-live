//! Relay links: staying attached to a moq relay.
//!
//! A relay link serves the route table's local view, authorized for the
//! node's [`Everyone`](crate::Audience::Everyone) publications when its
//! [`RelayOffer`] is public and for nothing otherwise. It feeds what the relay
//! announces into the route table, tagged [`Link::Upstream`]: the local view
//! hides such routes, so a route from a relay is never offered to another link.
//!
//! `https://`, `moqt://` and `moql://` relays are moq-tokio client
//! connections, which redial with backoff. moq-tokio has no iroh transport, so
//! `iroh://` relays are dialed through the node's own endpoint by
//! [`iroh_uplink`], which redials the same way.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use iroh::EndpointId;
use moq_net::{
    auth,
    origin::{self, Link},
};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};
use url::Url;

use crate::{
    ConnectOptions, Error, Grant, LinkId, LinkKind, LinkSample, Node,
    link::{self, LinkState},
    node::{Shared, Tasks},
    session::{client_handshake, hop_for},
    state::{LinkEntry, served},
    transport,
};

/// The cost a relay link adds to every route through it, unless configured.
///
/// Higher than a direct link's cost of one, so a direct route wins while it
/// exists.
pub const DEFAULT_RELAY_COST: u64 = 10;

/// How to attach to a relay.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// The relay to dial: `iroh://<endpoint-id>/<path>` or `https://host/<path>`.
    pub url: Url,
    /// Sent as `?jwt=` in the URL, and over iroh also in the MoQ SETUP.
    pub token: Option<String>,
    /// Added to every route through this relay.
    ///
    /// Defaults to [`DEFAULT_RELAY_COST`].
    pub cost: u64,
    /// What the node publishes into the relay.
    pub offer: RelayOffer,
    /// Whether the relay's routes enter the node's route table.
    ///
    /// A node that only publishes through the relay should turn this off.
    pub consume: bool,
}

impl RelayConfig {
    /// Creates a config for the relay at `url`.
    ///
    /// It offers every public publication and consumes what the relay routes.
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self {
            url,
            token: None,
            cost: DEFAULT_RELAY_COST,
            offer: RelayOffer::default(),
            consume: true,
        }
    }

    /// Creates a config for the relay with endpoint id `relay`, dialed over iroh.
    #[must_use]
    pub fn iroh(relay: EndpointId) -> Self {
        let url = format!("iroh://{relay}/")
            .parse()
            .expect("an endpoint id is a valid URL host");
        Self::new(url)
    }

    /// Returns the URL to dial, with the token in its query.
    fn dial_url(&self) -> Url {
        let mut url = self.url.clone();
        if let Some(token) = &self.token {
            url.query_pairs_mut().append_pair("jwt", token);
        }
        url
    }
}

/// What a node publishes into a relay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RelayOffer {
    /// Every publication whose audience is [`Everyone`](crate::Audience::Everyone).
    #[default]
    Public,
    /// Nothing: the relay is for consuming only.
    Nothing,
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
#[derive(Debug, Clone)]
pub struct RelayLink {
    inner: Arc<RelayInner>,
}

#[derive(derive_more::Debug)]
struct RelayInner {
    link: u64,
    url: Url,
    link_state: LinkState,
    status: Watchable<RelayStatus>,
    #[debug(skip)]
    uplink: Uplink,
    #[debug(skip)]
    shared: Weak<Shared>,
    #[debug(skip)]
    tasks: Weak<Tasks>,
}

impl RelayLink {
    /// Returns the relay's status, as it changes.
    pub fn status(&self) -> n0_watcher::Direct<RelayStatus> {
        self.inner.status.watch()
    }

    /// Returns the URL the link dials, without its token.
    pub fn url(&self) -> &Url {
        &self.inner.url
    }

    /// Returns this link's id, as [`ServingLink::id`](crate::ServingLink::id) names it.
    pub fn id(&self) -> LinkId {
        LinkId(self.inner.link)
    }

    /// Returns the connection monitor's latest reading of this link.
    ///
    /// Empty between sessions. Each new session bumps the path generation.
    pub fn link(&self) -> LinkSample {
        self.inner.link_state.get()
    }

    /// Detaches from the relay.
    ///
    /// Closes the session, stops redialing and withdraws the relay's routes.
    /// Returns once the session has closed. Idempotent.
    ///
    /// Cancellation safe: the link's task finishes the close.
    pub async fn detach(&self) {
        info!(url = %self.inner.url, "detaching relay");
        match &self.inner.uplink {
            Uplink::Tokio(connection) => {
                connection.abort(moq_net::Error::Cancel);
                // An error only says the session was already gone.
                connection.closed().await.ok();
            }
            Uplink::Iroh(stop) => stop.cancel(),
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
    fn insert_relay(&self, link: u64, task: RelayTask) {
        let mut relays = self.relays.lock().expect("poisoned");
        // Drop the handles of links whose client gave up.
        relays.retain(|_, relay| !relay.task.is_finished());
        relays.insert(link, task);
    }

    /// Takes a relay link's task out, to be finished.
    fn remove_relay(&self, link: u64) -> Option<RelayTask> {
        self.relays.lock().expect("poisoned").remove(&link)
    }

    /// Aborts every relay link's task, reporting each detached.
    ///
    /// An aborted task never reaches its own report, so this makes it. Use it
    /// where waiting is not possible. [`shutdown_relays`](Self::shutdown_relays)
    /// lets sessions close.
    pub(crate) fn detach_relays(&self) {
        let relays = std::mem::take(&mut *self.relays.lock().expect("poisoned"));
        for (_, relay) in relays {
            drop(relay.task);
            relay.status.set(RelayStatus::Detached).ok();
        }
    }

    /// Waits for every relay link's task to close its session.
    ///
    /// Each task gets up to the shutdown grace. The node's shutdown token must
    /// be cancelled already.
    pub(crate) async fn shutdown_relays(&self) {
        let relays = std::mem::take(&mut *self.relays.lock().expect("poisoned"));
        for (_, relay) in relays {
            relay.finish().await;
        }
    }
}

impl RelayTask {
    /// Ends the task and reports the link detached.
    ///
    /// Waits up to the shutdown grace for the task to end by itself, and then
    /// aborts it.
    async fn finish(mut self) {
        if tokio::time::timeout(crate::session::SHUTDOWN_GRACE, &mut self.task)
            .await
            .is_err()
        {
            warn!("relay task did not stop in time, aborting");
        }
        self.status.set(RelayStatus::Detached).ok();
    }
}

/// Attaches `moq` to a relay, for [`Node::attach_relay`].
pub(crate) fn attach(moq: &Node, config: RelayConfig) -> Result<RelayLink, Error> {
    let shared = &moq.shared;
    if shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    match config.url.scheme() {
        "iroh" | "https" | "http" | "moqt" | "moql" => {}
        scheme => {
            return Err(e!(Error::Relay {
                source: AnyError::from_string(format!(
                    "unsupported relay URL scheme {scheme:?}, use iroh:// or https://"
                )),
            }));
        }
    }
    let iroh_relay = match config.url.scheme() {
        "iroh" => Some(
            config
                .url
                .host_str()
                .and_then(|host| host.parse::<EndpointId>().ok())
                .ok_or_else(|| {
                    e!(Error::Relay {
                        source: AnyError::from_string(format!(
                            "{} names no endpoint id",
                            config.url
                        )),
                    })
                })?,
        ),
        _ => None,
    };
    // Routes the relay announces come from this hop. A relay over iroh is
    // known by its endpoint id. moq-tokio's redials keep the one hop assigned
    // here, and a relay that declares a hop of its own is matched by kind.
    let hop = iroh_relay
        .as_ref()
        .map_or_else(moq_net::Hop::random, hop_for);
    // Routes from a relay are tagged upstream, so the local view every link
    // serves never offers them to another link.
    let subscriber = config
        .consume
        .then(|| shared.table.clone().with_link(Link::Upstream));
    // The relay's sessions share one authorization across redials. moq-tokio
    // keeps its own, which the connection hands out.
    let (uplink, auth) = match iroh_relay {
        Some(_) => (Uplink::Iroh(CancellationToken::new()), auth::Handle::new()),
        None => {
            let mut client = moq_tokio::client::Config::default()
                .init()
                .map_err(|err| {
                    e!(Error::Relay {
                        source: AnyError::from_std(err)
                    })
                })?
                .with_publisher(served(&shared.table))
                .with_peer_hop(hop)
                .with_cost(config.cost);
            if let Some(subscriber) = subscriber.clone() {
                client = client.with_subscriber(subscriber);
            }
            let connection = client.connect(config.dial_url());
            let auth = connection.auth();
            (Uplink::Tokio(Box::new(connection)), auth)
        }
    };
    let link_state = LinkState::default();
    let link = {
        let mut state = shared.state.lock().expect("poisoned");
        if state.closed {
            if let Uplink::Tokio(connection) = &uplink {
                connection.abort(moq_net::Error::Cancel);
            }
            return Err(e!(Error::ShutDown));
        }
        let link = state.next_id();
        // Authorizes the link before its first session is up, which takes
        // at least a round trip.
        state.add_link(
            link,
            LinkEntry {
                kind: LinkKind::Relay,
                remote: iroh_relay,
                hop,
                grant: Grant::everything(),
                public: matches!(config.offer, RelayOffer::Public),
                consume: config.consume,
                auth: auth.clone(),
                session: None,
                link_state: link_state.clone(),
            },
        );
        link
    };
    let url = config.url.clone();
    info!(%url, cost = config.cost, consume = config.consume, "attaching relay");
    let status = Watchable::new(RelayStatus::Connecting);
    let task = match (&uplink, iroh_relay) {
        (Uplink::Iroh(stop), Some(relay)) => {
            let run = iroh_uplink(
                shared.clone(),
                relay,
                config.clone(),
                auth,
                subscriber,
                link,
                link_state.clone(),
                status.clone(),
                stop.clone(),
            );
            tokio::spawn(run.instrument(info_span!("relay", %url, link)))
        }
        (Uplink::Tokio(connection), _) => {
            let run = tokio_uplink(
                shared.clone(),
                moq_tokio::Connection::clone(connection),
                link,
                link_state.clone(),
                status.clone(),
            );
            tokio::spawn(run.instrument(info_span!("relay", %url, link)))
        }
        (Uplink::Iroh(_), None) => unreachable!("an iroh uplink names its relay"),
    };
    moq.tasks.insert_relay(
        link,
        RelayTask {
            task: AbortOnDropHandle::new(task),
            status: status.clone(),
        },
    );
    // A shutdown that ran between the check above and the insert found no
    // task to stop, so stop it here instead.
    if shared.shutdown.is_cancelled() {
        moq.tasks.detach_relays();
        return Err(e!(Error::ShutDown));
    }
    Ok(RelayLink {
        inner: Arc::new(RelayInner {
            link,
            url,
            link_state,
            status,
            uplink,
            shared: Arc::downgrade(shared),
            tasks: Arc::downgrade(&moq.tasks),
        }),
    })
}

/// How a relay link reaches its relay.
enum Uplink {
    /// A moq-tokio client connection, which redials itself.
    Tokio(Box<moq_tokio::Connection>),
    /// An iroh dial loop, stopped by cancelling the token.
    Iroh(CancellationToken),
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
    link: u64,
    link_state: LinkState,
    status: Watchable<RelayStatus>,
) {
    let monitor = link::monitor_relay(connection.monitor(), link_state);
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
                warn!(%err, "relay link ended");
                break;
            }
        }
    }
    status.set(RelayStatus::Detached).ok();
    shared.state.lock().expect("poisoned").remove_link(link);
}

/// Runs an `iroh://` relay link until it is stopped.
///
/// Dials the relay over the node's endpoint, and redials with backoff whenever
/// the session ends.
#[allow(clippy::too_many_arguments, reason = "one call site, all owned")]
async fn iroh_uplink(
    shared: Arc<Shared>,
    relay: EndpointId,
    config: RelayConfig,
    auth: auth::Handle,
    subscriber: Option<origin::Producer>,
    link: u64,
    link_state: LinkState,
    status: Watchable<RelayStatus>,
    stop: CancellationToken,
) {
    let publish = served(&shared.table);
    let options = ConnectOptions {
        token: config.token.clone(),
        cost: Some(config.cost),
        grant: Some(Grant::everything()),
    };
    let mut pause = REDIAL_MIN;
    let sampler = link::SharedSampler::default();
    loop {
        let dialed = tokio::select! {
            _ = stop.cancelled() => break,
            _ = shared.shutdown.cancelled() => break,
            dialed = dial_relay(&shared, relay, &options, &publish, subscriber.as_ref(), &auth) => dialed,
        };
        match dialed {
            Ok((moq, driver, connection)) => {
                info!("relay connected");
                status.set(RelayStatus::Connected).ok();
                let started = tokio::time::Instant::now();
                // A new session is a new path, even if QUIC numbers it alike.
                sampler.lock().expect("poisoned").new_session();
                let _driver = AbortOnDropHandle::new(tokio::spawn(async move {
                    moq_net::time::run(driver).await;
                }));
                let monitor = AbortOnDropHandle::new(tokio::spawn(link::monitor_direct(
                    moq.clone(),
                    connection,
                    link_state.clone(),
                    sampler.clone(),
                )));
                let closing = tokio::select! {
                    _ = stop.cancelled() => true,
                    _ = shared.shutdown.cancelled() => true,
                    err = moq.closed() => {
                        warn!(%err, "relay session dropped, redialing");
                        false
                    }
                };
                // The monitor must not overwrite the cleared reading.
                drop(monitor);
                link_state.clear(sampler.lock().expect("poisoned").generation());
                if closing {
                    moq.close().await.ok();
                    break;
                }
                // Only a session that held up earns a fresh backoff, so a
                // relay that accepts and drops at once is not hammered.
                if started.elapsed() >= STABLE_SESSION {
                    pause = REDIAL_MIN;
                }
            }
            Err(err) => warn!(%err, "relay dial failed, redialing"),
        }
        status.set(RelayStatus::Reconnecting).ok();
        // Equal jitter, half the pause plus a random half, so relays that
        // dropped together do not redial together.
        let jittered = pause.mul_f64(rand_unit());
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = shared.shutdown.cancelled() => break,
            _ = tokio::time::sleep(jittered) => {}
        }
        pause = (pause * 2).min(REDIAL_MAX);
    }
    status.set(RelayStatus::Detached).ok();
    shared.state.lock().expect("poisoned").remove_link(link);
}

/// Dials an iroh relay and completes the MoQ handshake.
async fn dial_relay(
    shared: &Shared,
    relay: EndpointId,
    options: &ConnectOptions,
    publish: &origin::Consumer,
    subscriber: Option<&origin::Producer>,
    auth: &auth::Handle,
) -> Result<
    (
        moq_net::Session,
        moq_net::Driver,
        iroh::endpoint::Connection,
    ),
    Error,
> {
    let transport = transport::dial(&shared.endpoint, relay).await?;
    let connection = transport.conn().clone();
    let (moq, driver) = client_handshake(
        transport,
        options,
        relay,
        publish,
        subscriber.cloned(),
        auth.clone(),
    )
    .await
    .map_err(|source| e!(Error::Moq { source }))?;
    Ok((moq, driver, connection))
}

/// Returns a factor in `[0.5, 1)` from the clock, random enough to spread redials.
fn rand_unit() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    0.5 + f64::from(nanos % 1_000_000) / 2_000_000.0
}

/// Aborts a moq-tokio connection when dropped.
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
    fn dial_url_carries_the_token_as_jwt() {
        let mut config = RelayConfig::new("https://relay.example/room".parse().expect("url"));
        assert_eq!(config.dial_url().as_str(), "https://relay.example/room");
        config.token = Some("a+b".into());
        assert_eq!(config.dial_url().as_str(), "https://relay.example/room?jwt=a%2Bb");
    }

    #[test]
    fn iroh_config_names_the_endpoint_id() {
        let id = SecretKey::generate().public();
        let config = RelayConfig::iroh(id);
        assert_eq!(config.url.scheme(), "iroh");
        assert_eq!(config.url.host_str(), Some(id.to_string().as_str()));
        assert_eq!(config.cost, DEFAULT_RELAY_COST);
    }
}
