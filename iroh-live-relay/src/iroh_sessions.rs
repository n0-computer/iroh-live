//! Sessions from iroh clients, scoped by their endpoint id.
//!
//! moq-tokio's server has no iroh backend, and an iroh session's auth request
//! would carry no peer identity anyway. So the relay accepts iroh sessions
//! itself with [`IrohSessions`]. It lets each client publish only at the paths
//! that name it: `live/<its id>/...` and `rooms/<topic>/<its id>/...`.
//! Browsers come in through moq-tokio's server, and [`browser_auth`] keeps them
//! to names of one segment.

use std::{sync::Arc, time::Duration};

use iroh::{
    Endpoint,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_live::layout::LiveLayout;
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
    /// Cancelled when the router shuts down, which closes every session.
    shutdown: CancellationToken,
}

impl IrohSessions {
    /// Creates the handler for `cluster`.
    ///
    /// With `pulls` set, a session that names a ticket in its path pulls the
    /// ticket's broadcast, as a browser's does.
    pub fn new(cluster: Cluster, pulls: Option<Arc<PullState>>) -> Self {
        Self {
            cluster,
            pulls,
            shutdown: CancellationToken::new(),
        }
    }

    /// Spawns a router on `endpoint` with this handler.
    pub fn router(self, endpoint: Endpoint) -> Router {
        let mut router = Router::builder(endpoint);
        for alpn in moq_transport_iroh::alpns() {
            router = router.accept(alpn, self.clone());
        }
        router.spawn()
    }

    /// Runs one session until it closes or the relay shuts down.
    async fn serve(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = connection.remote_id();
        let transport = moq_transport_iroh::accept(connection).map_err(AcceptError::from_err)?;
        let handshake = tokio::time::timeout(
            moq_transport_iroh::HANDSHAKE_TIMEOUT,
            moq_net::Server::new().accept_request(moq_net::time::Instant::now(), transport),
        )
        .await
        .map_err(|_| AcceptError::from_err(moq_net::Error::Timeout))?
        .map_err(AcceptError::from_err)?;
        let path = handshake.path().to_owned();
        let name = requested_name(&path);
        let publish = self
            .cluster
            .origin
            .scope("", &relay_grant(remote).publish)
            .map_err(AcceptError::from_err)?;
        // A session that names a ticket reads the pulled broadcast at that name.
        let serving = name
            .as_deref()
            .filter(|_| self.pulls.is_some())
            .and_then(|name| crate::pull::mounted(&self.cluster, name))
            .map_or_else(
                || self.cluster.origin.consume(),
                |cluster| cluster.origin.consume(),
            );
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
        info!(%path, "iroh client admitted, publishing under its own id only");

        // Held while the session runs.
        let _pull = self
            .pulls
            .as_ref()
            .and_then(|pulls| crate::pull_for(pulls.clone(), name.clone()?));

        let run = moq_net::time::run(driver);
        tokio::pin!(run);
        let ended = tokio::select! {
            err = &mut run => Some(err),
            () = self.shutdown.cancelled() => None,
        };
        match ended {
            Some(err) => debug!(%err, "iroh session ended"),
            // The driver keeps running, so the session can tell its peer.
            None => {
                session.abort(moq_net::Error::Cancel);
                if tokio::time::timeout(CLOSE_GRACE, run).await.is_err() {
                    warn!(grace = ?CLOSE_GRACE, "iroh session did not close in time");
                }
            }
        }
        Ok(())
    }
}

impl ProtocolHandler for IrohSessions {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let span = info_span!("iroh_client", remote = %connection.remote_id().fmt_short());
        self.serve(connection).instrument(span).await
    }

    async fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Returns moq-relay's auth for browsers: subscribe anywhere, publish one segment.
///
/// One segment keeps a browser out of `live/` and `rooms/`, whose paths name
/// an iroh publisher.
pub fn browser_auth() -> moq_relay::auth::Config {
    let mut config = moq_relay::auth::Config::default();
    config.public_subscribe = vec![Pattern::all()];
    config.public_publish = vec!["*".parse().expect("a valid pattern")];
    config
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
    let name = percent_encoding::percent_decode_str(path.trim_start_matches('/'))
        .decode_utf8()
        .ok()?;
    (!name.is_empty()).then(|| name.into_owned())
}

/// Returns what an iroh peer may publish into the relay.
///
/// That is its own `live/` broadcasts and its room broadcasts.
pub(crate) fn relay_grant(peer: iroh::EndpointId) -> iroh_live_node::Grant {
    LiveLayout::default().grant(peer, &[Arc::new(iroh_live::rooms::publish_scope)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_publishes_only_under_its_own_id() {
        let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[6; 32]).public();
        let scope = relay_grant(id).publish;
        assert!(scope.matches(&format!("live/{id}/cam")));
        assert!(scope.matches(&format!("rooms/topic/{id}/cam")));
        assert!(!scope.matches(&format!("live/{other}/cam")));
        assert!(!scope.matches(&format!("rooms/topic/{other}/cam")));
        assert!(!scope.matches("cam"));
    }

    #[test]
    fn a_browser_publishes_one_segment_names() {
        let auth = browser_auth();
        let publish = &auth.public_publish;
        let matches = |path: &str| publish.iter().any(|pattern| pattern.matches(path));
        assert!(matches("hello"));
        assert!(!matches("live/someone/cam"));
        assert!(!matches("rooms/topic/someone/cam"));
    }

    /// Pulls are read at the ticket's spelling, which always has a `/`
    /// between the endpoint id and the name, so no browser can publish there.
    #[test]
    fn a_browser_cannot_publish_at_a_pulled_name() {
        let ticket = iroh_live::BroadcastTicket::new(
            iroh::SecretKey::from_bytes(&[7; 32]).public(),
            "cam".to_owned(),
        );
        let auth = browser_auth();
        let matches = |path: &str| {
            auth.public_publish
                .iter()
                .any(|pattern| pattern.matches(path))
        };
        let full = ticket.to_string();
        let bare = full.strip_prefix("ilv1:").expect("a ticket prefix");
        assert!(!matches(&full));
        assert!(!matches(bare));
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
