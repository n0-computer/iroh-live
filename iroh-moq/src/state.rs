//! The node's bookkeeping: publications, links, and who offers and serves what.
//!
//! Everything here changes under one lock and never awaits, so the public calls
//! that change it (`publish`, `set_audience`, `offer`, dropping an offer guard)
//! are synchronous, and a publication appears on a link in the same critical
//! section that decides it should.
//!
//! Every link has a publish origin of its own. A publication is offered on a
//! link by adding a dynamic route at its path to that origin, answered by
//! splicing the publication's broadcast (`Request::accept`) through a gate that
//! withdrawing the offer tears down (see [`serve`]), so the peer sees exactly
//! the publications meant for it, moq keeps announcing and serving them
//! natively, and a withdrawn offer ends what the peer was reading.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use iroh::EndpointId;
use moq_net::{Path, PathOwned, broadcast, origin};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    Grant, LinkId, LinkKind, RouteInfo, Session,
    publish::{Audience, AudienceKind},
};

/// A running offer: the task answering requests for one path on one origin.
pub(crate) type Serve = AbortOnDropHandle<()>;

/// The node's publications and links.
#[derive(Debug, Default)]
pub(crate) struct State {
    next_id: u64,
    pub(crate) publications: BTreeMap<u64, PubEntry>,
    pub(crate) links: BTreeMap<u64, LinkEntry>,
    /// Which link last served each path a subscriber asked for.
    served: HashMap<PathOwned, u64>,
    /// The route lists [`Moq::routes`](crate::Moq::routes) handed out, by path.
    watchers: HashMap<PathOwned, Watchable<Vec<RouteInfo>>>,
    /// Set once the node shuts down; nothing is added after that.
    pub(crate) closed: bool,
}

/// One publication.
#[derive(derive_more::Debug)]
pub(crate) struct PubEntry {
    pub(crate) path: PathOwned,
    /// The path nodes on the layout before publisher-named paths ask for.
    pub(crate) legacy: Option<PathOwned>,
    #[debug(skip)]
    pub(crate) broadcast: broadcast::Consumer,
    pub(crate) audience: AudienceKind,
    /// Explicit offers, per link.
    ///
    /// Counted, because several guards can offer one publication on one link.
    pub(crate) manual: HashMap<u64, usize>,
    /// The publication's route in the node's own route table.
    ///
    /// Only for an `Everyone` publication, so a local subscriber resolves it.
    pub(crate) local: Option<Serve>,
    /// Follows a `Peers` audience's set.
    pub(crate) peers_task: Option<AbortOnDropHandle<()>>,
    /// Withdraws the publication once its broadcast ends.
    pub(crate) _closed_task: Option<AbortOnDropHandle<()>>,
    /// Cancelled when the entry goes, however it goes, for
    /// [`Publication::withdrawn`](crate::Publication::withdrawn).
    pub(crate) withdrawn: CancellationToken,
}

impl Drop for PubEntry {
    fn drop(&mut self) {
        self.withdrawn.cancel();
    }
}

/// One link: a direct session or a relay.
#[derive(derive_more::Debug)]
pub(crate) struct LinkEntry {
    pub(crate) kind: LinkKind,
    pub(crate) remote: Option<EndpointId>,
    pub(crate) grant: Grant,
    /// What this link's peer is offered.
    #[debug(skip)]
    pub(crate) publish: origin::Producer,
    /// Whether publications are also offered at their pre-layout paths.
    pub(crate) legacy: bool,
    /// For a relay, whether `Everyone` publications go to it.
    pub(crate) public: bool,
    /// Whether the link feeds the route table.
    pub(crate) consume: bool,
    /// The running offers, by publication.
    #[debug(skip)]
    pub(crate) offers: HashMap<u64, Vec<Serve>>,
    /// What the peer announces, by prefix: cost and hop count.
    pub(crate) announced: BTreeMap<PathOwned, (u64, usize)>,
    /// The session, for a direct link.
    pub(crate) session: Option<Session>,
}

