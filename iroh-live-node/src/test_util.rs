//! Helpers for tests that run nodes on endpoints in one process.
//!
//! The endpoints find each other through one in-memory address lookup, so no
//! test needs a relay server or DNS.

use std::{future::Future, sync::OnceLock, time::Duration};

use iroh::{
    Endpoint, EndpointId,
    address_lookup::MemoryLookup,
    endpoint::presets,
    protocol::{Router, RouterBuilder},
};

use crate::{Node, NodeConfig};

/// The default bound of [`step`] and [`eventually`].
///
/// Generous, because a test suite shares the machine with other work.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Returns the address lookup that every [`endpoint`] of the process shares.
pub fn lookup() -> &'static MemoryLookup {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    LOOKUP.get_or_init(MemoryLookup::new)
}

/// Binds an endpoint that the other test endpoints can dial.
///
/// # Panics
///
/// Panics if the endpoint fails to bind.
pub async fn endpoint() -> Endpoint {
    let endpoint = Endpoint::builder(presets::Minimal)
        .address_lookup(lookup().clone())
        .bind()
        .await
        .expect("failed to bind endpoint");
    lookup().add_endpoint_info(endpoint.addr());
    endpoint
}

/// Awaits `future`, and fails with `what` after [`TIMEOUT`].
///
/// # Panics
///
/// Panics if `future` does not complete within [`TIMEOUT`].
pub async fn step<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}

/// Polls `condition` until it holds, and fails with `what` after [`TIMEOUT`].
///
/// # Panics
///
/// Panics if `condition` does not hold within [`TIMEOUT`].
pub async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    step(what, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

/// A node mounted on a router, on an [`endpoint`].
#[derive(Debug)]
pub struct TestNode {
    /// The endpoint the node runs on.
    pub endpoint: Endpoint,
    /// The node, mounted on [`router`](Self::router).
    pub node: Node,
    /// The router that accepts the node's protocols.
    pub router: Router,
}

impl TestNode {
    /// Spawns a node with `config`, mounted on a router.
    pub async fn spawn(config: NodeConfig) -> Self {
        Self::spawn_with(config, |_, router| router).await
    }

    /// Spawns a node with `config`, and lets `mount` add protocols to its router.
    pub async fn spawn_with(
        config: NodeConfig,
        mount: impl FnOnce(&Node, RouterBuilder) -> RouterBuilder,
    ) -> Self {
        let endpoint = endpoint().await;
        let node = Node::new(endpoint.clone(), config);
        let router = mount(&node, node.mount(Router::builder(endpoint.clone()))).spawn();
        Self {
            endpoint,
            node,
            router,
        }
    }

    /// Returns the endpoint id of the node.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Shuts the router down, which shuts the node down and closes the endpoint.
    ///
    /// # Panics
    ///
    /// Panics if the router task panicked.
    pub async fn shutdown(&self) {
        self.router.shutdown().await.expect("router task panicked");
    }
}
