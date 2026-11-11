//! Ticket claims: widening a peer's grant to the one path a ticket names.

use std::{
    str::FromStr,
    sync::{Arc, Weak},
};

use iroh::EndpointId;
use moq_net::Path;
use n0_error::e;
use tracing::debug;

use crate::{Error, Node, node::Shared, publish::check_path, state::ClaimRefused};

/// Names the peer that owns a path, for [`NodeConfig::owner`](crate::NodeConfig::owner).
///
/// Returns `None` for a path no peer owns.
pub type OwnerFn = Arc<dyn Fn(&Path) -> Option<EndpointId> + Send + Sync>;

/// A claim on one path of one peer, held for as long as it widens the peer's grant.
///
/// Created by [`Node::claim_path`]. Dropping the last claim on a path narrows
/// the peer's sessions again. Not `Clone`: share it in an `Arc`.
#[derive(derive_more::Debug)]
#[debug("PathClaim({}, {})", peer.fmt_short(), path)]
pub struct PathClaim {
    peer: EndpointId,
    path: Path,
    /// `None` for a claim on the node's own paths, which widens nothing.
    #[debug(skip)]
    shared: Option<Weak<Shared>>,
}

impl PathClaim {
    /// Returns the peer the claim is for.
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Returns the claimed path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PathClaim {
    fn drop(&mut self) {
        let Some(shared) = self.shared.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        // A panic elsewhere may have poisoned the lock. Do not panic again.
        if let Ok(mut state) = shared.state.lock() {
            state.release_path(&self.peer, &self.path);
            debug!(peer = %self.peer.fmt_short(), path = %self.path, "ticket claim released");
        }
    }
}

/// Reports whether `peer` owns `path` by the generic rule.
///
/// Exactly one segment is the peer's id in its canonical 64-hex spelling, and
/// no other segment parses as an endpoint id. So a widened path names its
/// publisher and no other peer, and cannot overlap another peer's paths.
pub(crate) fn owns_by_id(peer: &EndpointId, path: &Path) -> bool {
    let own = peer.to_string();
    let mut named = 0;
    for part in path.parts() {
        if part == own {
            named += 1;
        } else if EndpointId::from_str(part).is_ok() {
            return false;
        }
    }
    named == 1
}

impl Node {
    /// Widens what `peer` may publish on this node to `path`, for as long as the claim lives.
    ///
    /// This is how a ticket subscribe reads a path the node's grant would not
    /// let the peer publish. The claim is added to the peer's grant on top of
    /// its base grant, on every session with the peer, including ones that
    /// start later. Dropping the last claim on a path narrows the grant again,
    /// and moq then ends what local readers read at the path from the peer.
    ///
    /// A live session applies the wider grant a moment after this returns, so
    /// a read of the path that starts at once may still be refused.
    ///
    /// A path that the peer's live authorization (see
    /// [`grant_for`](Self::grant_for)) already lets it publish, and any path
    /// of this node's own id, needs no widening, so the claim is a no-op. A
    /// claim made before the peer has a session is recorded, and widens
    /// nothing while the peer's grant covers the path.
    ///
    /// Otherwise the peer has to own the path: [`NodeConfig::owner`](crate::NodeConfig::owner)
    /// decides if it is set. Otherwise exactly one segment of the path is the
    /// peer's endpoint id, and no other segment parses as one. That keeps a
    /// claimed path from overlapping any other peer's paths, which hop pinning
    /// and the route table rely on.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotOwner`] if the peer does not own the path,
    /// [`Error::TooManyGrants`] if the peer already holds 64 claimed paths or
    /// 16 KiB of them, and [`Error::ShutDown`] once the node has shut down.
    pub fn claim_path(&self, peer: EndpointId, path: &Path) -> Result<PathClaim, Error> {
        if self.shared.shutdown.is_cancelled() {
            return Err(e!(Error::ShutDown));
        }
        check_path(path)?;
        // Only the authorization the peer already holds counts here: the
        // policy is async, and a claim must not wait for it.
        let granted = || {
            self.grant_for(peer)
                .is_some_and(|authorization| authorization.grant.allows_publish(path.as_str()))
        };
        if peer == self.shared.id || granted() {
            return Ok(PathClaim {
                peer,
                path: path.clone(),
                shared: None,
            });
        }
        let owned = match &self.shared.owner {
            Some(owner) => owner(path) == Some(peer),
            None => owns_by_id(&peer, path),
        };
        if !owned {
            return Err(e!(Error::NotOwner {
                path: path.clone(),
                peer
            }));
        }
        self.shared
            .state
            .lock()
            .expect("poisoned")
            .claim_path(peer, path)
            .map_err(|refused| match refused {
                ClaimRefused::Closed => e!(Error::ShutDown),
                ClaimRefused::TooMany => e!(Error::TooManyGrants { peer }),
            })?;
        debug!(peer = %peer.fmt_short(), %path, "ticket path claimed");
        Ok(PathClaim {
            peer,
            path: path.clone(),
            shared: Some(Arc::downgrade(&self.shared)),
        })
    }
}