impl State {
    /// Allocates an id for a publication or a link.
    pub(crate) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Adds a link and offers it every publication it should see.
    pub(crate) fn add_link(&mut self, id: u64, link: LinkEntry) {
        self.links.insert(id, link);
        let publications: Vec<u64> = self.publications.keys().copied().collect();
        for publication in publications {
            self.reconcile(publication, id);
        }
    }

    /// Removes a link, withdrawing its offers and forgetting its routes.
    pub(crate) fn remove_link(&mut self, id: u64) {
        if self.links.remove(&id).is_none() {
            return;
        }
        for manual in self
            .publications
            .values_mut()
            .map(|publication| &mut publication.manual)
        {
            manual.remove(&id);
        }
        self.served.retain(|_, link| *link != id);
        self.notify();
    }

    /// Adds a publication and offers it on every link it should reach.
    pub(crate) fn add_publication(&mut self, id: u64, publication: PubEntry) {
        self.publications.insert(id, publication);
        self.reconcile_publication(id);
        self.notify();
    }

    /// Removes a publication from every link and from the route table.
    pub(crate) fn remove_publication(&mut self, id: u64) -> Option<PubEntry> {
        let entry = self.publications.remove(&id)?;
        for link in self.links.values_mut() {
            link.offers.remove(&id);
        }
        self.notify();
        Some(entry)
    }

    /// Returns the publication that already answers `path` or `legacy`, if any.
    ///
    /// At its path or at its alias: two publications answering one path would
    /// put two routes at it on every link, and a subscriber would get either.
    pub(crate) fn publication_answering(
        &self,
        path: &Path<'_>,
        legacy: Option<&PathOwned>,
    ) -> Option<u64> {
        let taken = |candidate: &PathOwned| {
            *candidate == *path || legacy.is_some_and(|legacy| candidate == legacy)
        };
        self.publications
            .iter()
            .find(|(_, publication)| {
                taken(&publication.path) || publication.legacy.as_ref().is_some_and(taken)
            })
            .map(|(id, _)| *id)
    }

    /// Brings every link's offer of publication `id` in line with its audience.
    pub(crate) fn reconcile_publication(&mut self, id: u64) {
        let links: Vec<u64> = self.links.keys().copied().collect();
        for link in links {
            self.reconcile(id, link);
        }
    }

    /// Offers or withdraws publication `publication` on link `link`.
    ///
    /// Whichever its audience, the link's grant and any explicit offer call
    /// for.
    pub(crate) fn reconcile(&mut self, publication: u64, link: u64) {
        let Some(entry) = self.links.get_mut(&link) else {
            return;
        };
        let Some(publication_entry) = self.publications.get(&publication) else {
            entry.offers.remove(&publication);
            return;
        };
        let wanted = visible(publication_entry, link, entry);
        let offered = entry.offers.contains_key(&publication);
        if wanted == offered {
            return;
        }
        if !wanted {
            debug!(path = %publication_entry.path, link, "withdrawing offer");
            entry.offers.remove(&publication);
            return;
        }
        debug!(path = %publication_entry.path, link, "offering");
        let mut serves: Vec<Serve> = serve(
            &entry.publish,
            &publication_entry.path,
            &publication_entry.broadcast,
        )
        .into_iter()
        .collect();
        if entry.legacy
            && let Some(legacy) = &publication_entry.legacy
        {
            serves.extend(serve(&entry.publish, legacy, &publication_entry.broadcast));
        }
        entry.offers.insert(publication, serves);
    }

    /// Records what link `link` announces at `prefix`, or that it withdrew it.
    pub(crate) fn set_announced(
        &mut self,
        link: u64,
        prefix: PathOwned,
        route: Option<(u64, usize)>,
    ) {
        let Some(entry) = self.links.get_mut(&link) else {
            return;
        };
        match route {
            Some(route) => entry.announced.insert(prefix, route),
            None => entry.announced.remove(&prefix),
        };
        self.notify();
    }

