//! Publications: a broadcast placed at a path, before an audience.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Weak},
};

use iroh::EndpointId;
use moq_net::{Path, PathOwned, broadcast};
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{
    Error,
    node::Shared,
    state::{self, PeerSet, PubEntry},
};

/// Who may see a publication.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum Audience {
    /// Every admitted session, and every relay that takes public publications.
    ///
    /// The default.
    #[default]
    Everyone,
    /// These peers, as the set changes.
    ///
    /// A room passes its membership, an application its friend list. Never
    /// offered to relays, which would forward it to anyone. Once the set's
    /// watchable is dropped the publication is offered to nobody, since no one
    /// keeps the set current any more.
    Peers(n0_watcher::Direct<BTreeSet<EndpointId>>),
    /// No one, until offered explicitly per session.
    ///
    /// Offered with [`Session::offer`] or [`RelayLink::offer`], for access
    /// decided per session by application code.
    ///
    /// [`Session::offer`]: crate::Session::offer
    /// [`RelayLink::offer`]: crate::RelayLink::offer
    Manual,
}

/// An [`Audience`] as the registry keeps it: the current set, not its watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudienceKind {
    Everyone,
    Peers(PeerSet),
    Manual,
}

/// A published broadcast.
///
/// Cheap to clone; two handles are equal when they name the same publication.
/// The publication stays until [`unpublish`](Self::unpublish) withdraws it,
/// its broadcast ends, or the node shuts down; dropping the handles leaves it in
/// place.
#[derive(Debug, Clone)]
pub struct Publication {
    inner: Arc<PublicationInner>,
}

#[derive(derive_more::Debug)]
struct PublicationInner {
    id: u64,
    path: PathOwned,
    #[debug(skip)]
    shared: Weak<Shared>,
    #[debug(skip)]
    withdrawn: CancellationToken,
}

impl PartialEq for Publication {
    fn eq(&self, other: &Self) -> bool {
        self.inner.id == other.inner.id && Weak::ptr_eq(&self.inner.shared, &other.inner.shared)
    }
}

impl Eq for Publication {}

impl Publication {
    pub(crate) fn new(
        id: u64,
        path: PathOwned,
        shared: Weak<Shared>,
        withdrawn: CancellationToken,
    ) -> Self {
        Self {
            inner: Arc::new(PublicationInner {
                id,
                path,
                shared,
                withdrawn,
            }),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.inner.id
    }

    /// Returns the path the broadcast is published at.
    pub fn path(&self) -> &Path<'_> {
        &self.inner.path
    }

    /// Replaces who may see the publication.
    ///
    /// Takes effect at once: links the new audience admits are offered it, and
    /// links it no longer admits see it withdrawn, which ends what their peers
    /// were reading.
    pub fn set_audience(&self, audience: Audience) {
        let Some(shared) = self.inner.shared.upgrade() else {
            return;
        };
        let mut state = shared.state.lock().expect("poisoned");
        let Some(entry) = state.publications.get_mut(&self.inner.id) else {
            return;
        };
        info!(path = %entry.path, ?audience, "audience changed");
        entry.audience = state::audience_kind(&audience);
        entry.peers_task = peers_task(&audience, self.inner.id, &self.inner.shared);
        entry.local = match entry.audience {
            AudienceKind::Everyone => entry
                .local
                .take()
                .or_else(|| state::serve(&shared.table, &entry.path, &entry.broadcast)),
            _ => None,
        };
        state.reconcile_publication(self.inner.id);
    }

    /// Waits until the publication is withdrawn.
    ///
    /// By [`unpublish`](Self::unpublish), by its broadcast ending, or by the
    /// node shutting down. Cancellation safe.
    pub async fn withdrawn(&self) {
        self.inner.withdrawn.cancelled().await;
    }

    /// Reports whether the publication has been withdrawn.
    pub fn is_withdrawn(&self) -> bool {
        self.inner.withdrawn.is_cancelled()
    }

    /// Withdraws the publication from every link and from the route table.
    ///
    /// Peers already reading it are cut off: their subscriptions end, and they
    /// cannot subscribe again. The broadcast itself keeps running; publish it
    /// again to offer it anew.
    pub fn unpublish(&self) {
        let Some(shared) = self.inner.shared.upgrade() else {
            return;
        };
        let removed = shared
            .state
            .lock()
            .expect("poisoned")
            .remove_publication(self.inner.id);
        if let Some(entry) = removed {
            info!(path = %entry.path, "unpublished");
        }
    }
}

