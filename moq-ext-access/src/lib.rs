//! Access control for moq nodes, independent of the transport.
//!
//! - [`Grant`]: what a session may subscribe to and publish.
//! - [`SessionRequest`]: what a peer asked for when it opened a session.
//! - [`Audience`]: who may see a publication, generic over the peer id.
//! - [`AudienceKind`]: an [`Audience`] as a registry keeps it, the current set.
//! - [`visible_paths`]: the paths a link may see, given its [`LinkRole`]. A
//!   node hands them to moq as the subscribe half of the link's grant, and moq
//!   enforces it on the live session.
//!
//! The peer id is any type that implements [`PeerId`], such as an iroh
//! endpoint id.
//!
//! A conversion to and from `moq_auth::Grant` is left out on purpose:
//! moq-auth carries the JWT and crypto stack, which this crate stays free of.

use std::{collections::BTreeSet, fmt::Debug, fmt::Display, hash::Hash};

use moq_net::{Pattern, Patterns};

/// A peer's identity: any small, comparable, printable id.
pub trait PeerId: Clone + Eq + Ord + Hash + Debug + Display + Send + Sync + 'static {}

impl<T: Clone + Eq + Ord + Hash + Debug + Display + Send + Sync + 'static> PeerId for T {}

/// What a session may subscribe to from a node, and publish into it.
///
/// The shape of a moq-auth grant, so a token means the same thing to a node
/// and to a moq relay. Patterns are moq's: `*` matches one segment, `**` a
/// subtree, anything else itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The paths the peer may subscribe to.
    pub subscribe: Patterns,
    /// The paths the peer may publish into the node's route table.
    pub publish: Patterns,
}

impl Grant {
    /// Returns a grant that allows everything in both directions.
    #[must_use]
    pub fn everything() -> Self {
        Self {
            subscribe: Patterns::from(Pattern::all()),
            publish: Patterns::from(Pattern::all()),
        }
    }

    /// Returns a grant to subscribe to everything and publish nothing.
    #[must_use]
    pub fn subscribe_only() -> Self {
        Self {
            subscribe: Patterns::from(Pattern::all()),
            publish: Patterns::new(),
        }
    }

    /// Returns a grant to subscribe to everything and publish only at `publish`.
    ///
    /// With a pattern that names the peer, such as `live/<peer id>/**`, no peer
    /// can publish at another's paths.
    #[must_use]
    pub fn publish_under(publish: impl Into<Patterns>) -> Self {
        Self {
            subscribe: Patterns::from(Pattern::all()),
            publish: publish.into(),
        }
    }

    /// Reports whether the peer may subscribe to `path`.
    #[must_use]
    pub fn allows_subscribe(&self, path: &str) -> bool {
        self.subscribe.matches(path)
    }

    /// Reports whether the peer may publish `path`.
    #[must_use]
    pub fn allows_publish(&self, path: &str) -> bool {
        self.publish.matches(path)
    }
}

/// What a peer asked for when it opened a session.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
}

impl SessionRequest {
    /// Parses a request target, `/<path>?<query>`, and its headers.
    #[must_use]
    pub fn new(target: &str, headers: Vec<(String, String)>) -> Self {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = url::form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        Self {
            path: path.trim_matches('/').to_owned(),
            query,
            headers,
        }
    }

    /// Returns the request path, without its query.
    ///
    /// The setup path on a raw session, the CONNECT path on an HTTP/3 one.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns a query parameter of the request, such as `jwt`.
    #[must_use]
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Returns an HTTP/3 CONNECT header, matched case-insensitively.
    ///
    /// Always `None` on raw sessions.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Who may see a publication.
#[derive(Debug, Clone)]
pub enum Audience<P: PeerId> {
    /// Every admitted session, and every relay that takes public publications.
    Everyone,
    /// These peers, as the set changes.
    ///
    /// Never offered to relays. The publication holds the set: change it
    /// through a clone of the watchable.
    Peers(n0_watcher::Watchable<BTreeSet<P>>),
}

#[allow(
    clippy::derivable_impls,
    reason = "a derived Default would require `P: Default`, which peer ids such as iroh's EndpointId are not"
)]
impl<P: PeerId> Default for Audience<P> {
    fn default() -> Self {
        Self::Everyone
    }
}

/// An [`Audience`] as a registry keeps it: the current set, not its watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudienceKind<P: PeerId> {
    /// See [`Audience::Everyone`].
    Everyone,
    /// See [`Audience::Peers`], as of now.
    Peers(BTreeSet<P>),
}

impl<P: PeerId> From<&Audience<P>> for AudienceKind<P> {
    fn from(audience: &Audience<P>) -> Self {
        match audience {
            Audience::Everyone => Self::Everyone,
            Audience::Peers(peers) => Self::Peers(peers.get()),
        }
    }
}

/// What kind of link a publication may be offered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRole {
    /// A session with a peer.
    Direct,
    /// A relay. `public` says whether it takes [`Audience::Everyone`] publications.
    Relay {
        /// Whether `Everyone` publications go to the relay.
        public: bool,
    },
}

