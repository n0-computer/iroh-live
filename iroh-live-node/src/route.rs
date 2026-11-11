//! The route table, and the subscriptions that resolve paths in it.
//!
//! Every link announces straight into the node's route table: a session's
//! subscriber is the table tagged [`Link::Peer`], gated by the session's
//! authorization, which lets the peer publish only what its grant allows, and a relay's is the table tagged [`Link::Upstream`].
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
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use moq_net::{
    Instance, Path, Pattern, Patterns, broadcast,
    origin::{self, Link, RouteEvent, Source},
};
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{
    BroadcastTicket, Error, Node, PathClaim, Reach, ServingLink, Session,
    handshake::Hop,
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
/// link and a relay, without ending it. A task the subscription owns follows
/// the path: when another instance takes it over, or the broadcast ends while
/// a route still serves the path, [`as_moq`](Self::as_moq) switches to what
/// serves it now. Cheap to clone.
#[derive(Clone, derive_more::Debug)]
#[debug("Subscription({}, pinned {})", inner.path, inner.pin.is_some())]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

struct SubscriptionInner {
    path: Path,
    origin: origin::Consumer,
    /// The broadcast [`Subscription::as_moq`] returns, which the follow task updates.
    current: Arc<Mutex<broadcast::Consumer>>,
    /// Cancelled once nothing serves the path.
    ended: CancellationToken,
    /// Follows the path for as long as the subscription lives.
    _follow: AbortOnDropHandle<()>,
    /// The link the subscription reads from, if it does not follow the whole table.
    pin: Option<LinkId>,
    /// The route serving the path, as the table announces it.
    routes: Mutex<Routes>,
    shared: Weak<Shared>,
    /// The ticket claim that lets the publisher announce the path, while held.
    claim: OnceLock<PathClaim>,
}

/// Follows the route that serves one path.
struct Routes {
    announced: Option<origin::Routes>,
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
            .map(|scoped| scoped.routes());
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
                RouteEvent::End(_) => None,
                RouteEvent::Update(announce)
                | RouteEvent::Start(announce)
                | RouteEvent::Restart(announce)
                | RouteEvent::Takeover { announce, .. } => Some(announce.route.source()),
            };
        }
        self.source
    }
}

impl Subscription {
    /// Creates a subscription that reads `broadcast`, resolved at `path` in `origin`.
    ///
    /// `pin` is the link, and the instance it announced, that the subscription
    /// is limited to. `None` follows the whole table. Spawns the task that
    /// follows the path, so it must run inside a tokio runtime.
    pub(crate) fn new(
        path: Path,
        origin: origin::Consumer,
        broadcast: broadcast::Consumer,
        pin: Option<(LinkId, Instance)>,
        shared: Weak<Shared>,
    ) -> Self {
        let routes = Routes::new(&origin, &path);
        let current = Arc::new(Mutex::new(broadcast));
        let ended = CancellationToken::new();
        let follow = tokio::spawn(follow(
            path.clone(),
            origin.clone(),
            pin.as_ref().map(|(_, instance)| instance.clone()),
            current.clone(),
            ended.clone(),
        ));
        Self {
            inner: Arc::new(SubscriptionInner {
                path,
                origin,
                current,
                ended,
                _follow: AbortOnDropHandle::new(follow),
                pin: pin.map(|(link, _)| link),
                routes: Mutex::new(routes),
                shared,
                claim: OnceLock::new(),
            }),
        }
    }

    /// Keeps `claim` alive for as long as the subscription lives.
    fn holding(self, claim: PathClaim) -> Self {
        self.inner.claim.set(claim).ok();
        self
    }

    /// Returns the path this subscription resolves.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Returns the route table the path was resolved in.
    pub fn as_origin(&self) -> origin::Consumer {
        self.inner.origin.clone()
    }

