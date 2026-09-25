//! Admission: who gets a session, and what it may do once it has one.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use iroh::{EndpointId, endpoint::Connection};
use moq_net::{Pattern, Patterns, server::Handshake};
use n0_error::e;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::{
    Error, Session,
    node::Shared,
    session::{Origins, SessionParts, Transport, driver_now, hop_for},
    transport::accept_transport,
};

/// A function that gives a peer its grant, from its endpoint id.
pub type GrantFn = Arc<dyn Fn(EndpointId) -> Grant + Send + Sync>;

/// How many incoming sessions may wait for [`Moq::accept`] at once.
///
/// Past this, new sessions wait up to [`ADMISSION_TIMEOUT`] for room.
pub(crate) const INCOMING_QUEUE: usize = 16;

/// How long an incoming connection may take to open its MoQ session.
///
/// Without a bound, a peer that never sends its setup holds a task for as long
/// as the connection lives.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an incoming session waits for room in the admission queue.
///
/// Keeps a stalled accept loop from piling up connections whose peers think
/// they are connected.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);

/// How a node treats incoming sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Admission {
    /// Admits every session with the grant [`MoqConfig::grant`] gives its peer.
    ///
    /// [`MoqConfig::grant`]: crate::MoqConfig::grant
    #[default]
    Open,
    /// Hands every incoming session to [`Moq::accept`](crate::Moq::accept) to decide.
    Manual,
}

/// What a session may subscribe to from this node, and publish into it.
///
/// The shape of a moq-auth grant, so a token means the same thing to this node
/// and to a moq relay. Patterns are moq's: `*` matches one segment, `**` a
/// subtree, anything else itself.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Returns the grant verified moq-auth claims describe.
    ///
    /// Roots every pattern at the claims' root. A pattern that cannot be
    /// rooted, because root and pattern together exceed moq's path depth, is
    /// dropped.
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
    /// Returns [`Grant::everything`].
    fn default() -> Self {
        Self::everything()
    }
}

/// What a peer asked for when it opened a session.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    role: Option<moq_net::Role>,
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
    /// Always `None` on raw sessions.
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
    pub fn role(&self) -> Option<moq_net::Role> {
        self.role
    }
}

/// How to dial a peer with [`Moq::connect_with`](crate::Moq::connect_with).
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// A token to present, sent as `?jwt=` in the setup path or the CONNECT URL.
    pub token: Option<String>,
    /// The cost added to every route learned over this link.
    ///
    /// `None` keeps moq's default of one.
    pub cost: Option<u64>,
    /// What the dialed peer may do on this node.
    ///
    /// `None` takes it from [`MoqConfig::grant`](crate::MoqConfig::grant).
    pub grant: Option<Grant>,
}

impl ConnectOptions {
    /// Returns the setup path that carries the token, if there is one.
    pub(crate) fn setup_path(&self) -> Option<String> {
        let token = self.token.as_ref()?;
        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("jwt", token)
            .finish();
        Some(format!("/?{query}"))
    }
}

/// An incoming session waiting for admission.
///
/// Yielded by [`Moq::accept`](crate::Moq::accept) under
/// [`Admission::Manual`](crate::Admission::Manual). Dropping it without
/// admitting rejects the session.
#[derive(derive_more::Debug)]
pub struct Incoming {
    #[debug("{}", remote.fmt_short())]
    pub(crate) remote: EndpointId,
    pub(crate) request: SessionRequest,
    #[debug(skip)]
    pub(crate) connection: Connection,
    #[debug(skip)]
    pub(crate) handshake: Handshake<Transport>,
    #[debug(skip)]
    pub(crate) shared: Weak<Shared>,
}

impl Incoming {
    /// Returns the peer's endpoint id, which iroh has authenticated.
    pub fn remote_id(&self) -> EndpointId {
        self.remote
    }

    /// Returns what the peer asked for.
    pub fn request(&self) -> &SessionRequest {
        &self.request
    }

