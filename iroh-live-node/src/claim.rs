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
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Returns the claimed path.
    #[must_use]
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
    /// let the peer publish. The claim is added on top of each session's
    /// grant, on every session with the peer, including ones that
    /// start later. Dropping the last claim on a path narrows the grant again,
    /// and moq then ends what local readers read at the path from the peer.
    ///
    /// A live session applies the wider grant a moment after this returns, so
    /// a read of the path that starts at once may still be refused.
    ///
    /// A claim on a path of this node's own id is a no-op. A claim on a
    /// path the peer owns is recorded even while the peer's authorization
    /// (see [`authorization_for`](Self::authorization_for)) already lets it
    /// publish there: it widens nothing then, and keeps the path if a
    /// re-decide narrows the authorization. A path the peer does not own but its authorization
    /// covers is a no-op claim.
    ///
    /// Otherwise the peer has to own the path: the
    /// [`Layout::owner`](crate::Layout::owner) of
    /// [`NodeConfig::layout`](crate::NodeConfig::layout) decides if it is
    /// set. Otherwise exactly one segment of the path is the peer's endpoint
    /// id, and no other segment parses as one. That keeps a
    /// claimed path from overlapping any other peer's paths, which hop pinning
    /// and the route table rely on.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotOwner`] if the peer neither owns the path nor
    /// may publish there, [`Error::TooManyClaims`] if the peer already holds
    /// 64 claimed paths or 16 KiB of them, and [`Error::ShutDown`] once the
    /// node has shut down.
    pub fn claim_path(&self, peer: EndpointId, path: &Path) -> Result<PathClaim, Error> {
        if self.shared.shutdown.is_cancelled() {
            return Err(e!(Error::ShutDown));
        }
        check_path(path)?;
        // Only the authorization the peer already holds counts here: the
        // policy is async, and a claim must not wait for it.
        let granted = || {
            self.authorization_for(peer)
                .is_some_and(|authorization| authorization.grant.allows_publish(path.as_str()))
        };
        let unrecorded = || PathClaim {
            peer,
            path: path.clone(),
            shared: None,
        };
        if peer == self.shared.id {
            return Ok(unrecorded());
        }
        let owned = match &self.shared.layout {
            Some(layout) => layout.owner(path.as_str()) == Some(peer),
            None => owns_by_id(&peer, path),
        };
        if !owned {
            // The authorization alone allows the path, so there is nothing to keep.
            if granted() {
                return Ok(unrecorded());
            }
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
                ClaimRefused::TooMany => e!(Error::TooManyClaims { peer }),
            })?;
        debug!(peer = %peer.fmt_short(), %path, "ticket path claimed");
        Ok(PathClaim {
            peer,
            path: path.clone(),
            shared: Some(Arc::downgrade(&self.shared)),
        })
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn path(path: &str) -> Path {
        Path::new(path).expect("a path")
    }

    #[test]
    fn a_path_naming_the_peer_once_is_its_own() {
        let peer = SecretKey::generate().public();
        assert!(owns_by_id(&peer, &path(&format!("live/{peer}/cam"))));
        assert!(owns_by_id(&peer, &path(&format!("{peer}"))));
        assert!(owns_by_id(&peer, &path(&format!("a/b/{peer}"))));
    }

    #[test]
    fn a_path_naming_another_peer_or_none_is_not_its_own() {
        let (peer, other) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        assert!(!owns_by_id(&peer, &path("live/cam")));
        assert!(!owns_by_id(&peer, &path(&format!("live/{other}/cam"))));
        assert!(!owns_by_id(&peer, &path(&format!("live/{peer}/{other}"))));
        assert!(!owns_by_id(&peer, &path(&format!("live/{peer}/{peer}"))));
    }

    #[test]
    fn only_the_canonical_spelling_of_the_id_counts() {
        let peer = SecretKey::generate().public();
        let upper = peer.to_string().to_ascii_uppercase();
        assert!(!owns_by_id(&peer, &path(&format!("live/{upper}/cam"))));
    }
}
