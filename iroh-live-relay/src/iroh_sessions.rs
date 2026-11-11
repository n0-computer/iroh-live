//! Sessions from iroh clients, decided by a policy on their endpoint id.
//!
//! moq-tokio's server has no iroh backend, and an iroh session's auth request
//! would carry no peer identity anyway. So the relay accepts iroh sessions
//! itself with [`IrohSessions`], and asks a [`Policy`] about each. The
//! default, [`relay_policy`], lets each client publish only at the paths that
//! name it: `live/<its id>/...` and `rooms/<topic>/<its id>/...`. Browsers
//! come in through moq-relay's server, and [`browser_grant`] keeps them to
//! names of one segment below the root.

use std::{sync::Arc, time::Duration};

use iroh::{
    Endpoint, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_live::{layout::LiveLayout, rooms::RoomsLayout};
use iroh_live_node::{
    DECIDE_TIMEOUT, SessionRequest,
    policy::{Authorization, DynPolicy, Event, PeerRequest, Policy},
};
use moq_net::Pattern;
use moq_relay::cluster::Cluster;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::pull::PullState;

/// How long a closing session gets to tell its peer when the relay shuts down.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// Accepts iroh clients' MoQ sessions into the cluster.
///
/// Mount it under every ALPN in [`moq_transport_iroh::alpns`], or use
/// [`router`](Self::router).
#[derive(Clone, derive_more::Debug)]
pub struct IrohSessions {
    #[debug(skip)]
    cluster: Cluster,
    /// Pulls the broadcasts that sessions name by ticket, if enabled.
    pulls: Option<Arc<PullState>>,
    /// Decides who may hold a session, and what it may publish and read.
    policy: Arc<dyn DynPolicy<EndpointId>>,
    /// Cancelled when the router shuts down, which closes every session.
    shutdown: CancellationToken,
}

impl IrohSessions {
    /// Creates the handler for `cluster`, deciding sessions with [`relay_policy`].
    ///
    /// With `pulls` set, a session that names a ticket in its path pulls the
    /// ticket's broadcast, as a browser's does.
    #[must_use]
    pub fn new(cluster: Cluster, pulls: Option<Arc<PullState>>) -> Self {
        Self {
            cluster,
            pulls,
            policy: Arc::new(relay_policy()),
            shutdown: CancellationToken::new(),
        }
    }

    /// Returns this handler with `policy` deciding sessions.
    ///
    /// The relay asks it after a client's MoQ SETUP. A rejection closes the
    /// session with the rejection's code. An authorization's grant bounds
    /// what the client publishes into the cluster and what it reads from it,
    /// and the session closes when the authorization expires. The relay does
    /// not decide again: a client whose session closed reconnects.
    #[must_use]
    pub fn with_policy(mut self, policy: impl Policy<EndpointId>) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Spawns a router on `endpoint` with this handler.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn router(self, endpoint: Endpoint) -> Router {
        let mut router = Router::builder(endpoint);
        for alpn in moq_transport_iroh::alpns() {
            router = router.accept(alpn, self.clone());
        }
        router.spawn()
    }

    /// Runs one session until it closes or the relay shuts down.
    async fn serve(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = connection.remote_id();
        let transport = moq_transport_iroh::accept(connection).map_err(AcceptError::from_err)?;
        let handshake = tokio::time::timeout(
            moq_transport_iroh::HANDSHAKE_TIMEOUT,
            moq_net::Server::new().accept_request(moq_net::time::Instant::now(), transport),
        )
        .await
        .map_err(|_| AcceptError::from_err(moq_net::Error::Timeout))?
        .map_err(AcceptError::from_err)?;
        let name = requested_name(handshake.path());
        // Logged without the query: a token rides it as `?jwt=`.
        let session = SessionRequest::from_handshake(&handshake);
        let path = session.path().to_owned();
        let request = PeerRequest::new(peer, session, false, Event::Connect);
        let authorization =
            match moq_ext_access::policy::decide_within(&*self.policy, &request, DECIDE_TIMEOUT)
                .await
            {
                Ok(authorization) => authorization,
                Err(rejection) => {
                    info!(
                        reason = rejection.reason(),
                        code = %rejection.code(),
                        "the policy rejected the iroh client",
                    );
                    handshake.close(rejection.code().clone());
                    return Err(AcceptError::from_err(rejection));
                }
            };
        let grant = &authorization.grant;
        let publish = self
            .cluster
            .origin
            .scope("", &grant.publish)
            .map_err(AcceptError::from_err)?;
        // A session that names a ticket reads the pulled broadcast at that name.
        let serving = name
            .as_deref()
            .filter(|_| self.pulls.is_some())
            .and_then(|name| crate::pull::mounted(&self.cluster, name))
            .map_or_else(
                || self.cluster.origin.consume(),
                |cluster| cluster.origin.consume(),
            )
            .scope("", &grant.subscribe)
            .map_err(AcceptError::from_err)?;
        let (session, driver) = tokio::time::timeout(
            moq_transport_iroh::HANDSHAKE_TIMEOUT,
            handshake
                .with_publisher(serving)
                .with_subscriber(publish)
                .ok(),
        )
        .await
        .map_err(|_| AcceptError::from_err(moq_net::Error::Timeout))?
        .map_err(AcceptError::from_err)?;
        info!(%path, grant = ?authorization.grant, expires = ?authorization.expires, "iroh client admitted");

        // Held while the session runs.
        let _pull = self
            .pulls
            .as_ref()
            .and_then(|pulls| crate::pull_for(pulls.clone(), name.clone()?));

        let run = moq_net::time::run(driver);
        tokio::pin!(run);
        let expiry = expiry(&authorization);
        tokio::pin!(expiry);
        let close = tokio::select! {
            err = &mut run => {
                debug!(error = %err, "iroh session ended");
                return Ok(());
            }
            () = self.shutdown.cancelled() => moq_net::Error::Cancel,
            () = &mut expiry => {
                info!("the iroh client's authorization expired, closing its session");
                moq_net::Error::Expired
            }
        };
        // The driver keeps running, so the session can tell its peer.
        session.abort(close);
        if tokio::time::timeout(CLOSE_GRACE, run).await.is_err() {
            warn!(grace = ?CLOSE_GRACE, "iroh session did not close in time");
        }
        Ok(())
    }
}

/// Waits until `authorization` expires, or forever without an expiry.
async fn expiry(authorization: &Authorization) {
    match authorization.expires {
        Some(expires) => {
            let left = expires
                .duration_since(std::time::SystemTime::now())
                .unwrap_or_default();
            tokio::time::sleep(left).await;
        }
        None => std::future::pending().await,
    }
}

impl ProtocolHandler for IrohSessions {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let span = info_span!("iroh_client", peer = %connection.remote_id().fmt_short());
        self.serve(connection).instrument(span).await
    }

    async fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Returns the grant for a browser that dialed `path`: subscribe anywhere, publish one segment.
///
/// The rule is rooted at `/` and reduced to the dialed path, as moq-relay does
/// with its public rules. The grant itself is rooted at the dialed path, so
/// granting the unreduced rule would let a browser that dials `/live/<id>`
/// publish beneath it. One segment keeps a browser out of `live/` and
/// `rooms/`, whose paths name an iroh publisher.
///
/// # Errors
///
/// Returns an error if `path` is not a valid path.
pub fn browser_grant(path: &str) -> Result<moq_auth::Grant, moq_auth::Error> {
    let rule = moq_auth::Claims::default()
        .with_publish(["*".parse::<Pattern>().expect("a valid pattern")])
        .with_subscribe([Pattern::all()]);
    let permissions = rule.authorize(path)?;
    Ok(moq_auth::Grant::new(
        permissions.publish,
        permissions.subscribe,
    ))
}

/// Returns the broadcast name a session asked for in its path, if any.
///
/// A URL carries the name percent-encoded, so `/my%20cam` asks for `my cam`.
/// A name that does not decode to UTF-8 is none.
pub(crate) fn requested_name(path: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or_default();
    // A ticket is read from the raw path: its own escapes are part of it, and
    // decoding them first would change what it names.
    let raw = path.trim_start_matches('/');
    if raw.parse::<iroh_live::BroadcastTicket>().is_ok() {
        return Some(raw.to_owned());
    }
    let name = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    (!name.is_empty()).then(|| name.into_owned())
}

/// Returns the relay's default policy for iroh clients.
///
/// It admits every client to read the whole cluster and to publish its own
/// `live/` broadcasts and its room broadcasts, at the paths that name its
/// endpoint id. Use it as the fallback of a [`Verify`](moq_ext_access::policy::Verify)
/// to let a token widen what a client may publish.
#[must_use]
pub fn relay_policy() -> impl Policy<EndpointId> + Clone {
    LiveLayout::default().policy(&[Arc::new(RoomsLayout)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_publishes_only_under_its_own_id() {
        let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[6; 32]).public();
        let request = PeerRequest::new(id, SessionRequest::default(), false, Event::Connect);
        let scope = n0_future::future::block_on(Policy::decide(&relay_policy(), &request))
            .expect("admitted")
            .grant
            .publish;
        assert!(scope.matches(&format!("live/{id}/cam")));
        assert!(scope.matches(&format!("rooms/topic/{id}/cam")));
        assert!(!scope.matches(&format!("live/{other}/cam")));
        assert!(!scope.matches(&format!("rooms/topic/{other}/cam")));
        assert!(!scope.matches("cam"));
    }

    /// Returns whether [`browser_grant`] lets a browser at the root publish at `path`.
    fn a_browser_may_publish(path: &str) -> bool {
        browser_grant("/").expect("a grant").publish.matches(path)
    }

    #[test]
    fn a_browser_publishes_one_segment_names() {
        assert!(a_browser_may_publish("hello"));
        assert!(!a_browser_may_publish("live/someone/cam"));
        assert!(!a_browser_may_publish("rooms/topic/someone/cam"));
    }

    /// The grant is rooted at the dialed path, so its publish half must not reach below it.
    #[test]
    fn a_browser_grant_is_reduced_to_the_dialed_path() {
        let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
        for dialed in [
            format!("/live/{peer}"),
            format!("/rooms/topic/{peer}"),
            "/live".to_owned(),
        ] {
            let grant = browser_grant(&dialed).expect("a grant");
            assert!(
                !grant.publish.matches("cam"),
                "a browser at {dialed} may publish cam beneath it"
            );
            assert!(grant.subscribe.matches("cam"), "{dialed}");
        }
        // A one-segment path is itself the one segment a browser may publish.
        let grant = browser_grant("/hello").expect("a grant");
        assert!(grant.publish.matches(""));
        assert!(!grant.publish.matches("cam"));
    }

    /// A browser that dials a ticket reads it, and publishes nothing beneath it.
    #[test]
    fn a_browser_at_a_ticket_path_publishes_nothing() {
        let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let path = LiveLayout::default().path(peer, "cam").expect("a path");
        let ticket = iroh_live::BroadcastTicket::at(peer, path);
        let grant = browser_grant(&format!("/{ticket}")).expect("a grant");
        assert!(grant.publish.is_empty());
        assert!(grant.subscribe.matches(""));
    }

    /// A browser cannot publish at the name a pull is read under.
    ///
    /// Pulls are read at the ticket's spelling, which always has a `/` between
    /// the endpoint id and the name.
    #[test]
    fn a_browser_cannot_publish_at_a_pulled_name() {
        let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let path = LiveLayout::default().path(peer, "cam").expect("a path");
        let ticket = iroh_live::BroadcastTicket::at(peer, path);
        let full = ticket.to_string();
        let bare = full.strip_prefix("ilv1:").expect("a ticket prefix");
        assert!(!a_browser_may_publish(&full));
        assert!(!a_browser_may_publish(bare));
    }

    #[test]
    fn a_requested_name_drops_the_slash_and_the_query() {
        assert_eq!(requested_name("/abc?jwt=x").as_deref(), Some("abc"));
        assert_eq!(requested_name(""), None);
        assert_eq!(requested_name("/"), None);
    }

    #[test]
    fn a_requested_name_is_percent_decoded() {
        assert_eq!(requested_name("/my%20cam").as_deref(), Some("my cam"));
        assert_eq!(
            requested_name("/caf%C3%A9?jwt=x").as_deref(),
            Some("caf\u{e9}")
        );
        assert_eq!(requested_name("/100%25").as_deref(), Some("100%"));
        assert_eq!(requested_name("/%FF"), None);
    }
}
