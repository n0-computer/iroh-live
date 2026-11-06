//! The route table, and the subscriptions that resolve paths in it.
//!
//! Every link announces straight into the node's route table: a session's
//! subscriber is the table tagged [`Link::Peer`], scoped to the paths its grant
//! lets the peer publish, and a relay's is the table tagged [`Link::Upstream`].
//! Every link serves the table's local view, which hides both, so no route
//! learned on one link is offered on another. The peer's hop and the link's
//! cost travel with each route. moq serves the best route, and moves a reader
//! between routes of one instance, such as a direct one and a relay's, without
//! ending its subscriptions. Which link a route came from is the route's
//! [`Source`].
//!
//! # Trust
//!
//! An instance is what a route's announcer claims, and moq splices readers
//! between routes that claim the same one. So a peer or relay that announces a
//! path under another's instance could take over its readers. What keeps that
//! out is the table's contents: a direct peer announces only paths its grant
//! lets it publish, so with grants that name the peer it cannot reach another
//! peer's paths. An instance is a random string that only reaches the links
//! whose grant lets them see the publication. A relay is trusted with the
//! paths it forwards, so attach only relays that keep each publisher to its
//! own paths, as moq-relay's tokens do. Hops take no part in this, since moq
//! splices by instance and not by the first hop a publisher claims.

use std::{
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use moq_net::{
    Hop, Instance, Path, Pattern, Patterns,
    announce::Event as AnnounceEvent,
    broadcast,
    origin::{self, Link, Source},
};
use n0_error::e;
use tracing::{debug, info};

use crate::{
    Error, Node, Reach, ServingLink, Session,
    node::Shared,
    publish::check_path,
    state::{LinkEntry, State},
};

/// A broadcast that lived shorter than this counts as ending at once.
const REASK_WINDOW: Duration = Duration::from_secs(1);

/// The pause before asking the table again after a quick end.
const REASK_PAUSE: Duration = Duration::from_millis(100);

/// Identifies one link of a node: a session or a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
#[display("link-{_0}")]
pub struct LinkId(pub(crate) u64);

/// What kind of link serves a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// A session with a peer, dialed or accepted.
    Direct,
    /// A moq relay this node is attached to.
    Relay,
}

/// A path resolved in a route table.
///
/// moq moves the broadcast between routes of one instance, such as a direct
/// link and a relay, without ending it. When the instance's last route goes,
/// the broadcast ends, and [`closed`](Self::closed) asks the table again.
/// Cheap to clone.
#[derive(Clone, derive_more::Debug)]
#[debug("Subscription({}, pinned {})", inner.path, inner.pin.is_some())]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

/// The link a subscription reads from, and the stream it announced.
struct Pin {
    link: u64,
    /// The instance the link announced, if it declared one.
    instance: Option<Instance>,
}

struct SubscriptionInner {
    path: Path,
    origin: origin::Consumer,
    /// The broadcast [`Subscription::as_moq`] returns.
    current: Mutex<broadcast::Consumer>,
    /// Set if the subscription does not follow the whole table.
    pin: Option<Pin>,
    /// The route serving the path, as the table announces it.
    routes: Mutex<Routes>,
    shared: Weak<Shared>,
}

/// Follows the route that serves one path.
struct Routes {
    announced: Option<moq_net::announce::Consumer>,
    source: Option<Source>,
}

impl Routes {
    /// Starts following the routes at exactly `path` in `table`.
    fn new(table: &origin::Consumer, path: &Path) -> Self {
        let announced = path
            .as_str()
            .parse::<Pattern>()
            .ok()
            .and_then(|pattern| table.scope("", &Patterns::from(pattern)).ok())
            .map(|scoped| scoped.announced());
        Self {
            announced,
            source: None,
        }
    }

    /// Returns where the route serving the path entered the node.
    fn source(&mut self) -> Option<Source> {
        let announced = self.announced.as_mut()?;
        while let Some(event) = announced.try_next() {
            self.source = match event {
                AnnounceEvent::End(_) => None,
                AnnounceEvent::Update(announce)
                | AnnounceEvent::Start(announce)
                | AnnounceEvent::Restart(announce)
                | AnnounceEvent::Takeover { announce, .. } => Some(announce.route.source()),
            };
        }
        self.source
    }
}

