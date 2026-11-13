//! The route table, and the subscriptions that resolve paths in it.
//!
//! Every link announces straight into the node's route table. A session's
//! subscriber is the table tagged [`Link::Peer`], gated by the session's
//! authorization. A relay's subscriber is the table tagged [`Link::Upstream`].
//! Every link serves the table's local view, which hides both, so no route
//! learned on one link is offered on another. Which link a route came from is
//! the route's [`Source`].

use std::{
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use moq_net::{
    Instance, Path, Pattern, Patterns, broadcast,
    origin::{self, Link, RouteEvent, RouteKind, Source},
};
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{
    BroadcastTicket, Error, LinkId, Node, PathClaim, Reach, ServingLink, Session,
    handshake::Hop,
    json,
    node::Shared,
    publish::check_path,
    state::{LinkEntry, State},
};

/// The pause before asking the table again once a broadcast ended.
///
/// A successor, such as a second session with the publisher that may
/// announce only once the first ends, needs a moment to enter the table.
const REASK_PAUSE: Duration = Duration::from_millis(100);

/// A path resolved in a route table.
///
/// moq moves the broadcast between routes of one instance, such as a direct
/// link and a relay, without ending it. When another instance takes the path
/// over, [`broadcast`](Self::broadcast) switches to what serves it now. Cheap
/// to clone.
///
/// # Examples
///
/// ```no_run
/// # async fn run(node: iroh_live_node::Node, peer: iroh::EndpointId)
/// # -> Result<(), iroh_live_node::Error> {
/// use iroh_live_node::Reach;
///
/// let subscription = node.subscribe("live/camera", Reach::Both(peer)).await?;
/// let broadcast = subscription.broadcast();
/// # drop(broadcast);
/// // Resolves once no route serves the path any more.
/// subscription.closed().await;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, derive_more::Debug)]
#[debug("Subscription({}, pinned {})", inner.path, inner.pin.is_some())]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

/// What every clone of a [`Subscription`] shares.
struct SubscriptionInner {
    path: Path,
    origin: origin::Consumer,
    /// The broadcast [`Subscription::broadcast`] returns, which the follow task updates.
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

/// The route that serves one path, as the route table announces it.
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
            self.source = live_route(&event).map(|announce| announce.route.source());
        }
        self.source
    }
}

impl Subscription {
    /// Creates a subscription that reads `broadcast`, resolved at `path` in `origin`.
    ///
    /// `pin` is the link, and the instance it announced, that the subscription
    /// is limited to. Must run inside a tokio runtime.
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
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Returns the route table the path was resolved in.
    #[must_use]
    pub fn origin(&self) -> origin::Consumer {
        self.inner.origin.clone()
    }

    /// Returns the broadcast that serves the path now.
    ///
    /// A later call may return a newer broadcast than an earlier one.
    #[must_use]
    pub fn broadcast(&self) -> broadcast::Consumer {
        self.inner.current.lock().expect("poisoned").clone()
    }

