//! Publications: a broadcast created at a path in the route table, for an audience.

use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

use iroh::EndpointId;
use moq_net::{Path, broadcast, origin::RouteOptions};
use n0_error::{AnyError, e};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, debug_span, info};

use crate::{
    BroadcastTicket, Error,
    node::Shared,
    state::{PubEntry, State},
};

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
    /// The id of the node that published.
    peer: EndpointId,
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

    /// Returns the ticket that names this publication.
    ///
    /// It holds the node's endpoint id and the path, and nothing about layouts.
    #[must_use]
    pub fn ticket(&self) -> BroadcastTicket {
        BroadcastTicket::at(self.inner.peer, self.inner.path.clone())
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
        entry.audience_generation += 1;
        entry.peers_task = peers_task(
            &audience,
            id,
            entry.audience_generation,
            &entry.path,
            &self.inner.shared,
        );
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
/// in the state it updates. `generation` is the publication's audience
/// generation the task was started for.
fn peers_task(
    audience: &Audience,
    id: u64,
    generation: u64,
    path: &Path,
    shared: &Weak<Shared>,
) -> Option<AbortOnDropHandle<()>> {
    let Audience::Peers(set) = audience else {
        return None;
    };
    let set = set.clone();
    let shared = shared.clone();
    let span = debug_span!("audience", %path, generation);
    let task = async move {
        let mut peers = set.watch();
        // Applied once up front too, for a change made while publishing.
        let mut current = peers.get();
        loop {
            {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let mut state = shared.state.lock().expect("poisoned");
                if !apply_peers(&mut state, id, generation, current) {
                    return;
                }
            }
            // This task holds `set`, so the watcher stays connected.
            let Ok(next) = peers.updated().await else {
                return;
            };
            current = next;
        }
    };
    Some(AbortOnDropHandle::new(tokio::spawn(task.instrument(span))))
}

/// Sets publication `id`'s audience to `peers`, for the peers task of `generation`.
///
/// Returns `false`, and changes nothing, once the publication is gone or a
/// later [`Publication::set_audience`] replaced that task. The task then
/// ends: it may have been aborted while it waited on the lock.
fn apply_peers(state: &mut State, id: u64, generation: u64, peers: BTreeSet<EndpointId>) -> bool {
    let Some(entry) = state.publications.get(&id) else {
        return false;
    };
    if entry.audience_generation != generation {
        debug!(path = %entry.path, generation, "audience replaced, peers task ends");
        return false;
    }
    debug!(path = %entry.path, generation, peers = peers.len(), "audience peers set");
    state.set_audience(id, AudienceKind::Peers(peers));
    true
}

/// Publishes a broadcast at `path`, for [`Node::publish`](crate::Node::publish).
///
/// Creates the broadcast and runs `fill` before taking the node's lock, so a
/// fill that panics or calls back into the node cannot poison or deadlock
/// it. The broadcast stays unannounced until the lock is held.
pub(crate) fn publish<T, E>(
    shared: &Arc<Shared>,
    path: Path,
    audience: Audience,
    fill: impl FnOnce(&broadcast::Producer) -> Result<T, E>,
) -> Result<(Publication, T), Error>
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    check_path(&path)?;
    if shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let publication = shared
        .table
        .route(path.clone(), RouteOptions::hidden())
        .map_err(|source| e!(Error::Moq { source }))?;
    // Filled before the announcement, so no reader finds the broadcast
    // without the tracks `fill` creates.
    let filled = match fill(publication.broadcast()) {
        Ok(filled) => filled,
        Err(err) => {
            publication.broadcast().close();
            return Err(e!(Error::Fill {
                source: AnyError::from_std_box(err.into())
            }));
        }
    };
    // Ends the broadcast `fill` may have spawned work on, if it is not published.
    let refuse = |err: Error| {
        publication.broadcast().close();
        Err(err)
    };
    let weak = Arc::downgrade(shared);
    let mut state = shared.state.lock().expect("poisoned");
    if state.closed {
        return refuse(e!(Error::ShutDown));
    }
    if let Some(existing) = state.publication_at(&path) {
        // An ended broadcast's closed task may not have run yet. Not a clash.
        let entry = &state.publications[&existing];
        if !entry.publication.broadcast().consume().is_closed() {
            return refuse(e!(Error::Duplicate { path }));
        }
        state.remove_publication(existing);
    }
    // Announced under the lock, before any link may see the path, so no peer
    // learns of it before the table has it.
    if let Err(source) = publication.announce() {
        return refuse(e!(Error::Moq { source }));
    }
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
            peers_task: peers_task(&audience, id, 0, &path, &weak),
            audience_generation: 0,
            _closed_task: AbortOnDropHandle::new(closed_task),
            withdrawn: withdrawn.clone(),
        },
    );
    let publication = Publication {
        inner: Arc::new(PublicationInner {
            id,
            peer: shared.id,
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

#[cfg(test)]
mod tests {
    use iroh::{Endpoint, SecretKey, endpoint::presets};

    use super::*;
    use crate::{Node, NodeConfig};

    #[test]
    fn check_path_refuses_empty_and_wildcard_paths() {
        let check = |path: &str| check_path(&Path::new(path).expect("a path"));
        assert!(check("live/alice/cam").is_ok());
        assert!(matches!(check(""), Err(Error::InvalidPath { .. })));
        assert!(matches!(
            check("live/*/cam"),
            Err(Error::InvalidPath { .. })
        ));
        assert!(matches!(check("live/**"), Err(Error::InvalidPath { .. })));
    }

    /// A peers task that set_audience replaced cannot write its old set back.
    ///
    /// Aborting the task does not stop one that already waits on the lock, so
    /// this calls the step it takes under the lock with its old generation.
    #[tokio::test]
    async fn a_replaced_peers_task_does_not_restore_its_set() {
        let endpoint = Endpoint::bind(presets::Minimal).await.expect("binds");
        let node = Node::new(endpoint.clone(), NodeConfig::default());
        let (revoked, kept) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let old = BTreeSet::from([revoked, kept]);
        let publication = node
            .publish("live/test/cam", Audience::peers(old.clone()))
            .expect("publish");
        let new = BTreeSet::from([kept]);
        publication.set_audience(Audience::peers(new.clone()));

        let id = publication.inner.id;
        {
            let mut state = node.shared.state.lock().expect("poisoned");
            assert!(!apply_peers(&mut state, id, 0, old), "the old task wrote");
            assert_eq!(state.publications[&id].audience, AudienceKind::Peers(new));
        }

        node.shutdown().await;
        endpoint.close().await;
    }
}
