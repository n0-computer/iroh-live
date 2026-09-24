//! Publications: a broadcast placed at a path, before an audience.

use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

use iroh::EndpointId;
use moq_net::{Path, PathOwned};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tracing::{debug, info};

use crate::{
    node::Shared,
    path::{LIVE, publisher_of},
    state::{self, PeerSet},
    ticket::BroadcastTicket,
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
    /// offered to relays, which would forward it to anyone.
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
/// Cheap to clone. The publication stays until [`unpublish`](Self::unpublish)
/// withdraws it, its broadcast ends, or the node shuts down; dropping the
/// handles leaves it in place.
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
}

impl Publication {
    pub(crate) fn new(id: u64, path: PathOwned, shared: Weak<Shared>) -> Self {
        Self {
            inner: Arc::new(PublicationInner { id, path, shared }),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.inner.id
    }

    /// Returns the path the broadcast is published at.
    pub fn path(&self) -> &Path<'_> {
        &self.inner.path
    }

    /// Returns the ticket that names this broadcast.
    ///
    /// `None` for a publication at any path other than
    /// `live/<publisher>/<name>`, such as a room's: a ticket names a publisher
    /// and a name, which only that layout spells.
    pub fn ticket(&self) -> Option<BroadcastTicket> {
        let path = &self.inner.path;
        let (namespace, rest) = path.next_part()?;
        if namespace != LIVE {
            return None;
        }
        let publisher = publisher_of(path)?;
        let (_, name) = rest.next_part()?;
        Some(BroadcastTicket::new(publisher, name.as_str()))
    }

    /// Replaces who may see the publication.
    ///
    /// Takes effect at once: links the new audience admits are offered it, and
    /// links it no longer admits see it withdrawn.
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

    /// Withdraws the publication from every link and from the route table.
    ///
    /// The broadcast itself keeps running; publish it again to offer it anew.
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
/// offer again, unless the publication's audience admits the link on its own.
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
        // A watcher whose watchable is gone keeps the set it last had.
        while let Ok(set) = peers.updated().await {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let mut state = shared.state.lock().expect("poisoned");
            let Some(entry) = state.publications.get_mut(&id) else {
                return;
            };
            debug!(path = %entry.path, peers = set.len(), "audience peers changed");
            entry.audience = AudienceKind::Peers(set);
            state.reconcile_publication(id);
        }
    })))
}