    /// Returns a reader of the latest-value JSON track `name` of the broadcast.
    ///
    /// The publisher writes it with [`Node::publish_value`] or a
    /// [`json::snapshot::Producer`] of its own. The reader skips to the newest
    /// value with [`latest`](json::snapshot::Consumer::latest), and a late
    /// reader gets the current value at once. For a reader with other limits,
    /// pass [`broadcast`](Self::broadcast)`().track(name)` to
    /// [`json::snapshot::Consumer::new`] with a config of its own.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Moq`] when the broadcast has ended, or when a local
    /// broadcast has no track `name`. A remote publisher answers for its
    /// tracks itself, so there a missing track fails the first read instead.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run(subscription: iroh_live_node::Subscription)
    /// # -> Result<(), Box<dyn std::error::Error>> {
    /// let mut latency = subscription.value_track::<u64>("latency")?;
    /// while let Some(newest) = latency.latest().await? {
    ///     println!("latency {} ms", newest.value);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn value_track<T: DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<json::snapshot::Consumer<T>, Error> {
        let track = self
            .broadcast()
            .track(name)
            .map_err(|source| e!(Error::Moq { source }))?;
        debug!(path = %self.inner.path, track = name, "reading value track");
        Ok(json::snapshot::Consumer::new(track, Default::default()))
    }

    /// Returns the session serving the broadcast now, if a direct session does.
    #[must_use]
    pub fn session(&self) -> Option<Session> {
        let shared = self.inner.shared.upgrade()?;
        let (link, _) = self.serving(&shared.state.lock().expect("poisoned"))?;
        // Link ids are unique across sessions and relays, so a relay finds none.
        shared
            .sessions
            .get()
            .into_iter()
            .find(|session| session.inner.link == link)
    }

    /// Returns the link serving the broadcast now, and its latest reading.
    ///
    /// `None` before the table announces a route, for a publication of this
    /// node, and once the serving link is gone. Also `None` when a relay
    /// serves the route and several relays feed the table, since a relay's
    /// route does not say which relay it came from.
    #[must_use]
    pub fn serving_link(&self) -> Option<ServingLink> {
        let shared = self.inner.shared.upgrade()?;
        let state = shared.state.lock().expect("poisoned");
        let (link, entry) = self.serving(&state)?;
        Some(ServingLink {
            id: link,
            kind: entry.kind(),
            sample: entry.latest_sample.get(),
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
    /// A pinned subscription never moves to another instance, and ends once
    /// its own no longer serves the path. Cancellation safe.
    pub async fn closed(&self) {
        self.inner.ended.cancelled().await;
    }
}

/// Follows `path` in `origin` and keeps `current` on what serves it, until nothing does.
///
/// moq moves between routes of one instance by itself, and hands out each
/// instance that takes the path over. Once the broadcast ends, this asks the
/// table again after [`REASK_PAUSE`], and ends if no route serves the path.
/// With `instance` set, it follows only that instance.
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
        let taken = async {
            match following.as_mut() {
                Some(following) => following.available().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            taken = taken => match taken {
                Ok(Some(next)) if !next.is_closed() => {
                    debug!(%path, "another instance serves the path");
                    *current.lock().expect("poisoned") = next;
                    continue;
                }
                // The path went offline, or a takeover ended before it was
                // read. moq hands out what serves the path next.
                Ok(_) => continue,
                Err(moq_net::Error::Closed) => return,
                // A refusal: moq waits for the route to change.
                Err(_) => continue,
            },
            _ = serving.closed() => {}
        }
        tokio::time::sleep(REASK_PAUSE).await;
        match origin.request(&path, instance.clone()).await {
            Ok(next) if !next.is_closed() => {
                debug!(%path, "broadcast ended, a route still serves it");
                *current.lock().expect("poisoned") = next;
            }
            _ => return,
        }
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
        if let Some(announce) = live_route(&event)
            && Hop::of_route(announce.route.source()).is_some_and(&from)
        {
            return Ok(announce.route.instance().clone());
        }
    }
    Err(moq_net::Error::Closed)
}

/// Returns the route an event leaves covering the prefix, or `None` once no route does.
fn live_route(event: &RouteEvent) -> Option<&moq_net::announce::Announce> {
    match event.kind {
        RouteKind::End | RouteKind::Offline => None,
        RouteKind::Start | RouteKind::Update | RouteKind::Restart => Some(&event.announce),
    }
}

/// Resolves `path` in the route table, for [`Node::subscribe`](crate::Node::subscribe).
pub(crate) async fn subscribe(
    node: &Node,
    path: Path,
    reach: Reach,
) -> Result<Subscription, Error> {
    check_path(&path)?;
    if node.shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let table = node.shared.table.consume();
    // A route the table already knows answers at once.
    if let Ok(broadcast) = table.request(&path, None).await {
        debug!(%path, "resolved through an existing route");
        return Ok(subscription(&node.shared, path, table, broadcast));
    }

    let (dial, relays) = match reach {
        Reach::Direct(peer) => (Some(peer), false),
        Reach::Relays => (None, true),
        Reach::Both(peer) => (Some(peer), true),
    };
    let relays = relays && node.shared.state.lock().expect("poisoned").has_relays();
    // This node's own publications are in the table already.
    let Some(peer) = dial.filter(|peer| *peer != node.shared.id) else {
        if !relays {
            return Err(e!(Error::NoRoute { path }));
        }
        debug!(%path, "waiting for a relay to route the path");
        let resolved = routed(&table, &path).await;
        return outcome(&node.shared, path, table, resolved);
    };

    debug!(%path, peer = %peer.fmt_short(), relays, "dialing the publisher");
    let routed = {
        let (table, path) = (table.clone(), path.clone());
        async move { routed(&table, &path).await }
    };
    tokio::pin!(routed);
    let connected = tokio::select! {
        resolved = &mut routed => return outcome(&node.shared, path, table, resolved),
        connected = node.connect(peer) => connected,
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
                resolved = &mut routed => return outcome(&node.shared, path, table, resolved),
                _ = session.closed() => {}
            }
            if !relays {
                return Err(e!(Error::NotAnnounced { path }));
            }
        }
        Err(err) if relays => {
            info!(%path, error = %err, "publisher unreachable, waiting for a relay")
        }
        Err(err) => return Err(err),
    }
    let resolved = routed.await;
    outcome(&node.shared, path, table, resolved)
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

