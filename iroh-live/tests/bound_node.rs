//! A `BoundNode` dropped without a shutdown: it shuts down in the background.

use iroh::protocol::Router;
use iroh_live::{BoundNode, Node, layout::LiveLayout};
use iroh_live_node::test_util::{TestNode, endpoint, eventually, step};
use n0_tracing_test::traced_test;

/// Returns a node on a fresh test endpoint, mounted on a router if `accept`.
async fn bound(accept: bool) -> BoundNode {
    let endpoint = endpoint().await;
    let node = Node::new(endpoint.clone(), LiveLayout::default().config());
    let router = accept.then(|| node.mount(Router::builder(endpoint)).spawn());
    BoundNode::new(node, router)
}

#[tokio::test]
#[traced_test]
async fn a_dropped_bound_node_closes_its_sessions_and_its_endpoint() {
    let peer = TestNode::spawn(LiveLayout::default().config()).await;
    let bound = bound(true).await;
    let endpoint = bound.endpoint().clone();
    let session = step("the peer dials", peer.node.connect(endpoint.addr()))
        .await
        .expect("connect");
    let closed = bound.closed();

    drop(bound);
    step("the node shuts down", closed).await;
    // Well within the idle timeout, so the peer was told.
    step("the peer sees the close", session.closed()).await;
    eventually("the endpoint closes", || endpoint.is_closed()).await;

    peer.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_dropped_dial_only_bound_node_closes_its_sessions_and_its_endpoint() {
    let peer = TestNode::spawn(LiveLayout::default().config()).await;
    let bound = bound(false).await;
    let endpoint = bound.endpoint().clone();
    let session = step("the node dials", bound.connect(peer.endpoint.addr()))
        .await
        .expect("connect");

    drop(bound);
    step("the session closes", session.closed()).await;
    eventually("the endpoint closes", || endpoint.is_closed()).await;

    peer.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_shut_down_bound_node_drops_quietly() {
    let bound = bound(true).await;
    let endpoint = bound.endpoint().clone();
    bound.shutdown().await;
    assert!(endpoint.is_closed());
    // A second shutdown is a no-op, and so is the drop.
    bound.shutdown().await;
    drop(bound);
}

#[test]
fn a_bound_node_dropped_outside_a_runtime_does_not_panic() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let bound = runtime.block_on(bound(true));
    let endpoint = bound.endpoint().clone();

    // Outside the runtime, so nothing can run the shutdown.
    drop(bound);
    assert!(!endpoint.is_closed());

    runtime.block_on(endpoint.close());
}
