//! Relay links: staying attached to a moq relay.
//!
//! A relay link is a moq-tokio client connection. It publishes what the node
//! offers the relay, and feeds what the relay announces into the node's route
//! table. moq-tokio redials with backoff. `iroh://` relays are dialed through
//! the node's own endpoint.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use iroh::EndpointId;
use moq_net::origin;
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watchable;
use tracing::{Instrument, debug, info, info_span, warn};
use url::Url;

use crate::{
    Error, Grant, LinkId, LinkKind, LinkSample, Moq,
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
    connection: moq_tokio::Connection,
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
        self.inner.connection.abort(moq_net::Error::Cancel);
        // An error only says the session was already gone.
        self.inner.connection.closed().await.ok();
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
    let origins = Origins::new(shared);
    let mut client = moq_tokio::client::Config::default()
        .init()
        .map_err(|err| {
            e!(Error::Relay {
                source: AnyError::from_std(err)
            })
        })?
        .with_iroh(shared.endpoint.clone())
        .with_publisher(origins.publish.consume())
        .with_cost(config.cost);
    if config.consume {
        client = client.with_subscriber(origins.ingest.clone());
    }

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
                remote: config
                    .url
                    .host_str()
                    .filter(|_| config.url.scheme() == "iroh")
                    .and_then(|host| host.parse().ok()),
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
    let connection = client.connect(config.dial_url());
    let status = Watchable::new(RelayStatus::Connecting);
    let task = {
        let shared = shared.clone();
        let monitor = link::monitor(
            link::Source::Relay(connection.monitor()),
            link_state.clone(),
        );
        let mut watch = connection.clone();
        // Aborts the connection for every clone when the task ends, so a
        // `RelayLink` handle held past shutdown does not keep redialing.
        let _stop = StopOnDrop(connection.clone());
        let status = status.clone();
        tokio::spawn(
            async move {
                let _stop = _stop;
                let (ingest, _drivers) = origins.run();
                let _monitor = AbortOnDropHandle::new(tokio::spawn(monitor));
                let _bridge = config.consume.then(|| {
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
            .instrument(info_span!("relay", %url, link)),
        )
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
            connection,
            shared: Arc::downgrade(shared),
            tasks: Arc::downgrade(&moq.tasks),
        }),
    })
}

/// Aborts a moq-tokio connection when dropped.
struct StopOnDrop(moq_tokio::Connection);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.abort(moq_net::Error::Cancel);
    }
}