    /// Admits the session with `grant`.
    ///
    /// Dropping the future before the handshake completes rejects the session.
    ///
    /// # Errors
    ///
    /// Fails if the MoQ handshake fails, and with [`Error::ShutDown`] once the
    /// node has shut down, which refuses the peer.
    pub async fn admit(self, grant: Grant) -> Result<Session, Error> {
        let Some(shared) = self
            .shared
            .upgrade()
            .filter(|shared| !shared.shutdown.is_cancelled())
        else {
            self.handshake.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        };
        info!(remote = %self.remote.fmt_short(), ?grant, "admitting session");
        let origins = Origins::new(&shared);
        let mut handshake = self
            .handshake
            .with_publisher(origins.publish.consume())
            .with_peer_hop(hop_for(&self.remote));
        if let Some(subscriber) = origins.subscriber(&grant) {
            handshake = handshake.with_subscriber(subscriber);
        }
        let (moq, driver) = handshake
            .ok()
            .await
            .map_err(|source| e!(Error::Moq { source }))?;
        let parts = SessionParts {
            remote: self.remote,
            connection: self.connection,
            dialed: false,
            grant,
            request: self.request,
            moq,
            driver,
            origins,
        };
        shared.register(parts).await
    }

    /// Rejects the session with `reason`, such as [`moq_net::Error::Unauthorized`].
    pub fn reject(self, reason: moq_net::Error) {
        info!(remote = %self.remote.fmt_short(), %reason, "rejecting session");
        self.handshake.close(reason);
    }
}

/// Admits or queues one incoming connection, for the protocol handler.
pub(crate) async fn accept(shared: &Arc<Shared>, connection: Connection) -> Result<(), Error> {
    if shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let remote = connection.remote_id();
    let opened = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let (transport, h3) = accept_transport(connection.clone()).await?;
        let handshake = moq_net::Server::new()
            .accept_request(driver_now(), Transport::new(transport))
            .await
            .map_err(|source| e!(Error::Moq { source }))?;
        Ok::<_, Error>((h3, handshake))
    })
    .await;
    let (h3, handshake) = match opened {
        Ok(opened) => opened?,
        Err(_) => {
            debug!(remote = %remote.fmt_short(), "no session setup in time");
            return Err(e!(Error::Moq {
                source: moq_net::Error::Timeout
            }));
        }
    };
    let request = match h3 {
        Some((target, headers)) => SessionRequest::new(&target, headers, handshake.role()),
        None => SessionRequest::new(handshake.path(), Vec::new(), handshake.role()),
    };
    debug!(remote = %remote.fmt_short(), path = request.path(), "session requested");
    let incoming = Incoming {
        remote,
        request,
        connection,
        handshake,
        shared: Arc::downgrade(shared),
    };
    match shared.admission {
        Admission::Open => {
            incoming.admit(shared.grant_for(remote)).await?;
        }
        Admission::Manual => {
            let room = tokio::select! {
                room = tokio::time::timeout(
                    ADMISSION_TIMEOUT,
                    shared.incoming_tx.reserve(),
                ) => room,
                _ = shared.shutdown.cancelled() => Ok(Err(mpsc::error::SendError(()))),
            };
            match room {
                Ok(Ok(permit)) => permit.send(incoming),
                Ok(Err(_)) => {
                    incoming.handshake.close(moq_net::Error::Cancel);
                    return Err(e!(Error::ShutDown));
                }
                Err(_) => {
                    info!(remote = %remote.fmt_short(), "admission queue full, rejecting");
                    incoming.handshake.close(moq_net::Error::Timeout);
                    return Err(e!(Error::Moq {
                        source: moq_net::Error::Timeout
                    }));
                }
            }
        }
    }
    Ok(())
}

/// Waits for the next session that needs admission.
pub(crate) async fn next(shared: &Shared) -> Option<Incoming> {
    let mut queue = shared.incoming_rx.lock().await;
    tokio::select! {
        // Never hand out a session after the shutdown, even a queued one.
        biased;
        _ = shared.shutdown.cancelled() => None,
        incoming = queue.recv() => incoming,
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
        let options = ConnectOptions {
            token: Some("a+b".into()),
            ..Default::default()
        };
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
        let scoped = Grant {
            subscribe: Patterns::from(live),
            publish: Patterns::new(),
        };
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
