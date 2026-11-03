//! Relay links: staying attached to a moq relay.
//!
//! A relay link publishes what the node offers the relay, and feeds what the
//! relay announces into the node's route table. `https://`, `moqt://` and
//! `moql://` relays are moq-tokio client connections, which redial with
//! backoff. moq-tokio has no iroh transport, so `iroh://` relays are dialed
//! through the node's own endpoint by [`iroh_uplink`], which redials the same
//! way.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::Duration,
};

use iroh::EndpointId;
use moq_net::origin;
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};
use url::Url;

use crate::{
    ConnectOptions, Error, Grant, LinkId, LinkKind, LinkSample, Moq,
    link::{self, LinkState},
    node::{Shared, Tasks},
    route,
    session::Origins,
    state::LinkEntry,
};

/// The cost a relay link adds to every route through it, unless configured.
///
/// Higher than a direct link's cost of one, so a direct route wins while it
/// exists.
pub const DEFAULT_RELAY_COST: u64 = 10;

/// How to attach to a relay.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// `iroh://<endpoint-id>/<path>` or `https://host/<path>`.
    pub url: Url,
    /// Sent as `?jwt=` in the URL.
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
    /// Returns a config for the relay at `url`.
    ///
    /// It offers every public publication and consumes what the relay routes.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            token: None,
            cost: DEFAULT_RELAY_COST,
            offer: RelayOffer::default(),
            consume: true,
        }
    }

    /// Returns a config for the relay with endpoint id `relay`, dialed over iroh.
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
    /// The session dropped; redialing with backoff.
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
    ingest: origin::Consumer,
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

    /// Returns what the relay announces to this node.
    ///
    /// Empty for a link that does not consume.
    pub fn origin(&self) -> origin::Consumer {
        self.inner.ingest.clone()
    }

    /// Detaches from the relay.
    ///
    /// Closes the session, stops redialing and withdraws the relay's routes.
    /// Returns once the session has closed. Idempotent.
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
        if let Some(tasks) = self.inner.tasks.upgrade() {
            tasks.remove_relay(self.inner.link);
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

    /// Stops a relay link's task.
    fn remove_relay(&self, link: u64) {
        self.relays.lock().expect("poisoned").remove(&link);
    }

    /// Stops every relay link's task, reporting each detached.
    ///
    /// An aborted task never reaches its own report, so this makes it.
    pub(crate) fn detach_relays(&self) {
        let relays = std::mem::take(&mut *self.relays.lock().expect("poisoned"));
        for (_, relay) in relays {
            drop(relay.task);
            relay.status.set(RelayStatus::Detached).ok();
        }
    }
}

