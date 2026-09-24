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
    time::Duration,
};

use iroh::EndpointId;
use moq_net::{
    Hop, Hops, Path, PathOwned, announce, broadcast,
    origin::{self, Route},
};
use n0_error::e;
use n0_future::task::{AbortOnDropHandle, JoinSet};
use tracing::{debug, info, trace, warn};

use crate::{Error, Moq, Reach, ServingLink, Session, node::Shared, publish::check_path};

/// A broadcast that lived shorter than this before it ended counts as ending
/// at once, for [`Subscription::closed`]'s pause.
const REASK_WINDOW: Duration = Duration::from_secs(1);

/// The pause before asking the table again, per broadcast in a row that ended
/// at once.
const REASK_PAUSE: Duration = Duration::from_millis(100);

/// The pause stops growing after this many steps, at two seconds.
const REASK_MAX_STEPS: u32 = 20;

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
/// serving route changes within the same first hop, which routes through
/// different relays to one source share and a direct and a relay route never
/// do. A subscriber that sees the broadcast end can ask the table again
/// through [`as_origin`], or wait on [`closed`](Self::closed). Cheap to clone;
/// dropping it releases nothing the route table needs.
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

    /// Returns the link serving the broadcast now, and its latest reading.
    ///
    /// A direct session's or a relay link's, whichever served the last request
    /// for the path; a subscription resolved through one session is served by
    /// that session only. `None` while no request has been served and once the
    /// serving link is gone. What the media crate adapts a player on.
    pub fn link(&self) -> Option<ServingLink> {
        let shared = self.inner.shared.upgrade()?;
        let state = shared.state.lock().expect("poisoned");
        let link = match self.inner.link {
            Some(link) => link,
            None => state.served(&self.inner.path)?,
        };
        let entry = state.links.get(&link)?;
        Some(ServingLink {
            id: LinkId(link),
            kind: entry.kind,
            sample: entry.link_state.get(),
        })
    }

    /// Waits until the path has no route left.
    ///
    /// A change of route that moq cannot splice ends the broadcast; this asks
    /// the route table again then, and resolves only once nothing serves the
    /// path any more. A route that keeps answering with broadcasts that end at
    /// once is asked again with a growing pause rather than in a tight loop.
    /// Cancellation safe.
    pub async fn closed(&self) {
        let mut quick = 0u32;
        loop {
            let current = self.as_moq();
            let since = tokio::time::Instant::now();
            current.closed().await;
            if since.elapsed() >= REASK_WINDOW {
                quick = 0;
            }
            if quick > 0 {
                tokio::time::sleep(REASK_PAUSE * quick.min(REASK_MAX_STEPS)).await;
            }
            quick += 1;
            let next = match self.inner.origin.request_broadcast(&self.inner.path).await {
                Ok(next) if !next.is_closed() => next,
                _ => return,
            };
            debug!(path = %self.inner.path, "broadcast ended, a route still serves it");
            *self.inner.current.lock().expect("poisoned") = next;
        }
    }
}

/// Resolves `path` in the route table, for [`Moq::subscribe`](crate::Moq::subscribe).
pub(crate) async fn subscribe(
    moq: &Moq,
    path: PathOwned,
    reach: Reach,
) -> Result<Subscription, Error> {
    check_path(&path)?;
    if moq.shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let table = moq.shared.table.consume();
    // A route the table already knows answers at once.
    if let Ok(broadcast) = table.request_broadcast(&path).await {
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
        let resolved = table.routed_broadcast(&path).await;
        return outcome(&moq.shared, path, table, resolved);
    };

    debug!(%path, peer = %peer.fmt_short(), relays, "dialing the publisher");
    let routed = {
        let (table, path) = (table.clone(), path.clone());
        async move { table.routed_broadcast(&path).await }
    };
    tokio::pin!(routed);
    let session = tokio::select! {
        resolved = &mut routed => return outcome(&moq.shared, path, table, resolved),
        connected = moq.connect(peer) => match connected {
            Ok(session) => session,
            Err(err) if relays => {
                info!(%path, %err, "publisher unreachable, waiting for a relay");
                let resolved = routed.await;
                return outcome(&moq.shared, path, table, resolved);
            }
            Err(err) => return Err(err),
        },
    };

    // The session ending is the end of the wait unless a relay can still
    // bring the path.
    tokio::select! {
        resolved = &mut routed => return outcome(&moq.shared, path, table, resolved),
        _ = session.closed() => {}
    }
    if !relays {
        return Err(e!(Error::NotAnnounced { path }));
    }
    let resolved = routed.await;
    outcome(&moq.shared, path, table, resolved)
}

