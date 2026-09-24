//! Admission: who gets a session, and what it may do once it has one.

use moq_net::{Pattern, Patterns};

/// How a node treats incoming sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Admission {
    /// Admits every session with [`Grant::everything`].
    #[default]
    Open,
    /// Holds every incoming session until the application decides.
    ///
    /// Sessions wait in [`Moq::accept`](crate::Moq::accept) until the
    /// application admits or rejects them.
    Manual,
}

/// What a session may subscribe to from this node, and publish into it.
///
/// The shape of a moq-auth grant, so a token means the same thing to this node
/// and to a moq relay. Patterns are moq's: `*` matches one segment, `**` a
/// subtree, anything else itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grant {
    /// The paths the peer may subscribe to from this node.
    pub subscribe: Patterns,
    /// The paths the peer may publish into this node's route table.
    pub publish: Patterns,
}

impl Grant {
    /// Returns a grant that allows everything in both directions.
    pub fn everything() -> Self {
        Self {
            subscribe: Patterns::from(Pattern::all()),
            publish: Patterns::from(Pattern::all()),
        }
    }

    /// Returns a grant that allows nothing.
    pub fn nothing() -> Self {
        Self {
            subscribe: Patterns::new(),
            publish: Patterns::new(),
        }
    }

    /// Returns a grant with these subscribe and publish patterns.
    pub fn new(subscribe: Patterns, publish: Patterns) -> Self {
        Self { subscribe, publish }
    }

    /// Returns the grant verified moq-auth claims describe.
    ///
    /// Every pattern is rooted where the claims say. A claim pattern that cannot be rooted (the root and the pattern together
    /// exceed moq's path depth) is dropped, which grants less rather than more.
    #[cfg(feature = "auth")]
    pub fn from_claims(claims: &moq_auth::Claims) -> Self {
        let root = |patterns: &Patterns| -> Patterns {
            patterns
                .iter()
                .filter_map(|pattern| pattern.rooted(&claims.root).ok())
                .collect()
        };
        Self {
            subscribe: root(&claims.subscribe),
            publish: root(&claims.publish),
        }
    }

    /// Reports whether the peer may subscribe to `path`.
    pub fn allows_subscribe(&self, path: &str) -> bool {
        self.subscribe.matches(path)
    }

    /// Reports whether the peer may publish `path`.
    pub fn allows_publish(&self, path: &str) -> bool {
        self.publish.matches(path)
    }
}

impl Default for Grant {
    /// Returns [`Grant::everything`], which is what [`Admission::Open`] gives.
    fn default() -> Self {
        Self::everything()
    }
}

/// Whether a peer said it only publishes or only subscribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// The peer only publishes.
    Publisher,
    /// The peer only subscribes.
    Subscriber,
}

/// Why an incoming session was refused, as the peer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reject {
    /// The peer presented no credential, or one that did not verify.
    Unauthorized,
    /// The credential verified but does not allow what the peer asked for.
    Forbidden,
    /// An application-defined reason code.
    App(u16),
}

impl From<Reject> for moq_net::Error {
    fn from(reject: Reject) -> Self {
        match reject {
            // moq has one code for both; the distinction is for the log here.
            Reject::Unauthorized | Reject::Forbidden => Self::Unauthorized,
            Reject::App(code) => Self::App(code),
        }
    }
}

/// What a peer asked for when it opened a session.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    role: Option<Role>,
}

impl SessionRequest {
    /// Parses a request target, `/<path>?<query>`, and its headers.
    pub(crate) fn new(
        target: &str,
        headers: Vec<(String, String)>,
        role: Option<moq_net::Role>,
    ) -> Self {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = url::form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let role = match role {
            Some(moq_net::Role::Publisher) => Some(Role::Publisher),
            Some(moq_net::Role::Subscriber) => Some(Role::Subscriber),
            _ => None,
        };
        Self {
            path: path.trim_matches('/').to_owned(),
            query,
            headers,
            role,
        }
    }

    /// Returns the request path, without its query.
    ///
    /// The setup path on a raw iroh session, the CONNECT path on an HTTP/3 one.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns a query parameter of the request, such as `jwt`.
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Returns an HTTP/3 CONNECT header, matched case-insensitively.
    ///
    /// Always `None` on raw iroh sessions, which carry no headers.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Returns whether the peer said it only publishes or only subscribes.
    ///
    /// `None` for a peer that does both, and for one whose protocol version
    /// cannot say.
    pub fn role(&self) -> Option<Role> {
        self.role
    }
}

/// How to dial a peer with [`Moq::connect_with`](crate::Moq::connect_with).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConnectOptions {
    /// A token to present, sent as `?jwt=` in the setup path or the CONNECT URL.
    pub token: Option<String>,
    /// The price of this link, added to every route learned over it.
    ///
    /// `None` leaves moq's default of one per hop.
    pub cost: Option<u64>,
    /// What the dialed peer may do on this node.
    ///
    /// Defaults to [`Grant::everything`]: dialing a peer is trusting it.
    pub grant: Grant,
}

impl ConnectOptions {
    /// Presents `token` to the peer.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Prices the link at `cost`.
    pub fn with_cost(mut self, cost: u64) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Limits what the dialed peer may do on this node.
    pub fn with_grant(mut self, grant: Grant) -> Self {
        self.grant = grant;
        self
    }

    /// Returns the setup path that carries the token, if there is one.
    pub(crate) fn setup_path(&self) -> Option<String> {
        let token = self.token.as_ref()?;
        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("jwt", token)
            .finish();
        Some(format!("/?{query}"))
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
            None,
        );
        assert_eq!(request.path(), "studio");
        assert_eq!(request.query("jwt"), Some("abc+def"));
        assert_eq!(request.query("x"), Some("1"));
        assert_eq!(request.query("y"), None);
        assert_eq!(request.header("authorization"), Some("Bearer t"));
    }

    #[test]
    fn a_token_rides_the_setup_path() {
        let options = ConnectOptions::default().with_token("a+b");
        let path = options.setup_path().expect("a path");
        let request = SessionRequest::new(&path, vec![], None);
        assert_eq!(request.query("jwt"), Some("a+b"));
        assert_eq!(ConnectOptions::default().setup_path(), None);
    }

    #[test]
    fn grants_match_patterns() {
        let everything = Grant::everything();
        assert!(everything.allows_subscribe("live/x/cam"));
        assert!(everything.allows_publish("anything"));
        let nothing = Grant::nothing();
        assert!(!nothing.allows_subscribe("live/x/cam"));
        let live: Pattern = "live/**".parse().expect("pattern");
        let scoped = Grant::new(Patterns::from(live), Patterns::new());
        assert!(scoped.allows_subscribe("live/x/cam"));
        assert!(!scoped.allows_subscribe("rooms/t/x/cam"));
        assert!(!scoped.allows_publish("live/x/cam"));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn claims_are_rooted() {
        let claims = moq_auth::Claims::default()
            .with_root("rooms/topic")
            .with_subscribe(["**".parse().expect("pattern")])
            .with_publish(["alice/**".parse().expect("pattern")]);
        let grant = Grant::from_claims(&claims);
        assert!(grant.allows_subscribe("rooms/topic/bob/cam"));
        assert!(!grant.allows_subscribe("rooms/other/bob/cam"));
        assert!(grant.allows_publish("rooms/topic/alice/cam"));
        assert!(!grant.allows_publish("rooms/topic/bob/cam"));
    }
}
