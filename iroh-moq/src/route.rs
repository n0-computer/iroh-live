//! The route table, and the subscriptions that resolve paths in it.
//!
//! Every link writes what its peer announces into an ingest origin of its own,
//! and a bridge mirrors each of those routes into the node's one route table,
//! with its hop chain and cost. moq picks the best route in the table (lowest
//! cost, then fewest hops) and fails over when it dies. Keeping the links'
//! routes apart as well as merged is what lets the node say which link serves a
//! path, and lets a direct session answer a path that means something on that
//! session only.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use iroh::EndpointId;
use moq_net::{
    Path, PathOwned, announce, broadcast,
    origin::{self, Route},
};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tracing::{debug, trace, warn};

use crate::{Session, node::Shared, path::publisher_of};

/// Identifies one link of a node: a session, a relay, or the node itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
#[display("link-{_0}")]
pub struct LinkId(pub(crate) u64);

impl LinkId {
    /// The node itself, for its own publications.
    pub(crate) const LOCAL: Self = Self(0);
}

/// What kind of link a route or session runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkKind {
    /// This node's own publication.
    Local,
    /// A session with a peer, dialed or accepted.
    Direct,
    /// A moq relay this node is attached to.
    Relay,
}

/// One route to a path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RouteInfo {
    /// The link the route arrived over.
    pub via: LinkId,
    /// What kind of link that is.
    pub kind: LinkKind,
    /// The peer at the other end of the link, for a direct session.
    pub remote: Option<EndpointId>,
    /// How many hops the route has crossed to reach this node.
    pub hops: usize,
    /// What the route costs, accumulated over every link it crossed.
    pub cost: u64,
    /// Whether this route is the one serving the path now.
    pub active: bool,
}

/// A path resolved in a route table.
///
/// Follows the best route to its path: moq re-splices the broadcast when the
/// serving route changes within the same first hop, and a subscriber that sees
/// the broadcast end can ask the table again through [`as_origin`]. Cheap to
/// clone; dropping it releases nothing the route table needs.
///
/// [`as_origin`]: Self::as_origin
#[derive(Clone)]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

struct SubscriptionInner {
    path: PathOwned,
    origin: origin::Consumer,
    current: Mutex<broadcast::Consumer>,
    /// The link this subscription is pinned to, if it does not follow the table.
    link: Option<u64>,
    shared: Weak<Shared>,
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("path", &self.inner.path)
            .field("link", &self.inner.link)
            .finish_non_exhaustive()
    }
}

impl Subscription {
    pub(crate) fn new(
        path: PathOwned,
        origin: origin::Consumer,
        broadcast: broadcast::Consumer,
        link: Option<u64>,
        shared: Weak<Shared>,
    ) -> Self {
        Self {
            inner: Arc::new(SubscriptionInner {
                path,
                origin,
                current: Mutex::new(broadcast),
                link,
                shared,
            }),
        }
    }

    /// Returns the path this subscription resolves.
    pub fn path(&self) -> &Path<'_> {
        &self.inner.path
    }

    /// Returns the route table the path was resolved in.
    ///
    /// For a reader that re-resolves the path when its broadcast ends.
    ///
    /// Integration point: follows moq-net's versioning.
    pub fn as_origin(&self) -> origin::Consumer {
        self.inner.origin.clone()
    }

    /// Returns the broadcast the path resolves to now.
    ///
    /// Integration point: follows moq-net's versioning.
    pub fn as_moq(&self) -> broadcast::Consumer {
        self.inner.current.lock().expect("poisoned").clone()
    }

    /// Returns the session serving the broadcast now, if a direct session does.
    ///
    /// `None` while no request has been served, and when a relay serves it.
    pub fn session(&self) -> Option<Session> {
        let shared = self.inner.shared.upgrade()?;
        let state = shared.state.lock().expect("poisoned");
        let link = match self.inner.link {
            Some(link) => link,
            None => state.served(&self.inner.path)?,
        };
        state.links.get(&link)?.session.clone()
    }

    /// Waits until the path has no route left.
    ///
    /// A change of route that moq cannot splice ends the broadcast; this asks
    /// the route table again then, and resolves only once nothing serves the
    /// path any more. Cancellation safe.
    pub async fn closed(&self) {
        loop {
            let current = self.as_moq();
            current.closed().await;
            let next = match self.inner.origin.request_broadcast(&self.inner.path).await {
                Ok(next) if !next.is_closed() => next,
                _ => return,
            };
            debug!(path = %self.inner.path, "broadcast ended, a route still serves it");
            *self.inner.current.lock().expect("poisoned") = next;
        }
    }
}

