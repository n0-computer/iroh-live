//! A node on its own endpoint, for apps that run one node at a time.

use iroh::protocol::Router;
use iroh_live_node::{Node, NodeConfig};
use tracing::warn;

use crate::{EndpointConfig, Error};

/// A [`Node`] bound to its own endpoint, with the [`Router`] that accepts its peers.
///
/// Small apps bind one per screen or command. [`shutdown`](Self::shutdown)
/// closes every session, then the endpoint, so peers see the close at once.
/// Dropping it without that, as an early `?` does, shuts it down in a
/// background task instead of leaving the peers to time out.
#[derive(Debug)]
pub struct BoundNode {
    node: Node,
    /// Taken by `shutdown`, so the drop does nothing more.
    router: Option<Router>,
}

impl BoundNode {
    /// Binds an endpoint with `endpoint`, starts a node on it with `config`, and mounts it.
    ///
    /// # Errors
    ///
    /// Fails if the endpoint does not bind.
    pub async fn bind(endpoint: EndpointConfig, config: NodeConfig) -> Result<Self, Error> {
        let endpoint = endpoint.bind().await?;
        let node = Node::new(endpoint.clone(), config);
        let router = node.mount(Router::builder(endpoint)).spawn();
        Ok(Self {
            node,
            router: Some(router),
        })
    }

    /// Returns the node.
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// Shuts the node down, then the router, which closes the endpoint.
    pub async fn shutdown(mut self) {
        if let Some(router) = self.router.take() {
            shutdown(&self.node, router).await;
        }
    }
}

impl Drop for BoundNode {
    fn drop(&mut self) {
        let Some(router) = self.router.take() else {
            return;
        };
        // Drop cannot wait, so the shutdown runs on its own. It ends once the
        // endpoint has closed, and outside a runtime it cannot run at all.
        let node = self.node.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn(async move { shutdown(&node, router).await })),
            Err(_) => warn!("a node dropped outside a runtime, so its peers wait for a timeout"),
        }
    }
}

/// Shuts `node` down, so its sessions close cleanly, then `router` and the endpoint.
async fn shutdown(node: &Node, router: Router) {
    node.shutdown().await;
    if let Err(err) = router.shutdown().await {
        warn!(error = %format!("{err:#}"), "the router failed to shut down");
    }
}