    /// Records that link `link` served a subscriber's request for `path`.
    pub(crate) fn set_served(&mut self, path: PathOwned, link: u64) {
        if self.served.insert(path, link) != Some(link) {
            self.notify();
        }
    }

    /// Returns the link that last served `path`, if it still exists.
    pub(crate) fn served(&self, path: &Path<'_>) -> Option<u64> {
        self.served
            .get(path)
            .copied()
            .filter(|link| self.links.contains_key(link))
    }

    /// Returns a watcher over every route to `path`.
    pub(crate) fn watch_routes(&mut self, path: PathOwned) -> n0_watcher::Direct<Vec<RouteInfo>> {
        let routes = self.routes(&path);
        self.watchers
            .entry(path)
            .or_insert_with(|| Watchable::new(routes))
            .watch()
    }

    /// Returns every route to `path`.
    ///
    /// This node's own publication, and each link that announces a prefix
    /// covering it.
    pub(crate) fn routes(&self, path: &Path<'_>) -> Vec<RouteInfo> {
        let served = self.served(path);
        let local = self.publications.values().any(|publication| {
            publication.path == *path && matches!(publication.audience, AudienceKind::Everyone)
        });
        let mut routes = Vec::new();
        if local {
            routes.push(RouteInfo {
                via: LinkId::LOCAL,
                kind: LinkKind::Local,
                remote: None,
                hops: 0,
                cost: 0,
                active: true,
            });
        }
        for (id, link) in &self.links {
            // The most specific prefix wins within one link, as it does in moq.
            let covering = link
                .announced
                .iter()
                .filter(|(prefix, _)| path.has_prefix(*prefix))
                .max_by_key(|(prefix, _)| prefix.len());
            if let Some((_, (cost, hops))) = covering {
                routes.push(RouteInfo {
                    via: LinkId(*id),
                    kind: link.kind,
                    remote: link.remote,
                    hops: *hops,
                    cost: *cost,
                    active: !local && served == Some(*id),
                });
            }
        }
        routes
    }

    /// Recomputes the watched route lists and forgets the unwatched ones.
    fn notify(&mut self) {
        self.watchers
            .retain(|_, watchable| watchable.has_watchers());
        let paths: Vec<PathOwned> = self.watchers.keys().cloned().collect();
        for path in paths {
            let routes = self.routes(&path);
            if let Some(watchable) = self.watchers.get(&path) {
                watchable.set(routes).ok();
            }
        }
    }
}

/// Reports whether `publication` belongs on link `id`.
///
/// Offered when the link's grant covers the path and either an explicit offer
/// names the link or the audience admits it. Relays see `Everyone`
/// publications only when their configuration offers public ones, and never a
/// `Peers` one, which a relay would forward to anyone.
fn visible(publication: &PubEntry, id: u64, link: &LinkEntry) -> bool {
    if !link.grant.allows_subscribe(publication.path.as_str()) {
        return false;
    }
    if publication.manual.get(&id).is_some_and(|count| *count > 0) {
        return true;
    }
    match (&publication.audience, link.kind) {
        (AudienceKind::Everyone, LinkKind::Direct) => true,
        (AudienceKind::Everyone, LinkKind::Relay) => link.public,
        (AudienceKind::Peers(peers), LinkKind::Direct) => {
            link.remote.is_some_and(|remote| peers.contains(&remote))
        }
        _ => false,
    }
}