#[cfg(test)]
mod tests {
    use moq_net::origin::RouteOptions;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// A follow task over a local route table, and the broadcast it keeps current.
    struct Followed {
        current: Arc<Mutex<broadcast::Consumer>>,
        ended: CancellationToken,
        _task: AbortOnDropHandle<()>,
    }

    impl Followed {
        /// Resolves `path` in `table` and follows it, pinned to `instance` if given.
        async fn start(table: &origin::Producer, path: &str, instance: Option<Instance>) -> Self {
            let path = Path::new(path).expect("a path");
            let consumer = table.consume();
            let broadcast =
                tokio::time::timeout(TIMEOUT, consumer.request(&path, instance.clone()))
                    .await
                    .expect("resolved in time")
                    .expect("a route serves the path");
            let current = Arc::new(Mutex::new(broadcast));
            let ended = CancellationToken::new();
            let task = tokio::spawn(follow(
                path,
                consumer,
                instance,
                current.clone(),
                ended.clone(),
            ));
            Self {
                current,
                ended,
                _task: AbortOnDropHandle::new(task),
            }
        }

        /// Returns the instance of the broadcast the follow task keeps current.
        fn instance(&self) -> Option<Instance> {
            self.current
                .lock()
                .expect("poisoned")
                .info()
                .instance
                .clone()
        }

        /// Waits until the current broadcast is one of `instance`.
        async fn moves_to(&self, instance: &Instance) {
            tokio::time::timeout(TIMEOUT, async {
                while self.instance().as_ref() != Some(instance) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the subscription moved in time");
        }

        /// Waits until the follow task reports that nothing serves the path.
        async fn ends(&self) {
            tokio::time::timeout(TIMEOUT, self.ended.cancelled())
                .await
                .expect("the subscription ended in time");
        }
    }

    /// Publishes a broadcast at `path` with `cost`, until the route drops.
    fn route(table: &origin::Producer, path: &str, cost: u64) -> origin::Route {
        table
            .route(path, RouteOptions::default().with_cost(cost))
            .expect("a route")
    }

    #[tokio::test]
    async fn a_cheaper_instance_taking_over_moves_the_subscription() {
        let table = moq_tokio::origin::spawn();
        let first = route(&table, "cam", 5);
        let followed = Followed::start(&table, "cam", None).await;
        assert_eq!(followed.instance(), Some(first.instance()));

        let second = route(&table, "cam", 1);
        followed.moves_to(&second.instance()).await;
        assert!(!followed.ended.is_cancelled());
    }

    #[tokio::test]
    async fn an_ending_instance_hands_over_to_one_that_remains() {
        let table = moq_tokio::origin::spawn();
        let first = route(&table, "cam", 1);
        let second = route(&table, "cam", 5);
        let followed = Followed::start(&table, "cam", None).await;
        assert_eq!(followed.instance(), Some(first.instance()));

        drop(first);
        followed.moves_to(&second.instance()).await;
        assert!(!followed.ended.is_cancelled());
    }

    #[tokio::test]
    async fn a_successor_that_routes_just_after_the_end_takes_over() {
        let table = moq_tokio::origin::spawn();
        let first = route(&table, "cam", 1);
        let followed = Followed::start(&table, "cam", None).await;
        // Long enough that the end is not a quick one.
        tokio::time::sleep(Duration::from_millis(1100)).await;

        drop(first);
        tokio::time::sleep(REASK_PAUSE / 4).await;
        let second = route(&table, "cam", 1);
        followed.moves_to(&second.instance()).await;
        assert!(!followed.ended.is_cancelled());
    }

    #[tokio::test]
    async fn the_subscription_ends_once_nothing_serves_the_path() {
        let table = moq_tokio::origin::spawn();
        let only = route(&table, "cam", 1);
        let followed = Followed::start(&table, "cam", None).await;

        drop(only);
        followed.ends().await;
    }

    #[tokio::test]
    async fn a_pinned_subscription_never_moves_to_another_instance() {
        let table = moq_tokio::origin::spawn();
        let first = route(&table, "cam", 5);
        let followed = Followed::start(&table, "cam", Some(first.instance())).await;

        let _second = route(&table, "cam", 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(followed.instance(), Some(first.instance()));
        drop(first);
        followed.ends().await;
    }

    #[tokio::test]
    async fn a_pinned_subscription_stays_while_its_instance_serves() {
        let table = moq_tokio::origin::spawn();
        let first = route(&table, "cam", 1);
        let followed = Followed::start(&table, "cam", Some(first.instance())).await;

        let other = route(&table, "other", 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!followed.ended.is_cancelled());
        drop((first, other));
        followed.ends().await;
    }
}