/// Mirrors the routes `ingest` holds into the node's route table.
///
/// Runs until the link ends. Each route is re-announced in the table as a
/// dynamic route with the same hop chain and cost, and a request for a path
/// under it is resolved through `ingest`, which is the link's own view, and
/// spliced. Recording which link served each request is what
/// [`Subscription::session`] and [`Moq::routes`](crate::Moq::routes) read.
///
/// With `publisher` set, only routes to that publisher's own broadcasts are
/// mirrored, meaning paths that [`publisher_of`] reads as `publisher`'s. That is
/// what a direct session gets: the table is shared by every subscriber on the
/// node and answers a ticket without dialing, so a peer must not be able to put
/// a route to someone else's path into it. Anything else the peer announces,
/// a bare name from the older layout or a call path, stays reachable over that
/// session alone through [`Session::subscribe`]. A relay link passes `None`,
/// since forwarding other publishers' broadcasts is what a relay is for.
pub(crate) async fn bridge(
    shared: Arc<Shared>,
    link: u64,
    ingest: origin::Producer,
    publisher: Option<EndpointId>,
) {
    let mut announced = ingest.consume().announced();
    let mut mirrors: HashMap<PathOwned, (Arc<origin::Dynamic>, AbortOnDropHandle<()>)> =
        HashMap::new();
    while let Some(update) = announced.next().await {
        let prefix = update.prefix.clone();
        if let Some(publisher) = publisher
            && publisher_of(&prefix) != Some(publisher)
        {
            trace!(link, %prefix, "route names another publisher, not mirrored");
            continue;
        }
        if update.kind == announce::Kind::Retracted {
            trace!(link, %prefix, "route retracted");
            mirrors.remove(&prefix);
            shared
                .state
                .lock()
                .expect("poisoned")
                .set_announced(link, prefix, None);
            continue;
        }
        let route = Route::default()
            .with_hops(update.route.hops.clone())
            .with_cost(update.route.cost);
        trace!(link, %prefix, hops = route.hops.len(), cost = route.cost.warm, "route");
        shared.state.lock().expect("poisoned").set_announced(
            link,
            prefix.clone(),
            Some((route.cost.warm, route.hops.len())),
        );
        if let Some((dynamic, _)) = mirrors.get(&prefix) {
            if let Err(err) = dynamic.update(route) {
                warn!(link, %prefix, %err, "could not update a mirrored route");
            }
            continue;
        }
        let dynamic = match shared.table.dynamic(&prefix, route) {
            Ok(dynamic) => Arc::new(dynamic),
            Err(err) => {
                warn!(link, %prefix, %err, "could not mirror a route into the table");
                continue;
            }
        };
        let task = tokio::spawn(answer(
            Arc::downgrade(&shared),
            link,
            dynamic.clone(),
            ingest.consume(),
        ));
        mirrors.insert(prefix, (dynamic, AbortOnDropHandle::new(task)));
    }
}

/// Answers the requests one mirrored route receives.
///
/// Each path resolves through the link's own ingest origin, in a task of its
/// own: a path the peer is slow to serve must not hold up the others under the
/// same prefix.
async fn answer(
    shared: Weak<Shared>,
    link: u64,
    dynamic: Arc<origin::Dynamic>,
    ingest: origin::Consumer,
) {
    let mut pending = JoinSet::new();
    loop {
        tokio::select! {
            request = dynamic.requested_broadcast() => {
                let Ok(request) = request else { break };
                let ingest = ingest.clone();
                let shared = shared.clone();
                pending.spawn(async move {
                    let path = request.path().to_owned();
                    match ingest.request_broadcast(&path).await {
                        Ok(broadcast) => {
                            if let Some(shared) = shared.upgrade() {
                                shared.state.lock().expect("poisoned").set_served(path, link);
                            }
                            request.accept(broadcast);
                        }
                        Err(err) => {
                            debug!(link, %path, %err, "link could not serve a request");
                            request.reject(err);
                        }
                    }
                });
            }
            Some(_) = pending.join_next(), if !pending.is_empty() => {}
        }
    }
}
