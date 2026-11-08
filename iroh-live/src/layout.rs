//! The `<root>/<peer>/<name>` path layout.
//!
//! The node takes any path. A layout is one convention for choosing them:
//! [`LiveLayout`] publishes a peer's broadcasts under `<root>/<peer>/`, and
//! lets a peer publish only there, so no peer can publish at a path that names
//! another. Every use names the layout. An application with another scheme
//! writes its paths through [`Node::publish`](crate::Node::publish) and
//! [`Node::subscribe`](crate::Node::subscribe), and gives its node a grant of
//! its own.
//!
//! Rooms paths (`rooms/...`) belong to `iroh-live-rooms`.

use std::sync::Arc;

use iroh::EndpointId;
use iroh_live_node::{
    Audience, BroadcastTicket, Error, Grant, Node, NodeConfig, Publication,
    net::{Path, Pattern, Patterns},
};
use moq_net::broadcast;
use n0_error::e;

/// The root of a layout that names none.
const DEFAULT_ROOT: &str = "live";

/// Names the paths a peer may publish under, given its id.
///
/// Extends a [`LiveLayout`]'s own scope: `layout.config(&[Arc::new(rooms::publish_scope)])`
/// lets peers publish room broadcasts too. A closure over a second layout or
/// application state works as well.
pub type Scope = Arc<dyn Fn(EndpointId) -> Pattern + Send + Sync>;

/// The `<root>/<peer>/<name>` layout, with the root `live` by default.
///
/// A node publishes its broadcasts at `<root>/<its id>/<name>`, and a node
/// that uses [`config`](Self::config) lets each peer publish only under its
/// own id. Two peers with different roots do not see each other's broadcasts:
/// the node reports [`NotGranted`](iroh_live_node::Error::NotGranted).
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
    /// segment, or makes no valid path. Names come from flags and user
    /// interfaces, so they are checked here.
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

    /// Returns the paths `peer` may publish its own broadcasts under: `<root>/<peer>/**`.
    #[must_use]
    pub fn scope(&self, peer: EndpointId) -> Pattern {
        format!("{}/{peer}/**", self.root)
            .parse()
            .expect("the root and an endpoint id are valid path segments")
    }

    /// Returns [`scope`](Self::scope) as a [`Scope`] that owns a copy of the layout.
    ///
    /// `a.config(&[b.scope_fn()])` composes two layouts.
    #[must_use]
    pub fn scope_fn(&self) -> Scope {
        let layout = self.clone();
        Arc::new(move |peer| layout.scope(peer))
    }

    /// Returns the grant a node gives `peer`.
    ///
    /// The peer may subscribe to anything, and publish only under
    /// [`scope`](Self::scope) and the `extra` scopes applied to it.
    #[must_use]
    pub fn grant(&self, peer: EndpointId, extra: &[Scope]) -> Grant {
        let mut publish = Patterns::from(self.scope(peer));
        for scope in extra {
            publish.insert(scope(peer));
        }
        Grant::publish_under(publish)
    }

    /// Returns the [`NodeConfig`] of a node on this layout.
    ///
    /// It admits every peer and applies [`grant`](Self::grant). Pass `&[]` for no extra scopes.
    #[must_use]
    pub fn config(&self, extra: &[Scope]) -> NodeConfig {
        let layout = self.clone();
        let extra: Arc<[Scope]> = extra.into();
        NodeConfig {
            grant: Some(Arc::new(move |peer| layout.grant(peer, &extra))),
            ..Default::default()
        }
    }

    /// Publishes a broadcast as `<root>/<this node's id>/<name>` to everyone, and returns it.
    ///
    /// The broadcast is created in the node's route table, and peers read it
    /// with nothing copied in between. `B` is what fills it, built from the
    /// table's `broadcast::Producer`: a `LocalBroadcast`, or the producer
    /// itself. Everyone includes attached relays.
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
    /// let producer: broadcast::Producer = layout.publish::<broadcast::Producer>(&node, "studio")?;
    /// # drop(producer);
    /// # Ok(())
    /// # }
    /// ```
    pub fn publish<B>(&self, node: &Node, name: &str) -> Result<B, Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        let (_publication, broadcast) =
            self.publish_with(node, name, Audience::Everyone, |producer| {
                B::try_from(producer.clone())
            })?;
        Ok(broadcast)
    }

    /// Publishes a broadcast as `<root>/<this node's id>/<name>` to `audience`, filling it first.
    ///
    /// `fill` runs before the announcement, as in [`Node::publish_with`], so
    /// no reader finds the broadcast without the tracks it creates, such as a
    /// catalog. The result comes back with the [`Publication`].
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
    /// It takes an id, so a command line with `--endpoint-id` and `--name`
    /// needs no node. Does not check that anything is published there.
    ///
    /// # Errors
    ///
    /// Fails like [`path`](Self::path).
    pub fn ticket(&self, peer: EndpointId, name: &str) -> Result<BroadcastTicket, Error> {
        Ok(BroadcastTicket::at(peer, self.path(peer, name)?))
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

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
        let grant = layout.grant(a, &[]);
        assert!(grant.allows_publish(layout.path(a, "cam").expect("path").as_str()));
        assert!(!grant.allows_publish(layout.path(b, "cam").expect("path").as_str()));
        assert!(grant.allows_subscribe("anything/at/all"));
    }

    #[test]
    fn extra_scopes_widen_the_grant() {
        let layout = LiveLayout::default();
        let peer = id();
        let extra: Scope = Arc::new(|peer| {
            format!("rooms/*/{peer}/**")
                .parse()
                .expect("a rooms-style pattern")
        });
        let grant = layout.grant(peer, &[extra]);
        assert!(grant.allows_publish(&format!("rooms/topic/{peer}/cam")));
        assert!(!grant.allows_publish(&format!("rooms/topic/{}/cam", id())));
        assert!(grant.allows_publish(&format!("live/{peer}/cam")));
    }

    #[test]
    fn two_layouts_compose() {
        let (a, b) = (LiveLayout::new("a"), LiveLayout::new("b"));
        let peer = id();
        let config = a.config(&[b.scope_fn()]);
        let grant = config.grant.as_ref().expect("a grant")(peer);
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
