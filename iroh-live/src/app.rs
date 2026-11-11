//! A node on its own endpoint, for apps that run one node at a time.

use std::ops::Deref;

use iroh::protocol::Router;
use iroh_live_node::{Node, NodeConfig};
use tracing::warn;

use crate::{EndpointConfig, Error};

/// A [`Node`] bound to its own endpoint, with the [`Router`] that accepts its peers.
///
/// Small apps bind one per screen or command. Derefs to [`Node`].
/// [`shutdown`](Self::shutdown) closes every session, then the endpoint, so
/// peers see the close at once. Dropping it without that, as an early `?`
/// does, shuts it down in a background task instead of leaving the peers to
/// time out.
#[derive(Debug)]
pub struct BoundNode {
    node: Node,
    /// Accepts incoming sessions. `None` for a node that only dials.
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
        Ok(Self::new(node, Some(router)))
    }

    /// Wraps `node` and the `router` that serves it, or `None` for a node that only dials.
    ///
    /// For a router with services of its own beside the node, such as rooms.
    pub fn new(node: Node, router: Option<Router>) -> Self {
        Self { node, router }
    }

    /// Returns the node.
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// Shuts the node down, then the router and the endpoint.
    ///
    /// Sessions say goodbye before the endpoint closes. Idempotent.
    pub async fn shutdown(&self) {
        shutdown(&self.node, self.router.as_ref()).await;
    }
}

impl Deref for BoundNode {
    type Target = Node;

    fn deref(&self) -> &Node {
        &self.node
    }
}

impl Drop for BoundNode {
    fn drop(&mut self) {
        if self.node.endpoint().is_closed() {
            return;
        }
        // Drop cannot wait, so the shutdown runs on its own. It ends once the
        // endpoint has closed, and outside a runtime it cannot run at all.
        let node = self.node.clone();
        let router = self.router.take();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                drop(runtime.spawn(async move { shutdown(&node, router.as_ref()).await }))
            }
            Err(_) => warn!("a node dropped outside a runtime, so its peers wait for a timeout"),
        }
    }
}

/// Shuts `node` down, so its sessions close cleanly, then `router` and the endpoint.
async fn shutdown(node: &Node, router: Option<&Router>) {
    node.shutdown().await;
    // The router closes the endpoint once its handlers have shut down.
    if let Some(router) = router {
        match router.shutdown().await {
            Ok(()) => return,
            Err(err) => warn!(
                error = format_args!("{err:#}"),
                "the router failed to shut down"
            ),
        }
    }
    node.endpoint().close().await;
}
