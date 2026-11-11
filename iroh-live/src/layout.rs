//! The `<root>/<peer>/<name>` path layout.
//!
//! The node takes any path. [`LiveLayout`] publishes a peer's broadcasts
//! under `<root>/<peer>/`, and lets a peer publish only there, so no peer can
//! publish at a path that names another.
//!
//! Rooms paths (`rooms/...`) belong to `iroh-live-rooms`.

use iroh::EndpointId;
pub use iroh_live_node::Layout;
use iroh_live_node::{
    Audience, BroadcastTicket, Error, Grant, Node, NodeConfig, Publication,
    net::{Path, Pattern, Patterns},
    policy::{self, PeerRequest, Policy},
};
use moq_net::broadcast;
use n0_error::e;

/// The root of a layout that names none.
const DEFAULT_ROOT: &str = "live";

/// The `<root>/<peer>/<name>` layout, with the root `live` by default.
///
/// Two peers with different roots do not see each other's broadcasts: the
/// node reports [`NotGranted`](iroh_live_node::Error::NotGranted).
///
/// # Examples
///
/// ```
/// use iroh_live::layout::LiveLayout;
///
/// let layout = LiveLayout::default();
/// let peer = iroh::SecretKey::generate().public();
/// let path = layout.path(peer, "studio")?;
/// assert_eq!(path.as_str(), format!("live/{peer}/studio"));
/// assert_eq!(layout.parse(&path), Some((peer, "studio")));
/// # Ok::<(), iroh_live::node::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveLayout {
    root: String,
}

impl Default for LiveLayout {
    fn default() -> Self {
        Self::new(DEFAULT_ROOT)
    }
}

impl LiveLayout {
    /// Creates a layout under `root`.
    ///
    /// # Panics
    ///
    /// Panics unless `root` is one valid path segment: not empty, no `/`, and
    /// not a wildcard.
    #[must_use]
    pub fn new(root: impl Into<String>) -> Self {
        let root = root.into();
        assert!(
            !root.is_empty() && !root.contains('/') && Pattern::literal(&root).is_ok(),
            "the root of a layout is one valid path segment, not {root:?}"
        );
        Self { root }
    }

    /// Returns the first segment of every path in this layout.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Returns the path of `peer`'s broadcast `name`: `<root>/<peer>/<name>`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidPath`] if `name` is empty, has an empty
    /// segment, or makes no valid path.
    pub fn path(&self, peer: EndpointId, name: &str) -> Result<Path, Error> {
        let invalid = || {
            e!(Error::InvalidPath {
                path: name.to_owned()
            })
        };
        if name.is_empty() || name.split('/').any(str::is_empty) {
            return Err(invalid());
        }
        let path = format!("{}/{peer}/{name}", self.root);
        Pattern::literal(&path).map_err(|_| invalid())?;
        Path::new(&path).map_err(|_| invalid())
    }