impl Subscription {
    /// Creates a subscription that reads `broadcast`, resolved at `path` in `origin`.
    ///
    /// `pin` is the link, and the instance it announced, that the subscription
    /// is limited to. `None` follows the whole table.
    pub(crate) fn new(
        path: Path,
        origin: origin::Consumer,
        broadcast: broadcast::Consumer,
        pin: Option<(u64, Option<Instance>)>,
        shared: Weak<Shared>,
    ) -> Self {
        let routes = Routes::new(&origin, &path);
        Self {
            inner: Arc::new(SubscriptionInner {
                path,
                origin,
                current: Mutex::new(broadcast),
                pin: pin.map(|(link, instance)| Pin { link, instance }),
                routes: Mutex::new(routes),
                shared,
            }),
        }
    }

    /// Returns the path this subscription resolves.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Returns the route table the path was resolved in.
    pub fn as_origin(&self) -> origin::Consumer {
        self.inner.origin.clone()
    }

    /// Returns the broadcast the path resolved to.
    ///
    /// Only [`closed`](Self::closed) re-resolves the path, so this stays the
    /// first broadcast unless something awaits `closed`.
    pub fn as_moq(&self) -> broadcast::Consumer {
        self.inner.current.lock().expect("poisoned").clone()
    }

    /// Returns the session serving the broadcast now, if a direct session does.
    ///
    /// `None` while no route is known, and when a relay serves it.
    pub fn session(&self) -> Option<Session> {
        let shared = self.inner.shared.upgrade()?;
        let state = shared.state.lock().expect("poisoned");
        let (_, entry) = self.serving(&state)?;
        entry.session.clone()
    }

    /// Returns the link serving the broadcast now, and its latest reading.
    ///
    /// Follows the best route as it moves between links. `None` before the
    /// table announces one, for a publication of this node, and once the
    /// serving link is gone.
    pub fn link(&self) -> Option<ServingLink> {
        let shared = self.inner.shared.upgrade()?;
        let state = shared.state.lock().expect("poisoned");
        let (link, entry) = self.serving(&state)?;
        Some(ServingLink {
            id: LinkId(link),
            kind: entry.kind,
            sample: entry.link_state.get(),
        })
    }

