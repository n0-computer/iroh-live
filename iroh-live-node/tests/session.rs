//! Session lifecycle over real QUIC connections between two iroh endpoints.

mod common;

use common::{Peer, TIMEOUT, publish, read_counter, step};
use iroh_live_node::{Audience, Error, Reach};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;

/// Two connects to one peer share a session.
#[tokio::test]
#[traced_test]
async fn connecting_twice_to_a_peer_reuses_the_session() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let first = step("dial", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    first.close(moq_net::Error::Cancel);
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let addr = bob.endpoint.addr();
    let (first, second) = step(
        "concurrent dials",
        n0_future::future::zip(alice.moq.connect(addr.clone()), alice.moq.connect(addr)),
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

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

    assert_eq!(accepted.peer(), alice.id());
    assert!(dialed.dialed(), "the dialing side should report dialed");
    assert!(!accepted.dialed(), "the accepting side should not");

    // Closing it removes it again.
    accepted.close(moq_net::Error::Cancel);
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
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

/// The peer of a closed session sees the code it was closed with.
#[tokio::test]
#[traced_test]
async fn the_peer_sees_the_close_code() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

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

    dialed.close(moq_net::Error::App(7));
    let err = step("the peer sees the close", accepted.closed()).await;
    let Error::SessionClosed { source, .. } = &err else {
        panic!("expected SessionClosed, got {err:#}");
    };
    assert!(
        matches!(
            source,
            moq_net::Error::Session(moq_net::SessionError::App(7))
        ),
        "{source:?}"
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Two nodes that dial each other at once each end up with a usable session.
///
/// Both dials may complete, which leaves each side with two sessions to the
/// other. `connect` then hands out the same one every time, and paths resolve
/// in both directions. Whether the dials collide depends on timing, so the
/// test runs a few pairs and reports how many did.
#[tokio::test]
#[traced_test]
async fn a_simultaneous_dial_leaves_both_sides_a_usable_session() {
    const PAIRS: usize = 3;
    let mut collided = 0;
    for _ in 0..PAIRS {
        let alice = Peer::spawn().await;
        let bob = Peer::spawn().await;
        let _alice_cam =
            publish(&alice.moq, alice.path("cam"), Audience::Granted).expect("publish");
        let _bob_cam = publish(&bob.moq, bob.path("cam"), Audience::Granted).expect("publish");

        let (at_alice, at_bob) = step("both dial at once", async {
            tokio::join!(
                alice.moq.connect(bob.endpoint.addr()),
                bob.moq.connect(alice.endpoint.addr()),
            )
        })
        .await;
        let at_alice = at_alice.expect("alice's dial failed");
        let at_bob = at_bob.expect("bob's dial failed");
        assert_eq!(at_alice.peer(), bob.id());
        assert_eq!(at_bob.peer(), alice.id());

        for (node, peer, session) in [(&alice, &bob, &at_alice), (&bob, &alice, &at_bob)] {
            let again = step("connect again", node.moq.connect(peer.endpoint.addr()))
                .await
                .expect("the second connect failed");
            assert_eq!(&again, session, "connect handed out another session");
            let with_peer: Vec<_> = node
                .moq
                .sessions()
                .get()
                .into_iter()
                .filter(|listed| listed.peer() == peer.id())
                .collect();
            assert!(
                (1..=2).contains(&with_peer.len()),
                "{} sessions with one peer",
                with_peer.len()
            );
            assert!(
                with_peer
                    .iter()
                    .all(|listed| listed.connection().close_reason().is_none()),
                "a session with the peer closed"
            );
            if with_peer.len() == 2 {
                collided += 1;
            }
        }

        let from_alice = step(
            "bob subscribes to alice",
            bob.moq
                .subscribe(alice.path("cam"), Reach::Direct(alice.id())),
        )
        .await
        .expect("bob's subscribe failed");
        read_counter(&from_alice.broadcast()).await;
        let from_bob = step(
            "alice subscribes to bob",
            alice
                .moq
                .subscribe(bob.path("cam"), Reach::Direct(bob.id())),
        )
        .await
        .expect("alice's subscribe failed");
        read_counter(&from_bob.broadcast()).await;

        alice.shutdown().await;
        bob.shutdown().await;
    }
    eprintln!("{collided} of {} sides saw two sessions", PAIRS * 2);
}