    /// Splits a path of this layout into the peer and the name.
    ///
    /// The inverse of [`path`](Self::path). Returns `None` for a path
    /// elsewhere, one whose second segment is not an endpoint id in its
    /// canonical hex spelling, or one without a name.
    #[must_use]
    pub fn parse<'a>(&self, path: &'a Path) -> Option<(EndpointId, &'a str)> {
        let (peer, name) = path
            .as_str()
            .strip_prefix(&self.root)?
            .strip_prefix('/')?
            .split_once('/')?;
        let id: EndpointId = peer.parse().ok()?;
        (id.to_string() == peer && !name.is_empty()).then_some((id, name))
    }

    /// Returns the grant a node gives `peer`, as [`grant`] does for this layout.
    #[must_use]
    pub fn grant(&self, peer: EndpointId) -> Grant {
        grant(self, peer)
    }

    /// Returns a policy that admits every peer with its [`grant`](Self::grant).
    ///
    /// Narrow it with [`Policy::filter`], and call `Node::reauthorize` when
    /// the filter's state changes. To let peers publish room broadcasts too,
    /// pass the pair of this layout and the rooms layout to [`policy()`].
    ///
    /// # Examples
    ///
    /// Admit only contacts:
    ///
    /// ```
    /// use std::collections::BTreeSet;
    ///
    /// use iroh_live::{NodeConfig, layout::LiveLayout, node::policy::Policy};
    /// use n0_watcher::{Watchable, Watcher};
    ///
    /// let contacts = Watchable::new(BTreeSet::<iroh::EndpointId>::new());
    /// let known = contacts.clone();
    /// let policy = LiveLayout::default()
    ///     .policy()
    ///     .filter(move |request| known.get().contains(request.peer()));
    /// let config = NodeConfig::default().with_policy(policy);
    /// # drop((config, contacts));
    /// ```
    #[must_use]
    pub fn policy(&self) -> impl Policy<EndpointId> + Clone + use<> {
        policy(self.clone())
    }

    /// Returns the [`NodeConfig`] of a node on this layout, with its [`policy`](Self::policy).
    #[must_use]
    pub fn config(&self) -> NodeConfig {
        config(self.clone())
    }

    /// Publishes a broadcast at `<root>/<this node's id>/<name>` to granted sessions, and returns it.
    ///
    /// `B` is built from the route table's `broadcast::Producer`: a
    /// `LocalBroadcast`, or the producer itself. The audience is
    /// [`Audience::Granted`], which includes attached relays.
    ///
    /// The publication lasts until the broadcast closes. A `LocalBroadcast`
    /// closes when it is closed or its last clone drops. A producer closes
    /// only when it is closed, since the node holds the broadcast open while
    /// it is published. To keep the [`Publication`], or to publish to another
    /// audience, use [`publish_with`](Self::publish_with).
    ///
    /// # Errors
    ///
    /// Fails if something is already published under `name`, the name makes
    /// no valid path, or `B` cannot be built from the broadcast.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use iroh_live::{Node, layout::LiveLayout};
    /// use moq_net::broadcast;
    ///
    /// # fn run(node: Node) -> Result<(), Box<dyn std::error::Error>> {
    /// let layout = LiveLayout::default();
    /// let producer: broadcast::Producer = layout.publish(&node, "studio")?;
    /// # drop(producer);
    /// # Ok(())
    /// # }
    /// ```
    pub fn publish<B>(&self, node: &Node, name: &str) -> Result<B, Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        let (_publication, broadcast) =
            self.publish_with(node, name, Audience::Granted, |producer| {
                B::try_from(producer.clone())
            })?;
        Ok(broadcast)
    }

    /// Publishes a broadcast at `<root>/<this node's id>/<name>` to `audience`, filling it first.
    ///
    /// `fill` runs before the announcement, as in [`Node::publish_with`], so
    /// no reader finds the broadcast without the tracks it creates.
    ///
    /// # Errors
    ///
    /// As [`publish`](Self::publish), and [`Error::Fill`] if `fill` fails.
    pub fn publish_with<T, E>(
        &self,
        node: &Node,
        name: &str,
        audience: Audience,
        fill: impl FnOnce(&broadcast::Producer) -> Result<T, E>,
    ) -> Result<(Publication, T), Error>
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let path = self.path(node.id(), name)?;
        node.publish_with(path, audience, fill)
    }

    /// Returns the ticket of `peer`'s broadcast `name`.
    ///
    /// Does not check that anything is published there.
    ///
    /// # Errors
    ///
    /// Fails like [`path`](Self::path).
    pub fn ticket(&self, peer: EndpointId, name: &str) -> Result<BroadcastTicket, Error> {
        Ok(BroadcastTicket::at(peer, self.path(peer, name)?))
    }
}

impl Layout<EndpointId> for LiveLayout {
    /// Returns `<root>/<peer>/**`.
    fn scope(&self, peer: &EndpointId) -> Patterns {
        let pattern: Pattern = format!("{}/{peer}/**", self.root)
            .parse()
            .expect("the root and an endpoint id are valid path segments");
        Patterns::from(pattern)
    }

    /// Returns the peer a path names, if [`path`](LiveLayout::path) spells it that way.
    fn owner(&self, path: &str) -> Option<EndpointId> {
        let path = Path::new(path).ok()?;
        let (peer, name) = self.parse(&path)?;
        (self.path(peer, name).ok()? == path).then_some(peer)
    }
}

/// Returns the grant a node gives `peer` under `layout`.
///
/// The peer may subscribe to anything, and publish only under its
/// [`scope`](Layout::scope) in `layout`.
#[must_use]
pub fn grant(layout: &impl Layout<EndpointId>, peer: EndpointId) -> Grant {
    Grant::publish_under(layout.scope(&peer))
}

