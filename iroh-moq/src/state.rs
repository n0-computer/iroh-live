//! The node's bookkeeping: publications, links, and what each link is offered.
//!
//! Everything here changes under one lock and never awaits, so `publish`,
//! `set_audience` and `offer` are synchronous.
//!
//! Each link has its own publish origin. Offering a publication on a link adds
//! a dynamic route at its path there, answered through a gate (see [`serve`]).

use std::collections::{BTreeMap, HashMap};

use iroh::EndpointId;
use moq_net::{Path, PathOwned, broadcast, origin};
use n0_future::task::{AbortOnDropHandle, JoinSet};
use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{Grant, LinkId, LinkKind, RouteInfo, Session, link::LinkState, publish::AudienceKind};

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
    pub(crate) _closed_task: AbortOnDropHandle<()>,
    /// Cancelled when the entry goes, for [`Publication::withdrawn`](crate::Publication::withdrawn).
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
    /// For a relay, whether `Everyone` publications go to it.
    pub(crate) public: bool,
    /// Whether the link feeds the route table.
    pub(crate) consume: bool,
    /// The running offers, by publication.
    #[debug(skip)]
    pub(crate) offers: HashMap<u64, Option<Serve>>,
    /// What the peer announces, by prefix: cost and hop count.
    pub(crate) announced: BTreeMap<PathOwned, (u64, usize)>,
    /// The session, for a direct link.
    pub(crate) session: Option<Session>,
    /// The latest reading of the link's connection monitor.
    #[debug(skip)]
    pub(crate) link_state: LinkState,
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

    /// Returns the publication at `path`, if any.
    pub(crate) fn publication_at(&self, path: &Path<'_>) -> Option<u64> {
        self.publications
            .iter()
            .find(|(_, publication)| publication.path == *path)
            .map(|(id, _)| *id)
    }

    /// Brings every link's offer of publication `id` in line with its audience.
    pub(crate) fn reconcile_publication(&mut self, id: u64) {
        let links: Vec<u64> = self.links.keys().copied().collect();
        for link in links {
            self.reconcile(id, link);
        }
    }

    /// Offers or withdraws `publication` on `link`, as [`visible`] decides.
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
        let offer = serve(
            &entry.publish,
            &publication_entry.path,
            &publication_entry.broadcast,
        );
        entry.offers.insert(publication, offer);
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

    /// Reports whether a relay link feeds the route table.
    pub(crate) fn has_relays(&self) -> bool {
        self.links
            .values()
            .any(|link| link.kind == LinkKind::Relay && link.consume)
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

/// Offers `broadcast` at `path` on `origin` until the handle drops.
///
/// The offer also ends with the broadcast. Returns `None`, and logs why, if
/// the origin refuses the route.
///
/// Requests are answered through a gate, an origin of its own that serves the
/// broadcast. moq-net keeps serving a spliced broadcast after its route
/// retracts, and new requests join it, so without the gate a peer could read on
/// after the offer was withdrawn. Dropping the handle tears the gate down,
/// which ends every subscription through it.
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
                // A route must not outlive its broadcast.
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
                        // Resolving waits on the gate's driver, which this
                        // loop runs.
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use moq_net::{Hop, Timestamp, bytes::Bytes, track};

    use super::*;

    const MAX_AGE: Duration = Duration::from_secs(5);
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Runs an origin, as a session's publish origin runs.
    fn origin() -> origin::Producer {
        moq_tokio::origin::spawn_config(origin::Config::new(Hop::new(7).expect("a valid hop")))
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

    /// Resolves `path` on `origin`, and subscribes to its track until a group arrives.
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

    /// A spliced route keeps serving after it retracts, and new requests join it.
    ///
    /// This is why [`serve`] has a gate. If a moq-net release changes it, the
    /// gate can go.
    #[tokio::test]
    async fn a_retracted_splice_serves_on_without_a_gate() {
        let origin = origin();
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

    /// Measures what the gate in [`serve`] costs, per offer and per group.
    ///
    /// Run it with `cargo nextest run -p iroh-moq --run-ignored only gate_cost`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, run by hand"]
    async fn gate_cost() {
        const OFFERS: usize = 1_000;
        const GROUPS: u64 = 20_000;
        let payload = Bytes::from(vec![7u8; 1_000]);

        let table = origin();
        let broadcast = broadcast::Info::new().produce();
        let started = std::time::Instant::now();
        let offers: Vec<Serve> = (0..OFFERS)
            .map(|n| {
                let path = format!("live/publisher/cam{n}");
                serve(&table, &Path::new(&path), &broadcast.consume()).expect("offer")
            })
            .collect();
        let per_offer = started.elapsed() / OFFERS as u32;
        drop(offers);
        println!("setting up an offer: {per_offer:?}");

        for gated in [false, true] {
            let origin = origin();
            let broadcast = broadcast::Info::new().produce();
            let mut track = broadcast
                .create_track("video", track::Info::default().with_max_age(MAX_AGE))
                .expect("create track");
            let path = Path::new("live/publisher/cam");
            let _offer = if gated {
                serve(&origin, &path, &broadcast.consume()).expect("offer")
            } else {
                let route = origin
                    .dynamic(&path, origin::Route::default())
                    .expect("route");
                let consumer = broadcast.consume();
                AbortOnDropHandle::new(tokio::spawn(async move {
                    while let Ok(request) = route.requested_broadcast().await {
                        request.accept(&consumer);
                    }
                }))
            };
            // One group first, so the subscription is in place before timing.
            track
                .write_frame(Timestamp::now(), payload.clone())
                .expect("write");
            let (_served, mut subscriber) = read(&origin, &path).await;
            let started = std::time::Instant::now();
            for _ in 0..GROUPS {
                track
                    .write_frame(Timestamp::now(), payload.clone())
                    .expect("write");
            }
            let mut received = 0;
            while received < GROUPS {
                let mut group = tokio::time::timeout(TIMEOUT, subscriber.recv_group())
                    .await
                    .expect("stalled")
                    .expect("track failed")
                    .expect("track ended");
                while group.read_frame().await.expect("frame").is_some() {}
                received += 1;
            }
            let elapsed = started.elapsed();
            println!(
                "{GROUPS} groups of 1 KB {}: {elapsed:?}, {:?} per group",
                if gated {
                    "through the gate"
                } else {
                    "spliced directly"
                },
                elapsed / GROUPS as u32,
            );
        }
    }

    /// Withdrawing an offer ends what a peer reads, and the path stops resolving.
    ///
    /// Read at the session's publish origin, so a peer that ignores the
    /// retraction is cut off all the same.
    #[tokio::test]
    async fn a_withdrawn_offer_ends_its_subscriptions() {
        let origin = origin();
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
