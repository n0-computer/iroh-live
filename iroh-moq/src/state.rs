//! The node's bookkeeping: publications, links, and what each link is offered.
//!
//! Everything here changes under one lock and never awaits, so `publish`,
//! `publish` and `unpublish` are synchronous.
//!
//! Each link has its own publish origin. Offering a publication on a link adds
//! a route at its path there (see [`serve`]). What links announce goes straight
//! into the node's route table instead, so the state only maps a route's source
//! back to its link (see [`State::link_for`]).

use std::collections::{BTreeMap, HashMap};

use iroh::EndpointId;
use moq_net::{Hop, Instance, Path, broadcast, origin, origin::Source};
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{Grant, LinkKind, Session, link::LinkState, publish::AudienceKind};

/// A running offer: the task answering requests for one path on one origin.
pub(crate) type Serve = AbortOnDropHandle<()>;

/// The node's publications and links.
#[derive(Debug, Default)]
pub(crate) struct State {
    next_id: u64,
    pub(crate) publications: BTreeMap<u64, PubEntry>,
    pub(crate) links: BTreeMap<u64, LinkEntry>,
    /// Set once the node shuts down; nothing is added after that.
    pub(crate) closed: bool,
}

/// One publication.
#[derive(derive_more::Debug)]
pub(crate) struct PubEntry {
    pub(crate) path: Path,
    #[debug(skip)]
    pub(crate) broadcast: broadcast::Consumer,
    pub(crate) audience: AudienceKind,
    /// The stream every offer of the publication serves.
    ///
    /// One instance on every link and in the route table, so a reader moves
    /// between a direct route and a relay's without a break.
    pub(crate) instance: Instance,
    /// The publication's route in the node's own route table.
    ///
    /// Only for an `Everyone` publication, so a local subscriber resolves it.
    pub(crate) _local: Option<Serve>,
    /// Follows a `Peers` audience's set.
    pub(crate) _peers_task: Option<AbortOnDropHandle<()>>,
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
    /// The identity the link's routes are attributed to in the route table.
    pub(crate) hop: Hop,
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

    /// Removes a link, withdrawing its offers. Its session takes its routes
    /// out of the route table as it ends.
    pub(crate) fn remove_link(&mut self, id: u64) {
        self.links.remove(&id);
    }

    /// Adds a publication and offers it on every link it should reach.
    pub(crate) fn add_publication(&mut self, id: u64, publication: PubEntry) {
        self.publications.insert(id, publication);
        self.reconcile_publication(id);
    }

    /// Removes a publication from every link and from the route table.
    pub(crate) fn remove_publication(&mut self, id: u64) -> Option<PubEntry> {
        let entry = self.publications.remove(&id)?;
        for link in self.links.values_mut() {
            link.offers.remove(&id);
        }
        Some(entry)
    }

    /// Returns the publication at `path`, if any.
    pub(crate) fn publication_at(&self, path: &Path) -> Option<u64> {
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
        let wanted = visible(publication_entry, entry);
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
            &publication_entry.instance,
            &publication_entry.broadcast,
        );
        entry.offers.insert(publication, offer);
    }

    /// Returns the link whose peer announced a route from `source`.
    ///
    /// A route is attributed to the hop its session's peer declares, or else
    /// to the hop this node assigned the session. The two agree for peers on
    /// this crate. A relay that declares a hop of its own matches no link, and
    /// stands for the oldest relay link that feeds the table. `None` for a
    /// route published on this node.
    pub(crate) fn link_for(&self, source: Source) -> Option<u64> {
        let Source::Peer(hop) = source else {
            return None;
        };
        let by_hop = self.links.iter().find(|(_, link)| link.hop == hop);
        let relay = || {
            self.links
                .iter()
                .find(|(_, link)| link.kind == LinkKind::Relay && link.consume)
        };
        by_hop.or_else(relay).map(|(id, _)| *id)
    }

    /// Reports whether a relay link feeds the route table.
    pub(crate) fn has_relays(&self) -> bool {
        self.links
            .values()
            .any(|link| link.kind == LinkKind::Relay && link.consume)
    }
}

/// Reports whether `publication` belongs on link `id`.
///
/// Offered when the link's grant covers the path and the audience admits
/// the link. Relays see `Everyone`
/// publications only when their configuration offers public ones, and never a
/// `Peers` one, which a relay would forward to anyone.
fn visible(publication: &PubEntry, link: &LinkEntry) -> bool {
    if !link.grant.allows_subscribe(publication.path.as_str()) {
        return false;
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

/// Offers `broadcast` at `path` on `origin`, as `instance`, until the handle drops.
///
/// The offer also ends with the broadcast. Returns `None`, and logs why, if
/// the origin refuses the path.
pub(crate) fn serve(
    origin: &origin::Producer,
    path: &Path,
    instance: &Instance,
    broadcast: &broadcast::Consumer,
) -> Option<Serve> {
    let publication = match origin.create_broadcast(path) {
        Ok(publication) => publication,
        Err(err) => {
            warn!(%path, %err, "could not offer the broadcast");
            return None;
        }
    };
    let copy = moq_ext_forward::forward(broadcast.clone(), publication.broadcast().clone());
    if let Err(err) =
        publication.announce(origin::Route::default().with_instance(instance.clone(), 0))
    {
        warn!(%path, %err, "could not announce the broadcast");
        return None;
    }
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        // The publication stays announced while the copy runs.
        let _publication = publication;
        copy.await;
    })))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use moq_net::{Hop, bytes::Bytes, track};

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
                .write_frame(None, Bytes::from_static(b"frame"))
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
        path: &Path,
    ) -> (broadcast::Consumer, track::Subscriber) {
        let served = tokio::time::timeout(TIMEOUT, origin.consume().request(path, None))
            .await
            .expect("timed out resolving")
            .expect("the offered path resolves");
        let mut subscriber = tokio::time::timeout(
            TIMEOUT,
            served.track("video").expect("track").subscribe(
                track::Subscription::default()
                    .with_delay(moq_net::track::Delay::default().with_max(MAX_AGE)),
            ),
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

    /// Withdrawing an offer closes what it served, and the path stops resolving.
    ///
    /// Tracks already read keep running, as moq-lite wants: a retraction does
    /// not disturb subscriptions in flight.
    #[tokio::test]
    async fn a_withdrawn_offer_stops_resolving() {
        let origin = origin();
        let (broadcast, _writer) = writing();
        let path = Path::new("live/publisher/cam").expect("a valid path");
        let offer =
            serve(&origin, &path, &Instance::random(), &broadcast.consume()).expect("offer");
        let (served, _subscriber) = read(&origin, &path).await;

        drop(offer);
        tokio::time::timeout(TIMEOUT, served.closed())
            .await
            .expect("the served broadcast outlived the offer");
        let again = tokio::time::timeout(TIMEOUT, origin.consume().request(&path, None))
            .await
            .expect("timed out resolving again");
        assert!(again.is_err(), "a withdrawn path resolved again");
    }
}
