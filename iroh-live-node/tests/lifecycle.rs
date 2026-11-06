//! Node identity and lifecycle: `id`, `grant_for` and `closed`.

mod common;

use std::sync::Arc;

use common::{Peer, TIMEOUT};
use iroh_live_node::{Grant, NodeConfig};
use n0_tracing_test::traced_test;

#[tokio::test]
#[traced_test]
async fn closed_resolves_on_shutdown() {
    let peer = Peer::spawn().await;
    let closed = tokio::spawn(peer.moq.closed());
    assert!(!closed.is_finished());

    peer.moq.shutdown().await;
    tokio::time::timeout(TIMEOUT, closed)
        .await
        .expect("closed resolves")
        .expect("task");
    peer.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn closed_resolves_when_the_last_handle_drops() {
    let endpoint = common::endpoint().await;
    let node = iroh_live_node::Node::new(endpoint.clone(), NodeConfig::default());
    let closed = tokio::spawn(node.closed());

    drop(node);
    tokio::time::timeout(TIMEOUT, closed)
        .await
        .expect("closed resolves")
        .expect("task");
    endpoint.close().await;
}

#[tokio::test]
#[traced_test]
async fn id_and_grant_for_follow_the_config() {
    let default = Peer::bare(NodeConfig::default()).await;
    assert_eq!(default.moq.id(), default.id());
    let other = default.id();
    assert_eq!(default.moq.grant_for(other), Grant::subscribe_only());

    let open = Peer::bare(NodeConfig {
        grant: Some(Arc::new(|_| Grant::everything())),
        ..Default::default()
    })
    .await;
    assert_eq!(open.moq.grant_for(other), Grant::everything());

    default.shutdown().await;
    open.shutdown().await;
}
