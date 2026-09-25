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
    state::{self, PubEntry},
};

/// Who may see a publication.
#[derive(Debug, Clone, Default)]
pub enum Audience {
    /// Every admitted session, and every relay that takes public publications.
    #[default]
    Everyone,
    /// These peers, as the set changes.
    ///
    /// Never offered to relays. Once the set's watchable is dropped, the
    /// publication is offered to nobody.
    Peers(n0_watcher::Direct<BTreeSet<EndpointId>>),
    /// No one, until offered with [`Session::offer`] or [`RelayLink::offer`].
    ///
    /// [`Session::offer`]: crate::Session::offer
    /// [`RelayLink::offer`]: crate::RelayLink::offer
    Manual,
}

/// An [`Audience`] as the registry keeps it: the current set, not its watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudienceKind {
    Everyone,
    Peers(BTreeSet<EndpointId>),
    Manual,
}

impl From<&Audience> for AudienceKind {
    fn from(audience: &Audience) -> Self {
        match audience {
            Audience::Everyone => Self::Everyone,
            Audience::Peers(peers) => Self::Peers(peers.clone().get()),
            Audience::Manual => Self::Manual,
        }
    }
}

/// A published broadcast.
///
/// Cheap to clone. The publication stays until [`unpublish`](Self::unpublish),
/// until its broadcast ends, or until the node shuts down. Dropping the handles
/// leaves it in place.
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
    /// Takes effect at once. Peers the new audience leaves out lose what they
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
        entry.audience = AudienceKind::from(&audience);
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
    pub async fn withdrawn(&self) {
        self.inner.withdrawn.cancelled().await;
    }

    /// Reports whether the publication has been withdrawn.
    pub fn is_withdrawn(&self) -> bool {
        self.inner.withdrawn.is_cancelled()
    }

    /// Withdraws the publication from every link and from the route table.
    ///
    /// Ends what peers read through it. The broadcast keeps running and can be
    /// published again.
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
/// Dropping it withdraws the offer, unless the audience admits the link
/// anyway, and ends what the peer read through it.
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
/// `None` for any other audience. Holds the node weakly, since the task lives
/// in the state it updates.
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
            // The set's owner dropped it: offer to nobody.
            let (set, disconnected) = match peers.updated().await {
                Ok(set) => (set, false),
                Err(_) => (BTreeSet::new(), true),
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
        // An ended broadcast's closed task may not have run yet. Not a clash.
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
            audience: AudienceKind::from(&audience),
            manual: HashMap::new(),
            local,
            peers_task: peers_task(&audience, id, &weak),
            _closed_task: AbortOnDropHandle::new(closed_task),
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
