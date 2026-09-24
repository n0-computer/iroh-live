//! The node's bookkeeping: publications, links, and who offers and serves what.
//!
//! Everything here changes under one lock and never awaits, so the public calls
//! that change it (`publish`, `set_audience`, `offer`, dropping an offer guard)
//! are synchronous, and a publication appears on a link in the same critical
//! section that decides it should.
//!
//! Every link has a publish origin of its own. A publication is offered on a
//! link by adding a dynamic route at its path to that origin, answered by
//! splicing the publication's broadcast (`Request::accept`), so the peer sees
//! exactly the publications meant for it and moq keeps announcing and serving
//! them natively.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use iroh::EndpointId;
use moq_net::{Path, PathOwned, broadcast, origin};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watchable;
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

/// Offers `broadcast` at `path` on `origin`.
///
/// The offer lasts until the broadcast ends or the returned handle drops.
/// Returns `None` if the origin refuses the route, which it does only for a
/// path no pattern can spell or once its driver is gone; either is logged.
pub(crate) fn serve(
    origin: &origin::Producer,
    path: &Path<'_>,
    broadcast: &broadcast::Consumer,
) -> Option<Serve> {
    let dynamic = match origin.dynamic(path, origin::Route::default()) {
        Ok(dynamic) => dynamic,
        Err(err) => {
            warn!(%path, %err, "could not offer the broadcast");
            return None;
        }
    };
    let broadcast = broadcast.clone();
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            tokio::select! {
                // A route that outlived its broadcast would answer every new
                // request with a closed broadcast, so it goes with it.
                _ = broadcast.closed() => break,
                request = dynamic.requested_broadcast() => match request {
                    Ok(request) => request.accept(&broadcast),
                    Err(_) => break,
                },
            }
        }
        drop(dynamic);
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
