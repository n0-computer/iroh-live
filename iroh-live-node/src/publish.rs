//! Publications: a broadcast created at a path in the route table, for an audience.

use std::sync::{Arc, Weak};

use iroh::EndpointId;
use moq_net::{Path, broadcast, origin};
use n0_error::e;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{Error, node::Shared, state::PubEntry};

/// Who may see a publication.
///
/// See [`moq_ext_access::Audience`].
pub type Audience = moq_ext_access::Audience<EndpointId>;

/// An [`Audience`] as the registry keeps it: the current set, not its watcher.
pub(crate) type AudienceKind = moq_ext_access::AudienceKind<EndpointId>;

/// A published broadcast.
///
/// The broadcast lives in the node's route table: fill it through
/// [`broadcast`](Self::broadcast), and every link whose peer may see it serves
/// that one broadcast, with no copy in between.
///
/// Cheap to clone. The publication stays until [`unpublish`](Self::unpublish),
/// until its broadcast closes, or until the node shuts down. Dropping the
/// handles leaves it in place.
#[derive(Debug, Clone)]
pub struct Publication {
    inner: Arc<PublicationInner>,
}

#[derive(derive_more::Debug)]
struct PublicationInner {
    id: u64,
    path: Path,
    #[debug(skip)]
    broadcast: broadcast::Producer,
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
    /// Returns the path the broadcast is published at.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Returns the broadcast to fill.
    ///
    /// Tracks created here are what peers read. Closing it withdraws the
    /// publication.
    pub fn broadcast(&self) -> &broadcast::Producer {
        &self.inner.broadcast
    }

    /// Waits until the publication is withdrawn.
    ///
    /// Cancellation safe: dropping the future loses nothing.
    pub async fn withdrawn(&self) {
        self.inner.withdrawn.cancelled().await;
    }

    /// Reports whether the publication has been withdrawn.
    pub fn is_withdrawn(&self) -> bool {
        self.inner.withdrawn.is_cancelled()
    }

    /// Changes who may see the publication.
    ///
    /// Takes effect on every live session at once: a peer that may no longer
    /// see it loses the path, and what it reads there ends with
    /// [`moq_net::Error::Unauthorized`]. Does nothing once withdrawn.
    pub fn set_audience(&self, audience: Audience) {
        let Some(shared) = self.inner.shared.upgrade() else {
            return;
        };
        let mut state = shared.state.lock().expect("poisoned");
        let id = self.inner.id;
        let Some(entry) = state.publications.get_mut(&id) else {
            return;
        };
        entry.peers_task = peers_task(&audience, id, &self.inner.shared);
        state.set_audience(id, AudienceKind::from(&audience));
    }

    /// Withdraws the publication from every link and from the route table.
    ///
    /// Peers lose the path, and what they read there ends with
    /// [`moq_net::Error::Unauthorized`]. The broadcast stays open for whoever
    /// holds it, unannounced.
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

/// Returns a task that re-authorizes the node's links as a `Peers` set changes.
///
/// `None` for any other audience. Holds the node weakly, since the task lives
/// in the state it updates.
fn peers_task(
    audience: &Audience,
    id: u64,
    shared: &Weak<Shared>,
) -> Option<AbortOnDropHandle<()>> {
    let Audience::Peers(set) = audience else {
        return None;
    };
    let set = set.clone();
    let shared = shared.clone();
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        let mut peers = set.watch();
        // Applied once up front too, for a change made while publishing.
        let mut current = peers.get();
        loop {
            {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let mut state = shared.state.lock().expect("poisoned");
                if !state.publications.contains_key(&id) {
                    return;
                }
                debug!(peers = current.len(), "audience peers set");
                state.set_audience(id, AudienceKind::Peers(current));
            }
            // This task holds `set`, so the watcher stays connected.
            let Ok(next) = peers.updated().await else {
                return;
            };
            current = next;
        }
    })))
}

/// Publishes a broadcast at `path`, for [`Node::publish`](crate::Node::publish).
pub(crate) fn publish<T>(
    shared: &Arc<Shared>,
    path: Path,
    audience: Audience,
    fill: impl FnOnce(&broadcast::Producer) -> T,
) -> Result<(Publication, T), Error> {
    check_path(&path)?;
    let weak = Arc::downgrade(shared);
    let mut state = shared.state.lock().expect("poisoned");
    if state.closed {
        return Err(e!(Error::ShutDown));
    }
    if let Some(existing) = state.publication_at(&path) {
        // An ended broadcast's closed task may not have run yet. Not a clash.
        let entry = &state.publications[&existing];
        if !entry.publication.broadcast().consume().is_closed() {
            return Err(e!(Error::Duplicate { path }));
        }
        state.remove_publication(existing);
    }
    let publication = shared
        .table
        .create_broadcast(&path)
        .map_err(|source| e!(Error::Node { source }))?;
    // Filled before the announcement, so no reader finds the broadcast
    // without the tracks `fill` creates.
    let filled = fill(publication.broadcast());
    // Announced under the lock, before any link may see the path, so no peer
    // learns of it before the table has it.
    publication
        .announce(origin::Route::default())
        .map_err(|source| e!(Error::Node { source }))?;
    let broadcast = publication.broadcast().clone();
    let id = state.next_id();
    let closed_task = {
        let consumer = broadcast.consume();
        let weak = weak.clone();
        let path = path.clone();
        tokio::spawn(async move {
            consumer.closed().await;
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
    info!(%path, ?audience, "published");
    let withdrawn = CancellationToken::new();
    state.add_publication(
        id,
        PubEntry {
            path: path.clone(),
            publication,
            audience: AudienceKind::from(&audience),
            peers_task: peers_task(&audience, id, &weak),
            _closed_task: AbortOnDropHandle::new(closed_task),
            withdrawn: withdrawn.clone(),
        },
    );
    let publication = Publication {
        inner: Arc::new(PublicationInner {
            id,
            path,
            broadcast,
            shared: weak,
            withdrawn,
        }),
    };
    Ok((publication, filled))
}

/// Refuses a path that is empty or holds a segment only a pattern can spell.
pub(crate) fn check_path(path: &Path) -> Result<(), Error> {
    if path.is_empty() || path.parts().any(|part| part == "*" || part == "**") {
        return Err(e!(Error::InvalidPath {
            path: path.as_str().to_owned()
        }));
    }
    Ok(())
}
