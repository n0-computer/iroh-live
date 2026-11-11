//! Access control for moq nodes, independent of the transport.
//!
//! - [`Grant`]: what a session may subscribe to and publish.
//! - [`SessionRequest`]: what a peer asked for when it opened a session.
//! - [`Audience`]: who may see a publication, generic over the peer id.
//! - [`AudienceKind`]: an [`Audience`] as a registry keeps it, the current set.
//! - [`visible_paths`]: the paths a link may see, given its [`LinkRole`].
//! - [`policy`]: who may hold a session, and with which [`Grant`].
//! - [`Layout`]: a path convention, which says where each peer publishes and
//!   who owns a path.
//!
//! A conversion to and from `moq_auth::Grant` is left out on purpose:
//! moq-auth carries the JWT and crypto stack, which this crate stays free of.

use std::{
    collections::BTreeSet,
    fmt::{Debug, Display},
    hash::Hash,
    sync::Arc,
};

use moq_net::{Pattern, Patterns};
use n0_error::stack_error;

pub mod policy;

/// An error returned when no pattern names exactly one path.
#[stack_error(derive)]
#[error("no pattern names exactly the path {path:?}")]
pub struct InvalidPath {
    /// The path that was given.
    pub path: String,
    #[error(source, std_err)]
    source: moq_net::InvalidPattern,
}

/// A peer's identity: any small, comparable, printable id.
pub trait PeerId: Clone + Eq + Ord + Hash + Debug + Display + Send + Sync + 'static {}

impl<T: Clone + Eq + Ord + Hash + Debug + Display + Send + Sync + 'static> PeerId for T {}

/// What a session may subscribe to from a node, and publish into it.
///
/// The shape of a moq-auth grant, so a token means the same thing to a node
/// and to a moq relay. Patterns are moq's: `*` matches one segment, `**` a
/// subtree, anything else itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

    /// Returns a grant that allows nothing in either direction.
    #[must_use]
    pub fn nothing() -> Self {
        Self {
            subscribe: Patterns::new(),
            publish: Patterns::new(),
        }
    }

    /// Returns this grant with the literal `path` added to what the peer may publish.
    ///
    /// # Errors
    ///
    /// Fails with [`InvalidPath`] if no pattern names exactly `path`: a segment
    /// holds `*`, or the path has too many segments. moq patterns have no
    /// escape for `*`.
    pub fn with_publish_path(mut self, path: &str) -> Result<Self, InvalidPath> {
        let pattern = Pattern::literal(path).map_err(|source| InvalidPath {
            path: path.to_owned(),
            source,
        })?;
        self.publish.insert(pattern);
        Ok(self)
    }

    /// Returns a grant that allows what either this grant or `other` allows.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut union = self.clone();
        union.subscribe.extend(other.subscribe.iter().cloned());
        union.publish.extend(other.publish.iter().cloned());
        union
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

/// The wire convention for the token a dialing peer presents.
///
/// The SETUP of moq-lite-06 and older has no token field, so a token also
/// rides the `jwt` query of the setup path.
pub mod token {
    use moq_net::setup::Token;

    /// Returns `token` as the MoQ SETUP carries it.
    #[must_use]
    pub fn setup_token(token: &str) -> Token {
        Token {
            kind: Token::OUT_OF_BAND,
            value: token.as_bytes().to_vec(),
        }
    }

    /// Returns the setup target for `path`, with `token` in its `jwt` query.
    ///
    /// `path` is a URL path, without its query. Escapes it already holds stay
    /// as they are. Characters a URL path cannot hold, such as a space or
    /// `?`, are percent-encoded.
    #[must_use]
    pub fn setup_url(path: &str, token: Option<&str>) -> String {
        let mut url = url::Url::parse("moqt://setup/").expect("a valid URL");
        url.set_path(&format!("/{}", path.trim_start_matches('/')));
        if let Some(token) = token {
            url.query_pairs_mut().append_pair("jwt", token);
        }
        match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        }
    }
}

/// What a peer asked for when it opened a session.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    path: String,
    query: Vec<(String, String)>,
    token: Option<String>,
}

impl SessionRequest {
    /// Parses a request target, `/<path>?<query>`.
    #[must_use]
    pub fn new(target: &str) -> Self {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = url::form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        Self {
            path: path.trim_matches('/').to_owned(),
            query,
            token: None,
        }
    }

    /// Returns what a client asked for in its MoQ SETUP: a path, and maybe a token.
    #[must_use]
    pub fn from_handshake(handshake: &moq_net::server::Handshake) -> Self {
        let request = Self::new(handshake.path());
        let token = handshake
            .token()
            .filter(|token| token.kind == moq_net::setup::Token::OUT_OF_BAND)
            .and_then(|token| std::str::from_utf8(&token.value).ok());
        match token {
            Some(token) => request.with_token(token),
            None => request,
        }
    }

    /// Sets the token the peer presented outside the target, such as in its MoQ SETUP.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Returns the request path, without its query.
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