/// Returns the subscription `resolved` makes, or why there is none.
fn outcome(
    shared: &Arc<Shared>,
    path: PathOwned,
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
    path: PathOwned,
    table: origin::Consumer,
    broadcast: broadcast::Consumer,
) -> Subscription {
    Subscription::new(path, table, broadcast, None, Arc::downgrade(shared))
}

/// Mirrors the routes `ingest` holds into the node's route table.
///
/// Runs until the link ends. Each route is re-announced in the table as a
/// dynamic route with the same hop chain and cost, and a request for a path
/// under it is resolved through `ingest`, which is the link's own view, and
/// spliced. Recording which link served each request is what
/// [`Subscription::session`] and [`Moq::routes`](crate::Moq::routes) read.
///
/// A direct session's ingest holds only what its grant lets the peer publish,
/// so the grant is what keeps a peer from routing someone else's path.
///
/// A relay's routes (`relay` set) enter the table under a first hop of their
/// own (see [`relayed`]), so the table never takes a relay route for the same
/// source as a direct one. moq re-splices a broadcast only between routes that share a
/// first hop, and a first hop is only what a publisher declares: without this, a
/// peer that publishes someone else's path into a relay under that publisher's
/// hop would be spliced into a subscription the moment its direct session
/// dropped. A subscription that loses its direct route therefore ends, and
/// asking again resolves through the relay.
pub(crate) async fn bridge(shared: Arc<Shared>, link: u64, ingest: origin::Producer, relay: bool) {
    let mut announced = ingest.consume().announced();
    let mut mirrors: HashMap<PathOwned, (Arc<origin::Dynamic>, AbortOnDropHandle<()>)> =
        HashMap::new();
    while let Some(update) = announced.next().await {
        let prefix = update.prefix.clone();
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
        let hops = if relay {
            match relayed(&update.route.hops) {
                Some(hops) => hops,
                None => {
                    warn!(link, %prefix, "relay route with an unusable hop chain, not mirrored");
                    continue;
                }
            }
        } else {
            update.route.hops.clone()
        };
        let route = Route::default()
            .with_hops(hops)
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

/// Returns `hops` with its first hop, the source it claims, replaced by a hop
/// that stands for that source as reached through a relay.
///
/// Derived from the claimed hop alone, so every relay's route to one source
/// shares it and a subscription can still move between relays, while no relay
/// route shares a first hop with a direct one. An anonymous chain is left as it
/// is: moq never re-splices one. `None` if the new chain would repeat a hop,
/// which a real chain cannot.
fn relayed(hops: &Hops) -> Option<Hops> {
    let mut chain = hops.iter();
    let Some(first) = chain.next() else {
        return Some(hops.clone());
    };
    if *first == Hop::UNKNOWN {
        return Some(hops.clone());
    }
    // SplitMix64's finalizer, so nearby ids do not map to nearby ids, then
    // masked below 2^53 as every hop this crate derives is.
    let mut value = first.id() ^ RELAYED_SALT;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    let relayed = Hop::new((value & ((1u64 << 53) - 1)).max(1)).ok()?;
    let mut out = Hops::new();
    out.push(relayed).ok()?;
    for hop in chain {
        out.push(*hop).ok()?;
    }
    Some(out)
}

/// Mixed into a claimed hop for [`relayed`], so the relayed identity of a hop
/// is not a hop some node derives for itself.
const RELAYED_SALT: u64 = 0x7265_6c61_7965_6421;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(ids: &[u64]) -> Hops {
        let mut hops = Hops::new();
        for id in ids {
            hops.push(Hop::new(*id).expect("a valid hop"))
                .expect("distinct");
        }
        hops
    }

    fn ids(hops: &Hops) -> Vec<u64> {
        hops.iter().map(|hop| hop.id()).collect()
    }

    /// A relayed chain keeps its length and the hops after the first, and
    /// its first hop depends on the claimed source alone.
    #[test]
    fn a_relayed_chain_names_its_source_apart() {
        let direct = chain(&[7, 11]);
        let relayed_once = relayed(&direct).expect("relayed");
        let relayed_elsewhere = relayed(&chain(&[7, 13])).expect("relayed");
        assert_eq!(relayed_once.len(), 2);
        assert_ne!(
            ids(&relayed_once)[0],
            7,
            "a relay route shares the direct source"
        );
        assert_eq!(ids(&relayed_once)[1], 11);
        assert_eq!(
            ids(&relayed_once)[0],
            ids(&relayed_elsewhere)[0],
            "two relays' routes to one source differ"
        );
        assert_ne!(
            ids(&relayed_once)[0],
            ids(&relayed(&chain(&[8, 11])).expect("relayed"))[0],
            "two sources collapse into one"
        );
        assert!(ids(&relayed_once)[0] < 1 << 53);
    }
}
