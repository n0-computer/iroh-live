//! Sessions from iroh clients, admitted by their endpoint id.
//!
//! moq-tokio's server can accept iroh connections too, but the request it hands
//! to moq-relay's auth carries nothing that identifies the peer, although iroh
//! has authenticated its endpoint id. A relay that admitted iroh clients that
//! way could only let every one of them publish everywhere, and any client
//! could then publish at `live/<someone else>/<name>`, a path whose whole point
//! is that it names its publisher.
//!
//! So the relay accepts iroh sessions itself, with [`IrohSessions`] mounted on
//! its endpoint, and scopes each by the id iroh authenticated: a client may
//! publish only at the paths that name it, `live/<its id>/...` and
//! `rooms/<topic>/<its id>/...`, and may subscribe to anything. Browsers still
//! come in through moq-tokio's server and moq-relay's auth, which
//! [`browser_auth`] keeps to names of one segment, so they cannot publish into
//! either namespace.

use std::sync::Arc;

use iroh::{
    Endpoint, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use moq_net::{Pattern, Patterns};
use moq_relay::cluster::Cluster;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::pull::PullState;

/// How long a closing session gets to tell its peer when the relay shuts down.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Accepts MoQ sessions from iroh clients into the cluster, each scoped to the
/// paths that name it.
///
/// Mount it under every ALPN in [`alpns`](Self::alpns), or let
/// [`router`](Self::router) do so.
#[derive(Clone, derive_more::Debug)]
pub struct IrohSessions {
    #[debug(skip)]
    cluster: Cluster,
    pulls: Option<Arc<PullState>>,
    shutdown: CancellationToken,
}

impl IrohSessions {
    /// Creates the handler for `cluster`.
    ///
    /// With `pulls` set, a session that names a ticket in its path pulls that
    /// ticket's broadcast into the cluster, as a browser's does.
    pub fn new(cluster: Cluster, pulls: Option<Arc<PullState>>) -> Self {
        Self {
            cluster,
            pulls,
            shutdown: CancellationToken::new(),
        }
    }

    /// Returns every ALPN an iroh MoQ client may dial with: each MoQ version,
    /// and WebTransport over HTTP/3.
    pub fn alpns() -> Vec<Vec<u8>> {
        let mut alpns: Vec<Vec<u8>> = moq_net::ALPNS
            .iter()
            .map(|alpn| alpn.as_bytes().to_vec())
            .collect();
        alpns.push(web_transport_iroh::ALPN_H3.as_bytes().to_vec());
        alpns
    }

    /// Spawns a router on `endpoint` that accepts iroh clients with this
    /// handler.
    pub fn router(self, endpoint: Endpoint) -> Router {
        let mut router = Router::builder(endpoint);
        for alpn in Self::alpns() {
            router = router.accept(alpn, self.clone());
        }
        router.spawn()
    }

    /// Runs one session until it closes or the relay shuts down.
    async fn serve(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = connection.remote_id();
        let (transport, target) = iroh_moq::transport::accept(connection)
            .await
            .map_err(AcceptError::from_err)?;
        let handshake = moq_net::Server::new()
            .accept_request(
                tokio::time::Instant::now().into_std(),
                moq_tokio::transport::Session::new(transport),
            )
            .await
            .map_err(AcceptError::from_err)?;
        let path = target.unwrap_or_else(|| handshake.path().to_owned());
        let publish = self
            .cluster
            .origin
            .scope("", &publish_scope(remote))
            .map_err(AcceptError::from_err)?;
        let (session, driver) = handshake
            .with_publisher(self.cluster.origin.consume())
            .with_subscriber(publish)
            .ok()
            .await
            .map_err(AcceptError::from_err)?;
        info!(%path, "iroh client admitted, publishing under its own id only");

        // Held for as long as the session runs, which is what tells the pull
        // that this session stopped wanting the broadcast.
        let _pull = self
            .pulls
            .as_ref()
            .and_then(|pulls| crate::pull_for(pulls.clone(), requested_name(&path)?));

        let run = moq_net::time::run(driver);
        tokio::pin!(run);
        tokio::select! {
            err = &mut run => debug!(%err, "iroh session ended"),
            _ = self.shutdown.cancelled() => {
                session.abort(moq_net::Error::Cancel);
                if tokio::time::timeout(CLOSE_GRACE, &mut run).await.is_err() {
                    warn!("iroh session did not close in time");
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

/// Returns what the iroh client `id` may publish into the relay: the paths
/// that name it, as [`iroh_live::grant`] allows.
pub fn publish_scope(id: EndpointId) -> Patterns {
    iroh_live::grant(id).publish
}

/// Returns moq-relay's auth for sessions that are not iroh's, browsers above
/// all: anyone may subscribe to anything, and publish at a name of one segment.
///
/// One segment keeps a browser out of `live/` and `rooms/`, whose paths name
/// their publisher, which a browser has no iroh identity to prove. The relay's
/// publish page publishes at the one-segment name its `?name=` gives.
pub fn browser_auth() -> moq_relay::auth::Config {
    let mut config = moq_relay::auth::Config::default();
    config.public_subscribe = vec![Pattern::all()];
    config.public_publish = vec!["*".parse().expect("a valid pattern")];
    config
}

/// Returns the broadcast name a session asked for in its path, if any.
fn requested_name(path: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or_default();
    let name = path.trim_start_matches('/');
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_publishes_only_under_its_own_id() {
        let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[6; 32]).public();
        let scope = publish_scope(id);
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

    #[test]
    fn a_requested_name_drops_the_slash_and_the_query() {
        assert_eq!(requested_name("/abc?jwt=x").as_deref(), Some("abc"));
        assert_eq!(requested_name(""), None);
        assert_eq!(requested_name("/"), None);
    }
}
