//! Node identity and lifecycle: `id`, `grant_for` and `closed`.

mod common;

use common::{Peer, TIMEOUT, step};
use iroh_live_node::{
    Grant, NodeConfig,
    policy::{self, Authorization},
};
use n0_tracing_test::traced_test;

#[tokio::test]
#[traced_test]
async fn closed_resolves_on_shutdown() {
    let peer = Peer::spawn().await;
    let closed = tokio::spawn(peer.node.closed());
    assert!(!closed.is_finished());

    peer.node.shutdown().await;
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
async fn grant_for_reports_the_policy_authorization_of_a_live_peer() {
    let alice =
        Peer::bare(NodeConfig::default().with_policy(policy::Open(Grant::everything()))).await;
    assert_eq!(alice.node.id(), alice.id());
    let bob = Peer::bare(NodeConfig::default()).await;
    assert_eq!(alice.node.grant_for(bob.id()), None, "no session yet");

    let session = step("alice dials bob", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("connect");
    assert_eq!(
        alice.node.grant_for(bob.id()),
        Some(Authorization::from(Grant::everything()))
    );
    // Bob's default policy lets alice subscribe and publish nothing.
    let at_bob = step("bob registers alice", async {
        loop {
            if let Some(authorization) = bob.node.grant_for(alice.id()) {
                return authorization;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(at_bob, Authorization::from(Grant::subscribe_only()));

    session.close(moq_net::Error::Cancel);
    step("the authorization goes with the session", async {
        while alice.node.grant_for(bob.id()).is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}