/// Keeps a publication offered on one session or relay.
///
/// Returned by [`Session::offer`](crate::Session::offer) and
/// [`RelayLink::offer`](crate::RelayLink::offer). Dropping it withdraws the
/// offer again, unless the publication's audience admits the link on its own,
/// and a withdrawn offer ends the subscriptions the peer made through it.
#[derive(Debug)]
#[must_use = "dropping the guard withdraws the offer"]
pub struct OfferGuard {
    publication: u64,
    link: u64,
    shared: Weak<Shared>,
}

impl OfferGuard {
    /// Offers `publication` on `link`, counting the offer.
    pub(crate) fn new(shared: &Arc<Shared>, publication: u64, link: u64) -> Self {
        let mut state = shared.state.lock().expect("poisoned");
        if let Some(entry) = state.publications.get_mut(&publication) {
            *entry.manual.entry(link).or_default() += 1;
        }
        state.reconcile(publication, link);
        Self {
            publication,
            link,
            shared: Arc::downgrade(shared),
        }
    }
}

impl Drop for OfferGuard {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared.state.lock().expect("poisoned");
        if let Some(entry) = state.publications.get_mut(&self.publication)
            && let Some(count) = entry.manual.get_mut(&self.link)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                entry.manual.remove(&self.link);
            }
        }
        state.reconcile(self.publication, self.link);
    }
}

/// Returns a task that re-offers a `Peers` publication as its set changes.
///
/// `None` for any other audience.
///
/// Holds the node weakly: the task lives inside the registry it updates.
pub(crate) fn peers_task(
    audience: &Audience,
    id: u64,
    shared: &Weak<Shared>,
) -> Option<AbortOnDropHandle<()>> {
    let Audience::Peers(peers) = audience else {
        return None;
    };
    let mut peers = peers.clone();
    let shared = shared.clone();
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            // Whoever owned the set dropped it, a room that was left say, and
            // nobody will keep it current any more: fail closed.
            let (set, disconnected) = match peers.updated().await {
                Ok(set) => (set, false),
                Err(_) => (PeerSet::new(), true),
            };
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let mut state = shared.state.lock().expect("poisoned");
            let Some(entry) = state.publications.get_mut(&id) else {
                return;
            };
            if disconnected {
                info!(path = %entry.path, "audience set dropped, offering to nobody");
            } else {
                debug!(path = %entry.path, peers = set.len(), "audience peers changed");
            }
            entry.audience = AudienceKind::Peers(set);
            state.reconcile_publication(id);
            if disconnected {
                return;
            }
        }
    })))
}

/// Publishes `broadcast` at `path`, for [`Moq::publish`](crate::Moq::publish).
pub(crate) fn publish(
    shared: &Arc<Shared>,
    path: PathOwned,
    broadcast: broadcast::Consumer,
    audience: Audience,
) -> Result<Publication, Error> {
    check_path(&path)?;
    let weak = Arc::downgrade(shared);
    let mut state = shared.state.lock().expect("poisoned");
    if state.closed {
        return Err(e!(Error::ShutDown));
    }
    if let Some(existing) = state.publication_at(&path) {
        // A broadcast that ended is withdrawn by its closed task, which may
        // not have run yet; publishing anew at its path is not a clash.
        if !state.publications[&existing].broadcast.is_closed() {
            return Err(e!(Error::Duplicate { path }));
        }
        state.remove_publication(existing);
    }
    let id = state.next_id();
    let closed_task = {
        let broadcast = broadcast.clone();
        let weak = weak.clone();
        let path = path.clone();
        tokio::spawn(async move {
            broadcast.closed().await;
            let Some(shared) = weak.upgrade() else { return };
            let removed = shared
                .state
                .lock()
                .expect("poisoned")
                .remove_publication(id);
            if removed.is_some() {
                info!(%path, "broadcast ended, unpublished");
            }
        })
    };
    let kind = state::audience_kind(&audience);
    let local = matches!(audience, Audience::Everyone)
        .then(|| state::serve(&shared.table, &path, &broadcast))
        .flatten();
    info!(%path, ?audience, "published");
    let withdrawn = CancellationToken::new();
    state.add_publication(
        id,
        PubEntry {
            path: path.clone(),
            broadcast,
            audience: kind,
            manual: HashMap::new(),
            local,
            peers_task: peers_task(&audience, id, &weak),
            _closed_task: Some(AbortOnDropHandle::new(closed_task)),
            withdrawn: withdrawn.clone(),
        },
    );
    Ok(Publication::new(id, path, weak, withdrawn))
}

/// Refuses a path that is empty or holds a segment only a pattern can spell.
pub(crate) fn check_path(path: &Path<'_>) -> Result<(), Error> {
    if path.is_empty() || path.parts().any(|part| part == "*" || part == "**") {
        return Err(e!(Error::InvalidPath {
            path: path.as_str().to_owned()
        }));
    }
    Ok(())
}