    /// Returns the link serving the broadcast now: the pinned one, or the best route's.
    fn serving<'a>(&self, state: &'a State) -> Option<(u64, &'a LinkEntry)> {
        let link = match &self.inner.pin {
            Some(pin) => pin.link,
            None => {
                let source = self.inner.routes.lock().expect("poisoned").source()?;
                state.link_for(source)?
            }
        };
        Some((link, state.links.get(&link)?))
    }

    /// Follows the path until nothing serves it.
    ///
    /// Switches [`as_moq`](Self::as_moq) to whatever serves the path now. moq
    /// moves between routes of one instance by itself. This switches when
    /// another instance takes the path over, even while the old broadcast is
    /// still served to its readers. It also asks the table again when the
    /// broadcast ends, which happens when the instance's last route went while
    /// another remains. A pinned subscription follows only its own instance.
    ///
    /// Cancellation safe: dropping the future loses nothing.
    pub async fn closed(&self) {
        let instance = self.inner.pin.as_ref().and_then(|pin| pin.instance.clone());
        let mut following = self
            .inner
            .origin
            .follow(&self.inner.path, instance.clone())
            .ok();
        loop {
            let current = self.as_moq();
            let since = tokio::time::Instant::now();
            let taken = async {
                match following.as_mut() {
                    Some(following) => following.available().await.ok(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(next) = taken => {
                    if !next.is_closed() {
                        debug!(path = %self.inner.path, "another instance serves the path");
                        *self.inner.current.lock().expect("poisoned") = next;
                    }
                    continue;
                }
                _ = current.closed() => {}
            }
            if since.elapsed() < REASK_WINDOW {
                tokio::time::sleep(REASK_PAUSE).await;
            }
            let next = match self
                .inner
                .origin
                .request(&self.inner.path, instance.clone())
                .await
            {
                Ok(next) if !next.is_closed() => next,
                _ => return,
            };
            debug!(path = %self.inner.path, "broadcast ended, a route still serves it");
            *self.inner.current.lock().expect("poisoned") = next;
        }
    }
}

/// Returns the route table as direct sessions fill it, without relays' routes.
pub(crate) fn direct_view(table: &origin::Producer) -> origin::Consumer {
    table.clone().with_link(Link::Upstream).consume()
}

/// Waits for a route to `path` that entered from a hop `from` accepts, and returns its instance.
pub(crate) async fn announced_by(
    view: &origin::Consumer,
    path: &Path,
    from: impl Fn(Hop) -> bool,
) -> Result<Option<Instance>, moq_net::Error> {
    let pattern = path
        .as_str()
        .parse::<Pattern>()
        .map_err(|_| moq_net::Error::Unauthorized)?;
    let mut announced = view.scope("", &Patterns::from(pattern))?.announced();
    while let Some(event) = announced.next().await {
        match event {
            AnnounceEvent::End(_) => {}
            AnnounceEvent::Update(announce)
            | AnnounceEvent::Start(announce)
            | AnnounceEvent::Restart(announce)
            | AnnounceEvent::Takeover { announce, .. } => {
                if matches!(announce.route.source(), Source::Peer(hop) if from(hop)) {
                    return Ok(announce.route.instance);
                }
            }
        }
    }
    Err(moq_net::Error::Closed)
}

/// Resolves `path` in the route table, for [`Node::subscribe`](crate::Node::subscribe).
pub(crate) async fn subscribe(moq: &Node, path: Path, reach: Reach) -> Result<Subscription, Error> {
    check_path(&path)?;
    if moq.shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let table = moq.shared.table.consume();
    // A route the table already knows answers at once.
    if let Ok(broadcast) = table.request(&path, None).await {
        debug!(%path, "resolved through an existing route");
        return Ok(subscription(&moq.shared, path, table, broadcast));
    }

    let (dial, relays) = match reach {
        Reach::Direct(peer) => (Some(peer), false),
        Reach::Relays => (None, true),
        Reach::Both(peer) => (Some(peer), true),
    };
    let relays = relays && moq.shared.state.lock().expect("poisoned").has_relays();
    // This node's own publications are in the table already.
    let Some(peer) = dial.filter(|peer| *peer != moq.shared.id) else {
        if !relays {
            return Err(e!(Error::NoRoute { path }));
        }
        debug!(%path, "waiting for a relay to route the path");
        let resolved = routed(&table, &path).await;
        return outcome(&moq.shared, path, table, resolved);
    };

    debug!(%path, peer = %peer.fmt_short(), relays, "dialing the publisher");
    let routed = {
        let (table, path) = (table.clone(), path.clone());
        async move { routed(&table, &path).await }
    };
    tokio::pin!(routed);
    let connected = tokio::select! {
        resolved = &mut routed => return outcome(&moq.shared, path, table, resolved),
        connected = moq.connect(peer) => connected,
    };
    match connected {
        // The peer's announcement of the path would never enter the table.
        Ok(session) if !session.grant().allows_publish(path.as_str()) => {
            if !relays {
                return Err(Error::not_granted(path, peer, &session.grant()));
            }
            info!(%path, "the publisher may not publish the path here, waiting for a relay");
        }
        Ok(session) => {
            // The session ending is the end of the wait unless a relay can
            // still bring the path.
            tokio::select! {
                resolved = &mut routed => return outcome(&moq.shared, path, table, resolved),
                _ = session.closed() => {}
            }
            if !relays {
                return Err(e!(Error::NotAnnounced { path }));
            }
        }
        Err(err) if relays => info!(%path, %err, "publisher unreachable, waiting for a relay"),
        Err(err) => return Err(err),
    }
    let resolved = routed.await;
    outcome(&moq.shared, path, table, resolved)
}

/// Waits until a route serves `path`, and resolves it.
pub(crate) async fn routed(
    table: &origin::Consumer,
    path: &Path,
) -> Result<broadcast::Consumer, moq_net::Error> {
    table.follow(path, None)?.available().await
}

/// Returns the subscription `resolved` makes, or why there is none.
fn outcome(
    shared: &Arc<Shared>,
    path: Path,
    table: origin::Consumer,
    resolved: Result<broadcast::Consumer, moq_net::Error>,
) -> Result<Subscription, Error> {
    match resolved {
        Ok(broadcast) => Ok(subscription(shared, path, table, broadcast)),
        Err(moq_net::Error::Closed) => Err(e!(Error::ShutDown)),
        Err(source) => Err(e!(Error::Unresolved { path, source })),
    }
}

/// Returns a subscription to `path` that follows the route table.
fn subscription(
    shared: &Arc<Shared>,
    path: Path,
    table: origin::Consumer,
    broadcast: broadcast::Consumer,
) -> Subscription {
    Subscription::new(path, table, broadcast, None, Arc::downgrade(shared))
}