    /// Returns the broadcast that serves the path now.
    ///
    /// The subscription's own task re-resolves the path, so a later call may
    /// return a newer broadcast than an earlier one.
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
    /// serving link is gone. Also `None` when a relay serves the route and
    /// several relays feed the table, since a relay's route does not say
    /// which relay it came from.
    pub fn serving_link(&self) -> Option<ServingLink> {
        let shared = self.inner.shared.upgrade()?;
        let state = shared.state.lock().expect("poisoned");
        let (link, entry) = self.serving(&state)?;
        Some(ServingLink {
            id: link,
            kind: entry.kind,
            sample: entry.link_state.get(),
        })
    }

    /// Returns the link serving the broadcast now: the pinned one, or the best route's.
    fn serving<'a>(&self, state: &'a State) -> Option<(LinkId, &'a LinkEntry)> {
        let link = match self.inner.pin {
            Some(link) => link,
            None => {
                let source = self.inner.routes.lock().expect("poisoned").source()?;
                state.link_for(source)?
            }
        };
        Some((link, state.links.get(&link)?))
    }

    /// Waits until nothing serves the path.
    ///
    /// The subscription follows the path on its own, so awaiting this has no
    /// effect on what [`as_moq`](Self::as_moq) returns. A pinned subscription
    /// ends when another instance takes its path over.
    ///
    /// Cancellation safe: dropping the future loses nothing.
    pub async fn closed(&self) {
        self.inner.ended.cancelled().await;
    }
}

/// Follows `path` in `origin` and keeps `current` on what serves it, until nothing does.
///
/// moq moves between routes of one instance by itself. This switches when
/// another instance takes the path over, even while the old broadcast is still
/// served to its readers. It also asks the table again when the broadcast
/// ends, which happens when the instance's last route went while another
/// remains. With `instance` set, it follows only that instance. Cancels `ended`
/// when it returns.
async fn follow(
    path: Path,
    origin: origin::Consumer,
    instance: Option<Instance>,
    current: Arc<Mutex<broadcast::Consumer>>,
    ended: CancellationToken,
) {
    let _ended = ended.drop_guard();
    let mut following = origin.follow(&path, instance.clone()).ok();
    loop {
        let serving = current.lock().expect("poisoned").clone();
        let since = tokio::time::Instant::now();
        let taken = async {
            match following.as_mut() {
                Some(following) => following.available().await.ok().flatten(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(next) = taken => {
                if !next.is_closed() {
                    debug!(%path, "another instance serves the path");
                    *current.lock().expect("poisoned") = next;
                }
                continue;
            }
            _ = serving.closed() => {}
        }
        if since.elapsed() < REASK_WINDOW {
            tokio::time::sleep(REASK_PAUSE).await;
        }
        let next = match origin.request(&path, instance.clone()).await {
            Ok(next) if !next.is_closed() => next,
            _ => return,
        };
        debug!(%path, "broadcast ended, a route still serves it");
        *current.lock().expect("poisoned") = next;
    }
}

/// Resolves the broadcast `ticket` names, for [`Node::subscribe_ticket`].
pub(crate) async fn subscribe_ticket(
    node: &Node,
    ticket: &BroadcastTicket,
) -> Result<Subscription, Error> {
    let (peer, path) = (ticket.peer(), ticket.path());
    // A no-op for a path the node's grant policy already covers.
    let claim = node.claim_path(peer, path)?;
    let subscription = node.subscribe(path.clone(), Reach::Both(peer)).await?;
    Ok(subscription.holding(claim))
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
) -> Result<Instance, moq_net::Error> {
    let pattern = path
        .as_str()
        .parse::<Pattern>()
        .map_err(|_| moq_net::Error::Unauthorized)?;
    let mut announced = view.scope("", &Patterns::from(pattern))?.routes();
    while let Some(event) = announced.next().await {
        match event {
            RouteEvent::End(_) => {}
            RouteEvent::Update(announce)
            | RouteEvent::Start(announce)
            | RouteEvent::Restart(announce)
            | RouteEvent::Takeover { announce, .. } => {
                if Hop::of_route(announce.route.source()).is_some_and(&from) {
                    return Ok(announce.route.instance().clone());
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
    table
        .follow(path, None)?
        .available()
        .await?
        .ok_or(moq_net::Error::Closed)
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
