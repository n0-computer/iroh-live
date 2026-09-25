//! Sessions from iroh clients, scoped by their endpoint id.
//!
//! moq-tokio's server can accept iroh connections, but the auth request it
//! hands moq-relay carries no peer identity. So the relay accepts iroh sessions
//! itself with [`IrohSessions`], and lets each client publish only at the paths
//! that name it, `live/<its id>/...` and `rooms/<topic>/<its id>/...`.
//! Browsers come in through moq-tokio's server, and [`browser_auth`] keeps them
//! to names of one segment.

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

/// Accepts iroh clients' MoQ sessions into the cluster.
///
/// Mount it under every ALPN in [`iroh_moq::alpns`], or use
/// [`router`](Self::router).
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
        for alpn in iroh_moq::alpns() {
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

        // Held while the session runs.
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

/// Returns the paths the iroh client `id` may publish at, as [`iroh_live::grant`] allows.
pub fn publish_scope(id: EndpointId) -> Patterns {
    iroh_live::grant(id).publish
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
pub(crate) fn requested_name(path: &str) -> Option<String> {
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