    /// Returns the token the peer presented.
    ///
    /// That is the token set with [`with_token`](Self::with_token), or else the
    /// `jwt` query parameter, or else the `token` one. Writers use `jwt`, which
    /// moq-relay reads.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.token
            .as_deref()
            .or_else(|| self.query("jwt"))
            .or_else(|| self.query("token"))
    }
}

/// Who may see a publication.
#[derive(Debug, Clone)]
pub enum Audience<P: PeerId> {
    /// Every session whose grant covers the path, relays included.
    Granted,
    /// These peers, as the set changes.
    ///
    /// Never offered to relays. The publication holds the set: change it
    /// through a clone of the watchable.
    Peers(n0_watcher::Watchable<BTreeSet<P>>),
}

impl<P: PeerId> Audience<P> {
    /// Returns an audience of `peers`, as a set nobody else holds.
    ///
    /// ```
    /// use moq_ext_access::Audience;
    ///
    /// let only_bob: Audience<&str> = Audience::peers(["bob"]);
    /// ```
    #[must_use]
    pub fn peers(peers: impl IntoIterator<Item = P>) -> Self {
        Self::Peers(n0_watcher::Watchable::new(peers.into_iter().collect()))
    }
}

#[allow(
    clippy::derivable_impls,
    reason = "a derived Default would require `P: Default`, which peer ids such as iroh's EndpointId are not"
)]
impl<P: PeerId> Default for Audience<P> {
    fn default() -> Self {
        Self::Granted
    }
}

/// An [`Audience`] as a registry keeps it: the current set, not its watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudienceKind<P: PeerId> {
    /// See [`Audience::Granted`].
    Granted,
    /// See [`Audience::Peers`], as of now.
    Peers(BTreeSet<P>),
}

impl<P: PeerId> From<&Audience<P>> for AudienceKind<P> {
    fn from(audience: &Audience<P>) -> Self {
        match audience {
            Audience::Granted => Self::Granted,
            Audience::Peers(peers) => Self::Peers(peers.get()),
        }
    }
}

/// What kind of link a publication may be offered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRole {
    /// A session with a peer.
    Direct,
    /// A moq relay, which gets the [`Audience::Granted`] publications.
    Relay,
}

/// A path convention: where each peer publishes, and which peer owns a path.
///
/// The two halves agree: [`owner`](Self::owner) of a path that
/// [`scope`](Self::scope) gives a peer names that peer.
///
/// A pair `(A, B)` is the union of two layouts.
pub trait Layout<P: PeerId>: Debug + Send + Sync + 'static {
    /// Returns the paths `peer` may publish its own broadcasts under.
    fn scope(&self, peer: &P) -> Patterns;

    /// Returns the peer that owns `path`, or `None` for a path outside the layout.
    fn owner(&self, path: &str) -> Option<P>;
}

impl<P: PeerId, T: Layout<P> + ?Sized> Layout<P> for Arc<T> {
    fn scope(&self, peer: &P) -> Patterns {
        (**self).scope(peer)
    }

    fn owner(&self, path: &str) -> Option<P> {
        (**self).owner(path)
    }
}

impl<P: PeerId, A: Layout<P>, B: Layout<P>> Layout<P> for (A, B) {
    fn scope(&self, peer: &P) -> Patterns {
        let mut scope = self.0.scope(peer);
        scope.extend(self.1.scope(peer).iter().cloned());
        scope
    }

    fn owner(&self, path: &str) -> Option<P> {
        self.0.owner(path).or_else(|| self.1.owner(path))
    }
}

