//! Pull ownership: the relay pulls a ticket only for the peer that owns its path.

use std::{sync::OnceLock, time::Duration};

use iroh::{SecretKey, address_lookup::MemoryLookup, protocol::Router};
use iroh_live::{BroadcastTicket, layout::LiveLayout};
use iroh_live_node::{Audience, Node};
use iroh_live_relay::pull::{self, PullState};
use moq_net::Path;
use moq_relay::cluster::Cluster;
use n0_future::task::AbortOnDropHandle;
use serial_test::serial;

/// How long any one wait may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Returns the address lookup every test endpoint registers with.
fn lookup() -> &'static MemoryLookup {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    LOOKUP.get_or_init(MemoryLookup::new)
}

/// Binds an endpoint with a fresh identity that the other test endpoints can dial.
async fn endpoint() -> iroh::Endpoint {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(lookup().clone())
        .secret_key(SecretKey::generate())
        .bind()
        .await
        .expect("bind");
    lookup().add_endpoint_info(endpoint.addr());
    endpoint
}

/// A cluster with a running driver, and nothing else.
fn cluster() -> (Cluster, AbortOnDropHandle<()>) {
    let (cluster, driver) =
        Cluster::new(moq_relay::cluster::Options::new(Default::default())).expect("init cluster");
    let task = tokio::spawn(async move {
        driver.run().await.ok();
    });
    (cluster, AbortOnDropHandle::new(task))
}

/// A publisher node that serves `live/<its id>/<name>`.
struct Publisher {
    endpoint: iroh::Endpoint,
    node: Node,
    router: Router,
}

impl Publisher {
    /// Starts the node and publishes `name`.
    async fn start(name: &str) -> Self {
        let endpoint = endpoint().await;
        let node = Node::new(endpoint.clone(), LiveLayout::default().config(&[]));
        let router = node.mount(Router::builder(endpoint.clone())).spawn();
        let publisher = Self {
            endpoint,
            node,
            router,
        };
        publisher.publish(name);
        publisher
    }

    /// Publishes `name` to everyone and returns its ticket.
    fn publish(&self, name: &str) -> BroadcastTicket {
        let path = LiveLayout::default()
            .path(self.node.id(), name)
            .expect("a path");
        let publication = self
            .node
            .publish(path, Audience::Everyone)
            .expect("publish");
        // The broadcast has to stay open for as long as the test runs.
        std::mem::forget(publication.broadcast().clone());
        publication.ticket()
    }

    /// Shuts down the node, its router and its endpoint.
    async fn shutdown(self) {
        self.node.shutdown().await;
        self.router.shutdown().await.expect("router");
        self.endpoint.close().await;
    }
}

/// Returns whether `cluster` routes `name` within half a second.
async fn routed(cluster: &Cluster, name: &str) -> bool {
    let consumer = cluster.origin.consume();
    let follow = async {
        consumer
            .follow(name, None)
            .expect("a name")
            .available()
            .await
    };
    tokio::time::timeout(Duration::from_millis(500), follow)
        .await
        .is_ok_and(|found| found.is_ok())
}

/// A ticket that names another peer's path is refused.
#[tokio::test]
#[serial]
async fn a_pull_of_another_peers_path_is_refused() {
    let (cluster, _driver) = cluster();
    let victim = Publisher::start("cam").await;
    let mallory = SecretKey::generate().public();
    let victim_path = LiveLayout::default()
        .path(victim.node.id(), "cam")
        .expect("a path");
    let forged = BroadcastTicket::at(mallory, victim_path.clone());
    let requested = forged.to_string();

    let state = PullState::new(endpoint().await, cluster.clone());
    let err = tokio::time::timeout(TIMEOUT, state.pull(&requested, &forged))
        .await
        .expect("no timeout")
        .expect_err("mallory does not own the path");
    assert!(format!("{err:#}").contains("does not own"), "{err:#}");
    assert!(!routed(&cluster, victim_path.as_str()).await);

    victim.shutdown().await;
}

/// A path outside `live/` and `rooms/` has no owner the relay can verify.
#[tokio::test]
#[serial]
async fn a_path_in_a_custom_scheme_is_refused() {
    let (cluster, _driver) = cluster();
    let publisher = Publisher::start("cam").await;
    let custom = Path::new("fleet/robot/cam").expect("a path");
    let ticket = BroadcastTicket::at(publisher.node.id(), custom);

    let state = PullState::new(endpoint().await, cluster);
    let result = tokio::time::timeout(TIMEOUT, state.pull(&ticket.to_string(), &ticket))
        .await
        .expect("no timeout");
    assert!(result.is_err(), "the relay cannot verify the owner");

    publisher.shutdown().await;
}

/// A ticket with reserved characters mounts under its raw string, and routes at its own path.
#[tokio::test]
#[serial]
async fn a_ticket_with_reserved_characters_mounts_once() {
    let (cluster, _driver) = cluster();
    let publisher = Publisher::start("100%").await;
    let path = LiveLayout::default()
        .path(publisher.node.id(), "100%")
        .expect("a path");
    let ticket = BroadcastTicket::at(publisher.node.id(), path.clone());
    let raw = ticket.to_string();
    assert!(raw.ends_with("/100%25"), "{raw}");

    let state = PullState::new(endpoint().await, cluster.clone());
    let guard = tokio::time::timeout(TIMEOUT, state.pull(&raw, &ticket))
        .await
        .expect("no timeout")
        .expect("pull");
    // A session that dialed the ticket's URL reads it under the raw string.
    let serving = pull::mounted(&cluster, &raw).expect("a ticket the cluster mounts");
    assert!(routed(&serving, &raw).await, "mounted under {raw}");
    assert!(routed(&cluster, path.as_str()).await, "at its own path");

    drop(guard);
    publisher.shutdown().await;
}