/// Returns a policy that admits every peer with its [`grant`] under `layout`.
///
/// A pair of layouts is their union. For example,
/// `policy((LiveLayout::default(), RoomsLayout))` lets every peer publish its
/// own broadcasts and its room broadcasts.
#[must_use]
pub fn policy(layout: impl Layout<EndpointId> + Clone) -> impl Policy<EndpointId> + Clone {
    policy::from_fn(move |request: &PeerRequest| Ok(grant(&layout, *request.peer()).into()))
}

/// Returns the [`NodeConfig`] of a node on `layout`, with its [`policy()`].
#[must_use]
pub fn config(layout: impl Layout<EndpointId> + Clone) -> NodeConfig {
    NodeConfig::default().with_policy(policy(layout))
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;
    use iroh_live_node::SessionRequest;

    use super::*;

    fn id() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn path_and_parse_invert() {
        let layout = LiveLayout::default();
        let peer = id();
        for name in ["cam", "cam/hd", "call"] {
            let path = layout.path(peer, name).expect("a path");
            assert_eq!(layout.parse(&path), Some((peer, name)));
        }
        let elsewhere = Path::new(&format!("rooms/{peer}/cam")).expect("path");
        assert_eq!(layout.parse(&elsewhere), None);
        let other_root = LiveLayout::new("fleet").path(peer, "cam").expect("path");
        assert_eq!(layout.parse(&other_root), None);
        assert_eq!(
            layout.parse(&Path::new("live/nobody/cam").expect("path")),
            None
        );
        assert_eq!(
            layout.parse(&Path::new(&format!("livestock/{peer}/cam")).expect("path")),
            None
        );
    }

    #[test]
    fn parse_wants_the_canonical_id() {
        let layout = LiveLayout::default();
        let peer = id();
        let base32 = data_encoding::BASE32_NOPAD
            .encode(peer.as_bytes())
            .to_ascii_lowercase();
        let path = Path::new(&format!("live/{base32}/cam")).expect("path");
        assert_eq!(layout.parse(&path), None);
        let bare = Path::new(&format!("live/{peer}")).expect("path");
        assert_eq!(layout.parse(&bare), None);
    }

    #[test]
    fn the_scope_matches_only_the_peers_own_paths() {
        let layout = LiveLayout::default();
        let (a, b) = (id(), id());
        let grant = layout.grant(a);
        assert!(grant.allows_publish(layout.path(a, "cam").expect("path").as_str()));
        assert!(!grant.allows_publish(layout.path(b, "cam").expect("path").as_str()));
        assert!(grant.allows_subscribe("anything/at/all"));
    }

    #[test]
    fn the_layout_owns_what_it_scopes() {
        let layout = LiveLayout::default();
        let (peer, other) = (id(), id());
        let scope = layout.scope(&peer);
        for name in ["cam", "cam/hd"] {
            let path = layout.path(peer, name).expect("a path");
            assert!(scope.matches(path.as_str()), "{path}");
            assert_eq!(layout.owner(path.as_str()), Some(peer), "{path}");
            assert!(!layout.scope(&other).matches(path.as_str()), "{path}");
        }
        assert_eq!(layout.owner(&format!("live/{peer}")), None);
        assert_eq!(layout.owner(&format!("rooms/topic/{peer}/cam")), None);
        assert_eq!(layout.owner(&format!("fleet/{peer}/cam")), None);
    }

    #[test]
    fn two_layouts_compose() {
        let (a, b) = (LiveLayout::new("a"), LiveLayout::new("b"));
        let peer = id();
        let request = PeerRequest::new(peer, SessionRequest::default(), policy::Event::Connect);
        let grant = n0_future::future::block_on(Policy::decide(&policy((a, b)), &request))
            .expect("admitted")
            .grant;
        assert!(grant.allows_publish(&format!("a/{peer}/cam")));
        assert!(grant.allows_publish(&format!("b/{peer}/cam")));
        assert!(!grant.allows_publish(&format!("c/{peer}/cam")));
    }

    #[test]
    #[should_panic(expected = "one valid path segment")]
    fn new_panics_on_a_bad_root() {
        let _ = LiveLayout::new("a/b");
    }

    #[test]
    fn names_that_make_no_path_fail() {
        let layout = LiveLayout::default();
        let peer = id();
        for name in ["", "a//b", "/a", "a/", "*", "a/**"] {
            let err = layout.path(peer, name).expect_err(name);
            assert!(matches!(err, Error::InvalidPath { .. }), "{name}: {err:#}");
            assert!(layout.ticket(peer, name).is_err(), "{name}");
        }
    }
}