/// Returns the exact paths of `publications` a link may see, as patterns.
///
/// A publication is visible when the link's `grant` covers its path and its
/// audience admits the link. A relay sees `Granted` publications, and never a
/// `Peers` one, which a relay would forward to anyone. A direct link is in a
/// `Peers` audience when its `remote` is in the set.
///
/// Each path is a literal pattern, so nothing beside the listed publications
/// matches. A path no pattern can spell is left out.
///
/// moq bounds the result. It refuses an AUTH_OK larger than 64 KiB, and caps
/// a union of patterns at [`Pattern::MAX_INTERSECTIONS`] (1024) members. A
/// link that may see more publications than that, or whose paths add up to
/// more bytes, cannot be granted them one by one.
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
        (AudienceKind::Granted, _) => true,
        (AudienceKind::Peers(peers), LinkRole::Direct) => {
            remote.is_some_and(|remote| peers.contains(remote))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout that gives each peer `<prefix>/<peer>/**`.
    #[derive(Debug)]
    struct Prefix(&'static str);

    impl Layout<String> for Prefix {
        fn scope(&self, peer: &String) -> Patterns {
            Patterns::from(
                format!("{}/{peer}/**", self.0)
                    .parse::<Pattern>()
                    .expect("pattern"),
            )
        }

        fn owner(&self, path: &str) -> Option<String> {
            let rest = path.strip_prefix(self.0)?.strip_prefix('/')?;
            let (peer, name) = rest.split_once('/')?;
            (!name.is_empty()).then(|| peer.to_owned())
        }
    }

    #[test]
    fn a_pair_of_layouts_is_their_union() {
        let both = (Prefix("live"), Arc::new(Prefix("rooms")));
        let bob = "bob".to_owned();
        let scope = both.scope(&bob);
        assert!(scope.matches("live/bob/cam"));
        assert!(scope.matches("rooms/bob/cam"));
        assert!(!scope.matches("live/carol/cam"));
        assert_eq!(both.owner("live/bob/cam"), Some(bob.clone()));
        assert_eq!(both.owner("rooms/bob/cam"), Some(bob));
        assert_eq!(both.owner("other/bob/cam"), None);
    }

    #[test]
    fn a_request_splits_its_path_and_query() {
        let request = SessionRequest::new("/studio?jwt=abc%2Bdef&x=1");
        assert_eq!(request.path(), "studio");
        assert_eq!(request.query("jwt"), Some("abc+def"));
        assert_eq!(request.query("x"), Some("1"));
        assert_eq!(request.query("y"), None);
    }

    #[test]
    fn a_setup_token_wins_over_the_query() {
        let request = SessionRequest::new("/studio?jwt=from-query");
        assert_eq!(request.token(), Some("from-query"));
        let request = request.with_token("from-setup");
        assert_eq!(request.token(), Some("from-setup"));
        assert_eq!(SessionRequest::new("/studio").token(), None);
    }

    #[test]
    fn a_token_rides_the_setup_path() {
        let target = token::setup_url("", Some("a+b"));
        assert_eq!(target, "/?jwt=a%2Bb");
        assert_eq!(SessionRequest::new(&target).token(), Some("a+b"));
        assert_eq!(token::setup_token("a+b").value, b"a+b");
    }

    #[test]
    fn a_setup_url_joins_path_and_token() {
        assert_eq!(token::setup_url("", None), "/");
        assert_eq!(token::setup_url("studio/cam", None), "/studio/cam");
        assert_eq!(token::setup_url("/studio", None), "/studio");
        assert_eq!(token::setup_url("studio", Some("abc")), "/studio?jwt=abc");
        let request = SessionRequest::new(&token::setup_url("studio", Some("abc")));
        assert_eq!(request.path(), "studio");
        assert_eq!(request.token(), Some("abc"));
    }

    #[test]
    fn a_setup_url_encodes_what_a_path_cannot_hold() {
        assert_eq!(token::setup_url("my cam?x#y", None), "/my%20cam%3Fx%23y");
        // An escape the path already holds is not encoded twice.
        assert_eq!(token::setup_url("my%20cam", None), "/my%20cam");
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
    fn a_publish_path_with_a_wildcard_is_an_error() {
        for path in ["a/*/b", "a/**", "a/b*c"] {
            let err = Grant::nothing()
                .with_publish_path(path)
                .expect_err("a path with a wildcard");
            assert_eq!(err.path, path);
        }
    }

    #[test]
    fn nothing_and_union_compose() {
        let nothing = Grant::nothing();
        assert!(!nothing.allows_publish("a/b") && !nothing.allows_subscribe("a/b"));
        let one = Grant::nothing()
            .with_publish_path("a/b")
            .expect("a literal path");
        assert!(one.allows_publish("a/b") && !one.allows_publish("a/b/c"));
        let both = Grant::subscribe_only().union(&one);
        assert!(both.allows_publish("a/b") && both.allows_subscribe("x"));
        assert!(!both.allows_publish("x"));
    }

    #[test]
    fn visibility_follows_grant_audience_and_link() {
        let grant = Grant::everything();
        let granted = AudienceKind::<String>::Granted;
        let peers = AudienceKind::Peers(BTreeSet::from(["a".to_string()]));
        let direct = LinkRole::Direct;
        let a = "a".to_string();
        let b = "b".to_string();
        assert!(visible("p", &granted, &grant, direct, Some(&b)));
        assert!(visible("p", &peers, &grant, direct, Some(&a)));
        assert!(!visible("p", &peers, &grant, direct, Some(&b)));
        assert!(!visible("p", &peers, &grant, direct, None));
        assert!(visible("p", &granted, &grant, LinkRole::Relay, None));
        assert!(!visible("p", &peers, &grant, LinkRole::Relay, Some(&a)));
        let none = Grant {
            subscribe: Patterns::new(),
            publish: Patterns::new(),
        };
        assert!(!visible("p", &granted, &none, direct, Some(&a)));
    }

    #[test]
    fn visible_paths_list_exactly_what_a_link_sees() {
        let granted = AudienceKind::<String>::Granted;
        let peers = AudienceKind::Peers(BTreeSet::from(["a".to_string()]));
        let publications = [("live/x/public", &granted), ("live/x/secret", &peers)];
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

        let relay = visible_paths(publications, &grant, LinkRole::Relay, None);
        assert!(relay.matches("live/x/public"));
        assert!(!relay.matches("live/x/secret"));
    }
}