/// Attaches `moq` to a relay, for [`Moq::attach_relay`].
pub(crate) fn attach(moq: &Moq, config: RelayConfig) -> Result<RelayLink, Error> {
    let shared = &moq.shared;
    if shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    match config.url.scheme() {
        "iroh" | "https" | "http" | "moqt" | "moql" => {}
        scheme => {
            return Err(e!(Error::Relay {
                source: AnyError::from_string(format!(
                    "unsupported relay URL scheme {scheme:?}; use iroh:// or https://"
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
    let origins = Origins::new(shared);
    let link_state = LinkState::default();
    let link = {
        let mut state = shared.state.lock().expect("poisoned");
        if state.closed {
            return Err(e!(Error::ShutDown));
        }
        let link = state.next_id();
        state.add_link(
            link,
            LinkEntry {
                kind: LinkKind::Relay,
                remote: iroh_relay,
                grant: Grant::everything(),
                publish: origins.publish.clone(),
                public: matches!(config.offer, RelayOffer::Public),
                consume: config.consume,
                offers: HashMap::new(),
                session: None,
                link_state: link_state.clone(),
            },
        );
        link
    };
    let ingest = origins.ingest.consume();
    let url = config.url.clone();
    info!(%url, cost = config.cost, consume = config.consume, "attaching relay");
    let status = Watchable::new(RelayStatus::Connecting);
    let (uplink, task) = match iroh_relay {
        Some(relay) => {
            let stop = CancellationToken::new();
            let run = iroh_uplink(
                shared.clone(),
                relay,
                config.clone(),
                origins,
                link,
                link_state.clone(),
                status.clone(),
                stop.clone(),
            );
            (
                Uplink::Iroh(stop),
                tokio::spawn(run.instrument(info_span!("relay", %url, link))),
            )
        }
        None => {
            let mut client = moq_tokio::client::Config::default()
                .init()
                .map_err(|err| {
                    e!(Error::Relay {
                        source: AnyError::from_std(err)
                    })
                })?
                .with_publisher(origins.publish.consume())
                .with_cost(config.cost);
            if config.consume {
                client = client.with_subscriber(origins.ingest.clone());
            }
            let connection = client.connect(config.dial_url());
            let run = tokio_uplink(
                shared.clone(),
                connection.clone(),
                config.consume,
                origins,
                link,
                link_state.clone(),
                status.clone(),
            );
            (
                Uplink::Tokio(connection),
                tokio::spawn(run.instrument(info_span!("relay", %url, link))),
            )
        }
    };
    moq.tasks.insert_relay(
        link,
        RelayTask {
            task: AbortOnDropHandle::new(task),
            status: status.clone(),
        },
    );
    // A shutdown that ran between the check above and the insert found
    // no task to stop; stop it here instead.
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
            ingest,
            uplink,
            shared: Arc::downgrade(shared),
            tasks: Arc::downgrade(&moq.tasks),
        }),
    })
}

/// How a relay link reaches its relay.
enum Uplink {
    /// A moq-tokio client connection, which redials itself.
    Tokio(moq_tokio::Connection),
    /// An iroh dial loop, stopped by cancelling the token.
    Iroh(CancellationToken),
}

/// The first pause before redialing a relay.
const REDIAL_MIN: Duration = Duration::from_secs(1);

/// The longest pause between redials.
const REDIAL_MAX: Duration = Duration::from_secs(30);

/// Runs a moq-tokio relay link until its client gives up.
async fn tokio_uplink(
    shared: Arc<Shared>,
    connection: moq_tokio::Connection,
    consume: bool,
    origins: Origins,
    link: u64,
    link_state: LinkState,
    status: Watchable<RelayStatus>,
) {
    let monitor = link::monitor(link::Source::Relay(connection.monitor()), link_state);
    let mut watch = connection.clone();
    // Aborts the connection for every clone when the task ends, so a
    // `RelayLink` handle held past shutdown does not keep redialing.
    let _stop = StopOnDrop(connection);
    let (ingest, _drivers) = origins.run();
    let _monitor = AbortOnDropHandle::new(tokio::spawn(monitor));
    let _bridge = consume.then(|| {
        AbortOnDropHandle::new(tokio::spawn(
            route::bridge(shared.clone(), link, ingest, true).in_current_span(),
        ))
    });
    loop {
        match watch.status().await {
            Ok(moq_tokio::Status::Connected | moq_tokio::Status::Migrating) => {
                info!("relay connected");
                status.set(RelayStatus::Connected).ok();
            }
            Ok(moq_tokio::Status::Disconnected) => {
                warn!("relay session dropped, redialing");
                status.set(RelayStatus::Reconnecting).ok();
            }
            Ok(other) => debug!(?other, "relay status"),
            Err(err) => {
                warn!(%err, "relay link ended");
                break;
            }
        }
    }
    status.set(RelayStatus::Detached).ok();
    shared.state.lock().expect("poisoned").remove_link(link);
}

/// Runs an `iroh://` relay link: dials the relay over the node's endpoint,
/// and redials with backoff whenever the session ends.
#[allow(clippy::too_many_arguments, reason = "one call site, all owned")]
async fn iroh_uplink(
    shared: Arc<Shared>,
    relay: EndpointId,
    config: RelayConfig,
    origins: Origins,
    link: u64,
    link_state: LinkState,
    status: Watchable<RelayStatus>,
    stop: CancellationToken,
) {
    let publish = origins.publish.consume();
    let subscriber = config.consume.then(|| origins.ingest.clone());
    let (ingest, _drivers) = origins.run();
    let _bridge = config.consume.then(|| {
        AbortOnDropHandle::new(tokio::spawn(
            route::bridge(shared.clone(), link, ingest, true).in_current_span(),
        ))
    });
    let options = ConnectOptions {
        token: config.token.clone(),
        cost: Some(config.cost),
        grant: Some(Grant::everything()),
    };
    let mut pause = REDIAL_MIN;
    loop {
        let dialed = tokio::select! {
            _ = stop.cancelled() => break,
            _ = shared.shutdown.cancelled() => break,
            dialed = dial_relay(&shared, relay, &options, &publish, subscriber.as_ref()) => dialed,
        };
        match dialed {
            Ok((moq, driver, connection)) => {
                info!("relay connected");
                status.set(RelayStatus::Connected).ok();
                pause = REDIAL_MIN;
                let _driver = AbortOnDropHandle::new(tokio::spawn(async move {
                    moq_net::time::run(driver).await;
                }));
                let _monitor = AbortOnDropHandle::new(tokio::spawn(link::monitor(
                    link::Source::Direct(moq.clone(), connection),
                    link_state.clone(),
                )));
                tokio::select! {
                    _ = stop.cancelled() => {
                        moq.close().await.ok();
                        break;
                    }
                    _ = shared.shutdown.cancelled() => {
                        moq.close().await.ok();
                        break;
                    }
                    err = moq.closed() => warn!(%err, "relay session dropped, redialing"),
                }
            }
            Err(err) => warn!(%err, "relay dial failed, redialing"),
        }
        status.set(RelayStatus::Reconnecting).ok();
        // Full jitter, so relays that dropped together do not redial together.
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
) -> Result<
    (
        moq_net::Session,
        moq_net::Driver,
        iroh::endpoint::Connection,
    ),
    Error,
> {
    let dialed = crate::transport::dial_with(&shared.endpoint, relay.into(), options).await?;
    let connection = dialed.conn().clone();
    let mut client = moq_net::Client::new()
        .with_publisher(publish.clone())
        .with_peer_hop(crate::session::hop_for(&relay));
    if let Some(subscriber) = subscriber {
        client = client.with_subscriber(subscriber.clone());
    }
    if let Some(cost) = options.cost {
        client = client.with_cost(cost);
    }
    let address = if dialed.is_h3() {
        moq_net::transport::Address::Uri
    } else {
        if let Some(path) = options.setup_path()
            && let Ok(url) = Url::parse(&format!("moqt://iroh{path}"))
        {
            client = client.with_url(url);
        }
        moq_net::transport::Address::Setup
    };
    let (moq, driver) = client
        .connect(
            crate::session::driver_now(),
            crate::session::Transport::new(dialed.session, address),
        )
        .await
        .map_err(|source| e!(Error::Moq { source }))?;
    Ok((moq, driver, connection))
}

/// A uniform sample from `[0.5, 1)`, enough to spread redials.
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