/// Offers `broadcast` at `path` on `origin`, so that withdrawing the offer cuts
/// off whoever reads it.
///
/// The offer lasts until the broadcast ends or the returned handle drops.
/// Returns `None` if the origin refuses the route, which it does only for a
/// path no pattern can spell or once its driver is gone; either is logged.
///
/// Requests are not answered with `broadcast` itself. moq-net keeps serving a
/// path through a front for as long as the source it was handed lives, even
/// after the route retracts, and a new request for the path joins that front,
/// so a peer that already subscribed would read on, and could subscribe again,
/// after the offer was withdrawn. Each offer therefore answers through a gate:
/// an origin of its own that serves the broadcast, whose fronts' broadcasts are
/// what `origin`'s fronts splice. Dropping the handle tears the gate down,
/// which closes those broadcasts, which ends `origin`'s fronts and every
/// subscription through them.
pub(crate) fn serve(
    origin: &origin::Producer,
    path: &Path<'_>,
    broadcast: &broadcast::Consumer,
) -> Option<Serve> {
    let offered = match origin.dynamic(path, origin::Route::default()) {
        Ok(dynamic) => dynamic,
        Err(err) => {
            warn!(%path, %err, "could not offer the broadcast");
            return None;
        }
    };
    let (gate, gate_driver) = origin::Producer::new(origin.config());
    let gated = match gate.dynamic(path, origin::Route::default()) {
        Ok(dynamic) => dynamic,
        Err(err) => {
            warn!(%path, %err, "could not gate the broadcast");
            return None;
        }
    };
    let broadcast = broadcast.clone();
    let path = path.to_owned();
    let root = origin.root().to_owned();
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        let through_gate = gate.consume();
        let run_gate = moq_net::time::run(gate_driver);
        tokio::pin!(run_gate);
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                // A route that outlived its broadcast would answer every new
                // request with a closed broadcast, so it goes with it.
                _ = broadcast.closed() => break,
                _ = &mut run_gate => break,
                request = gated.requested_broadcast() => match request {
                    Ok(request) => request.accept(&broadcast),
                    Err(_) => break,
                },
                request = offered.requested_broadcast() => match request {
                    Ok(request) => {
                        let through_gate = through_gate.clone();
                        let exact = request
                            .path()
                            .strip_prefix(&root)
                            .is_some_and(|requested| requested == path);
                        let path = path.clone();
                        // Resolving waits on the gate, whose driver this loop
                        // runs, so it waits elsewhere.
                        requests.spawn(async move {
                            if !exact {
                                request.reject(moq_net::Error::NotFound);
                                return;
                            }
                            match through_gate.request_broadcast(&path).await {
                                Ok(served) => request.accept(served),
                                Err(err) => request.reject(err),
                            }
                        });
                    }
                    Err(_) => break,
                },
                Some(_) = requests.join_next(), if !requests.is_empty() => {}
            }
        }
    })))
}

/// Converts a public audience into the state the registry keeps.
pub(crate) fn audience_kind(audience: &Audience) -> AudienceKind {
    match audience {
        Audience::Everyone => AudienceKind::Everyone,
        Audience::Peers(peers) => {
            let mut peers = peers.clone();
            AudienceKind::Peers(n0_watcher::Watcher::get(&mut peers))
        }
        Audience::Manual => AudienceKind::Manual,
    }
}

