//! Pull ownership: the relay pulls a ticket only for the peer that owns its path.

use std::time::Duration;

use iroh::SecretKey;
use iroh_live::{BroadcastTicket, layout::LiveLayout};
use iroh_live_node::{
    Audience,
    test_util::{TestNode, endpoint, step},
};
use iroh_live_relay::pull::{self, Pulls};
use moq_net::Path;
use moq_relay::cluster::Cluster;
use n0_future::task::AbortOnDropHandle;
use serial_test::serial;

/// A cluster with a running driver, and nothing else.
fn cluster() -> (Cluster, AbortOnDropHandle<()>) {
    let (cluster, driver) =
        Cluster::new(moq_relay::cluster::Options::new(Default::default())).expect("init cluster");
    let task = tokio::spawn(async move {
        driver.run().await.ok();
    });
    (cluster, AbortOnDropHandle::new(task))
}

/// Starts a node that serves `live/<its id>/<name>`.
async fn publisher(name: &str) -> TestNode {
    let publisher = TestNode::spawn(LiveLayout::default().config()).await;
    let path = LiveLayout::default()
        .path(publisher.id(), name)
        .expect("a path");
    let publication = publisher
        .node
        .publish(path, Audience::Granted)
        .expect("publish");
    // The broadcast has to stay open for as long as the test runs.
    std::mem::forget(publication.broadcast().clone());
    publisher
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
    let victim = publisher("cam").await;
    let mallory = SecretKey::generate().public();
    let victim_path = LiveLayout::default()
        .path(victim.node.id(), "cam")
        .expect("a path");
    let forged = BroadcastTicket::at(mallory, victim_path.clone());
    let requested = forged.to_string();

    let pulls = Pulls::new(endpoint().await, cluster.clone());
    let err = step("no timeout", pulls.pull(&requested, &forged))
        .await
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
    let publisher = publisher("cam").await;
    let custom = Path::new("fleet/robot/cam").expect("a path");
    let ticket = BroadcastTicket::at(publisher.node.id(), custom);

    let pulls = Pulls::new(endpoint().await, cluster);
    let result = step("no timeout", pulls.pull(&ticket.to_string(), &ticket)).await;
    assert!(result.is_err(), "the relay cannot verify the owner");

    publisher.shutdown().await;
}

/// A ticket with reserved characters mounts under its raw string, and routes at its own path.
#[tokio::test]
#[serial]
async fn a_ticket_with_reserved_characters_mounts_once() {
    let (cluster, _driver) = cluster();
    let publisher = publisher("100%").await;
    let path = LiveLayout::default()
        .path(publisher.node.id(), "100%")
        .expect("a path");
    let ticket = BroadcastTicket::at(publisher.node.id(), path.clone());
    let raw = ticket.to_string();
    assert!(raw.ends_with("/100%25"), "{raw}");

    let pulls = Pulls::new(endpoint().await, cluster.clone());
    let guard = step("no timeout", pulls.pull(&raw, &ticket))
        .await
        .expect("pull");
    // A session that dialed the ticket's URL reads it under the raw string.
    let serving = pull::mounted(&cluster, &raw).expect("a ticket the cluster mounts");
    assert!(routed(&serving, &raw).await, "mounted under {raw}");
    assert!(routed(&cluster, path.as_str()).await, "at its own path");

    drop(guard);
    publisher.shutdown().await;
}