/// Returns the exact paths of `publications` a link may see, as patterns.
///
/// A publication is visible when the link's `grant` covers its path and its
/// audience admits the link. A relay sees `Everyone` publications only when
/// `public`, and never a `Peers` one, which a relay would forward to anyone. A
/// direct link is in a `Peers` audience when its `remote` is in the set.
///
/// Each path is a literal pattern, so nothing beside the listed publications
/// matches: not a path below one, and not a route the node learned from
/// another link. A path no pattern can spell is left out.
///
/// The result grows with the number of visible publications, and moq bounds
/// it. moq refuses an AUTH_OK larger than 64 KiB, which a session that
/// reports its grant to the peer sends, and caps a union of patterns at
/// [`Pattern::MAX_INTERSECTIONS`] (1024) members. A link that may see more
/// publications than that, or whose paths add up to more bytes, cannot be
/// granted them one by one.
#[must_use]
pub fn visible_paths<'a, P: PeerId>(
    publications: impl IntoIterator<Item = (&'a str, &'a AudienceKind<P>)>,
    grant: &Grant,
    role: LinkRole,
    remote: Option<&P>,
) -> Patterns {
    publications
        .into_iter()
        .filter(|(path, audience)| visible(path, audience, grant, role, remote))
        .filter_map(|(path, _)| Pattern::literal(path).ok())
        .collect()
}

/// Reports whether a publication at `path` for `audience` belongs on a link.
fn visible<P: PeerId>(
    path: &str,
    audience: &AudienceKind<P>,
    grant: &Grant,
    role: LinkRole,
    remote: Option<&P>,
) -> bool {
    if !grant.allows_subscribe(path) {
        return false;
    }
    match (audience, role) {
        (AudienceKind::Everyone, LinkRole::Direct) => true,
        (AudienceKind::Everyone, LinkRole::Relay { public }) => public,
        (AudienceKind::Peers(peers), LinkRole::Direct) => {
            remote.is_some_and(|remote| peers.contains(remote))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_splits_its_path_and_query() {
        let request = SessionRequest::new(
            "/studio?jwt=abc%2Bdef&x=1",
            vec![("Authorization".into(), "Bearer t".into())],
        );
        assert_eq!(request.path(), "studio");
        assert_eq!(request.query("jwt"), Some("abc+def"));
        assert_eq!(request.query("x"), Some("1"));
        assert_eq!(request.query("y"), None);
        assert_eq!(request.header("authorization"), Some("Bearer t"));
    }

    #[test]
    fn grants_match_patterns() {
        let everything = Grant::everything();
        assert!(everything.allows_subscribe("live/x/cam"));
        assert!(everything.allows_publish("anything"));
        let only = Grant::subscribe_only();
        assert!(only.allows_subscribe("anything"));
        assert!(!only.allows_publish("anything"));
        let live: Pattern = "live/**".parse().expect("pattern");
        let scoped = Grant {
            subscribe: Patterns::from(live),
            publish: Patterns::new(),
        };
        assert!(scoped.allows_subscribe("live/x/cam"));
        assert!(!scoped.allows_subscribe("rooms/t/x/cam"));
        assert!(!scoped.allows_publish("live/x/cam"));
    }

    #[test]
    fn visibility_follows_grant_audience_and_link() {
        let g = Grant::everything();
        let everyone = AudienceKind::<String>::Everyone;
        let peers = AudienceKind::Peers(BTreeSet::from(["a".to_string()]));
        let direct = LinkRole::Direct;
        let a = "a".to_string();
        let b = "b".to_string();
        assert!(visible("p", &everyone, &g, direct, Some(&b)));
        assert!(visible("p", &peers, &g, direct, Some(&a)));
        assert!(!visible("p", &peers, &g, direct, Some(&b)));
        assert!(!visible("p", &peers, &g, direct, None));
        assert!(!visible(
            "p",
            &everyone,
            &g,
            LinkRole::Relay { public: false },
            None
        ));
        assert!(visible(
            "p",
            &everyone,
            &g,
            LinkRole::Relay { public: true },
            None
        ));
        assert!(!visible(
            "p",
            &peers,
            &g,
            LinkRole::Relay { public: true },
            Some(&a)
        ));
        let none = Grant {
            subscribe: Patterns::new(),
            publish: Patterns::new(),
        };
        assert!(!visible("p", &everyone, &none, direct, Some(&a)));
    }

    #[test]
    fn visible_paths_list_exactly_what_a_link_sees() {
        let everyone = AudienceKind::<String>::Everyone;
        let peers = AudienceKind::Peers(BTreeSet::from(["a".to_string()]));
        let publications = [("live/x/public", &everyone), ("live/x/secret", &peers)];
        let grant = Grant::everything();
        let (a, b) = ("a".to_string(), "b".to_string());

        let for_a = visible_paths(publications, &grant, LinkRole::Direct, Some(&a));
        assert!(for_a.matches("live/x/public"));
        assert!(for_a.matches("live/x/secret"));
        assert!(
            !for_a.matches("live/x/public/below"),
            "a literal, not a subtree"
        );
        assert!(
            !for_a.matches("live/y/learned"),
            "a path nobody published here"
        );

        let for_b = visible_paths(publications, &grant, LinkRole::Direct, Some(&b));
        assert!(for_b.matches("live/x/public"));
        assert!(!for_b.matches("live/x/secret"));

        let public = LinkRole::Relay { public: true };
        let relay = visible_paths(publications, &grant, public, None);
        assert!(relay.matches("live/x/public"));
        assert!(!relay.matches("live/x/secret"));

        let private = LinkRole::Relay { public: false };
        assert!(visible_paths(publications, &grant, private, None).is_empty());
    }
}
