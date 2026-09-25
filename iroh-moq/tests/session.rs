//! Session lifecycle over real QUIC connections between two iroh endpoints.

mod common;

use common::{Node, TIMEOUT, step};
use iroh_moq::Error;
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;

/// Two connects to one peer share a session.
#[tokio::test]
#[traced_test]
async fn connecting_twice_to_a_peer_reuses_the_session() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let first = step("dial", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    let second = step("second dial", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("the second dial failed");

    assert_eq!(first, second, "the second connect opened a second session");
    assert_eq!(
        first.connection().stable_id(),
        second.connection().stable_id()
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A connect right after a close dials anew.
#[tokio::test]
#[traced_test]
async fn a_connect_after_a_close_dials_anew() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let first = step("dial", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    first.close("done with it");
    let second = step("dial again", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("the second dial failed");
    assert_ne!(first, second, "connect handed out the session being closed");
    assert!(second.connection().close_reason().is_none());

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Concurrent calls for one peer coalesce onto a single dial.
#[tokio::test]
#[traced_test]
async fn concurrent_connects_to_a_peer_share_one_dial() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let addr = bob.endpoint.addr();
    let (first, second) = step(
        "concurrent dials",
        futures_lite::future::zip(alice.moq.connect(addr.clone()), alice.moq.connect(addr)),
    )
    .await;
    let first = first.expect("failed to dial");
    let second = second.expect("the concurrent dial failed");
    assert_eq!(
        first.connection().stable_id(),
        second.connection().stable_id(),
        "two concurrent connects opened two connections",
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// The accepting side sees its session in `sessions()`.
#[tokio::test]
#[traced_test]
async fn an_accepted_session_appears_in_sessions() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let mut sessions = bob.moq.sessions();
    let dialed = step("dial", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");

    let accepted = step("the accepted session", async {
        loop {
            if let Some(session) = sessions.get().into_iter().next() {
                return session;
            }
            sessions.updated().await.expect("node gone");
        }
    })
    .await;

    assert_eq!(accepted.remote_id(), alice.id());
    assert!(dialed.dialed(), "the dialing side should report dialed");
    assert!(!accepted.dialed(), "the accepting side should not");

    // Closing it removes it again.
    accepted.close("test");
    step("the session leaves the list", async {
        while !sessions.get().is_empty() {
            sessions.updated().await.expect("node gone");
        }
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Shutting the transport down closes its sessions and opens no more.
#[tokio::test]
#[traced_test]
async fn shutdown_closes_sessions_and_refuses_new_ones() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let bob_addr = bob.endpoint.addr();

    let session = step("dial", alice.moq.connect(bob_addr.clone()))
        .await
        .expect("failed to dial");

    alice.moq.shutdown().await;

    tokio::time::timeout(TIMEOUT, session.closed())
        .await
        .expect("the session did not close when the transport shut down");

    let err = step("connecting after shutdown", alice.moq.connect(bob_addr))
        .await
        .expect_err("connecting after shutdown should fail");
    assert!(matches!(err, Error::ShutDown { .. }), "{err:#}");
    assert!(
        alice.moq.sessions().get().is_empty(),
        "no session survives shutdown"
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// `accept` returns `None` at shutdown, even while a clone of the node is held.
#[tokio::test]
#[traced_test]
async fn accept_ends_at_shutdown() {
    let alice = Node::spawn().await;
    let moq = alice.moq.clone();
    let waiting = tokio::spawn(async move { moq.accept().await.is_none() });
    alice.moq.shutdown().await;
    assert!(
        step("accept", waiting).await.expect("task"),
        "accept should end at shutdown"
    );
    alice.shutdown().await;
}