/// The set a `Peers` audience names right now.
pub(crate) type PeerSet = BTreeSet<EndpointId>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use moq_net::{Hop, Timestamp, bytes::Bytes, track};

    use super::*;

    const MAX_AGE: Duration = Duration::from_secs(5);
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Runs an origin, as a session's publish origin runs.
    fn origin() -> (origin::Producer, AbortOnDropHandle<()>) {
        let (origin, driver) =
            origin::Producer::new(origin::Config::new(Hop::new(7).expect("a valid hop")));
        let task = tokio::spawn(async move {
            moq_net::time::run(driver).await;
        });
        (origin, AbortOnDropHandle::new(task))
    }

    /// Writes a frame into a track every few milliseconds until it closes.
    fn writing() -> (broadcast::Producer, AbortOnDropHandle<()>) {
        let broadcast = broadcast::Info::new().produce();
        let mut track = broadcast
            .create_track("video", track::Info::default().with_max_age(MAX_AGE))
            .expect("create track");
        let task = tokio::spawn(async move {
            while track
                .write_frame(Timestamp::now(), Bytes::from_static(b"frame"))
                .is_ok()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        (broadcast, AbortOnDropHandle::new(task))
    }

    /// Resolves `path` on `origin` and returns the broadcast and a subscriber
    /// to its track that has read one group.
    async fn read(
        origin: &origin::Producer,
        path: &Path<'_>,
    ) -> (broadcast::Consumer, track::Subscriber) {
        let served = tokio::time::timeout(TIMEOUT, origin.consume().request_broadcast(path))
            .await
            .expect("timed out resolving")
            .expect("the offered path resolves");
        let mut subscriber = tokio::time::timeout(
            TIMEOUT,
            served
                .track("video")
                .expect("track")
                .subscribe(track::Subscription::default().with_max_age(MAX_AGE)),
        )
        .await
        .expect("timed out subscribing")
        .expect("subscribe");
        tokio::time::timeout(TIMEOUT, subscriber.recv_group())
            .await
            .expect("timed out reading")
            .expect("track failed")
            .expect("a group");
        (served, subscriber)
    }

    /// Pins down the moq-net behaviour the gate in [`serve`] exists for: a
    /// route answered with a broadcast itself keeps serving a subscriber after
    /// the route retracts, and a new request joins that front.
    ///
    /// If a moq-net release changes this, the gate is no longer needed.
    #[tokio::test]
    async fn a_retracted_splice_serves_on_without_a_gate() {
        let (origin, _origin) = origin();
        let (broadcast, _writer) = writing();
        let path = Path::new("live/publisher/cam");
        let route = origin
            .dynamic(&path, origin::Route::default())
            .expect("route");
        let consumer = broadcast.consume();
        let answer = AbortOnDropHandle::new(tokio::spawn(async move {
            while let Ok(request) = route.requested_broadcast().await {
                request.accept(&consumer);
            }
        }));
        let (served, mut subscriber) = read(&origin, &path).await;

        drop(answer);
        tokio::time::sleep(Duration::from_millis(200)).await;
        for _ in 0..3 {
            tokio::time::timeout(TIMEOUT, subscriber.recv_group())
                .await
                .expect("the subscription stalled")
                .expect("track failed")
                .expect("the subscription ended");
        }
        assert!(!served.is_closed());
        let again = tokio::time::timeout(TIMEOUT, origin.consume().request_broadcast(&path))
            .await
            .expect("timed out resolving again");
        assert!(again.is_ok(), "a new request no longer joins the front");
    }

    /// Withdrawing an offer ends what a peer already reads through it, and a
    /// new request for the path finds nothing.
    ///
    /// Read at the origin a session publishes from, so the peer's own
    /// behaviour on seeing the route retract plays no part: a peer that
    /// ignores the retraction must be cut off all the same.
    #[tokio::test]
    async fn a_withdrawn_offer_ends_its_subscriptions() {
        let (origin, _origin) = origin();
        let (broadcast, _writer) = writing();
        let path = Path::new("live/publisher/cam");
        let offer = serve(&origin, &path, &broadcast.consume()).expect("offer");
        let (served, mut subscriber) = read(&origin, &path).await;

        drop(offer);
        tokio::time::timeout(TIMEOUT, async {
            while let Ok(Some(_)) = subscriber.recv_group().await {}
        })
        .await
        .expect("the subscription outlived the offer");
        assert!(
            served.is_closed(),
            "the served broadcast outlived the offer"
        );
        let again = tokio::time::timeout(TIMEOUT, origin.consume().request_broadcast(&path))
            .await
            .expect("timed out resolving again");
        assert!(again.is_err(), "a withdrawn path resolved again");
    }
}
