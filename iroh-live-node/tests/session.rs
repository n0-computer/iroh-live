//! Session lifecycle over real QUIC connections between two iroh endpoints.

mod common;

use std::{num::NonZeroU64, time::Duration};

use common::{Peer, TIMEOUT, TestBroadcast, eventually, publish, read_counter, step, subscription};
use iroh_live_node::{Audience, ConnectOptions, Direction, Error, Reach};
use moq_net::{
    broadcast,
    origin::{self, RouteOptions},
};
use n0_future::StreamExt;
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;

/// Two connects to one peer share a session.
#[tokio::test]
#[traced_test]
async fn connecting_twice_to_a_peer_reuses_the_session() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let first = step("dial", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    let second = step("second dial", alice.node.connect(bob.endpoint.addr()))
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

    let first = step("dial", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    first.close(moq_net::Error::Cancel);
    let second = step("dial again", alice.node.connect(bob.endpoint.addr()))
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
        n0_future::future::zip(alice.node.connect(addr.clone()), alice.node.connect(addr)),
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

/// A session's connection lists the network paths to the peer, one of them selected.
#[tokio::test]
#[traced_test]
async fn a_session_connection_lists_its_paths() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let session = step("dial", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    let paths = session.connection().paths();
    assert!(!paths.is_empty(), "an open session has no path");
    assert!(
        paths.iter().any(|path| path.is_selected()),
        "no path is selected"
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

    let mut sessions = bob.node.sessions();
    let dialed = step("dial", alice.node.connect(bob.endpoint.addr()))
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
    assert_eq!(dialed.direction(), Direction::Dialed);
    assert_eq!(accepted.direction(), Direction::Accepted);

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

/// `accepted()` yields accepted sessions once each, never a dialed one, and ends at shutdown.
#[tokio::test]
#[traced_test]
async fn accepted_yields_each_accepted_session_once() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;
    let dave = Peer::spawn().await;

    step("alice dials", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    step(
        "bob lists alice",
        common::session_with(&bob.node, alice.id()),
    )
    .await;
    let mut accepted = bob.node.accepted();
    let first = step("the open session", accepted.next())
        .await
        .expect("the stream ended");
    assert_eq!(first.peer(), alice.id());

    // A session bob dials is not an accepted one.
    step("bob dials dave", bob.node.connect(dave.endpoint.addr()))
        .await
        .expect("failed to dial");
    step("carol dials", carol.node.connect(bob.endpoint.addr()))
        .await
        .expect("failed to dial");
    let second = step("the new session", accepted.next())
        .await
        .expect("the stream ended");
    assert_eq!(second.peer(), carol.id(), "yielded {second:?}");
    assert_eq!(second.direction(), Direction::Accepted);

    bob.node.shutdown().await;
    let end = step("the end of the stream", accepted.next()).await;
    assert!(end.is_none(), "yielded {end:?} after shutdown");

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
    dave.shutdown().await;
}

/// Shutting the transport down closes its sessions and opens no more.
#[tokio::test]
#[traced_test]
async fn shutdown_closes_sessions_and_refuses_new_ones() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let bob_addr = bob.endpoint.addr();

    let session = step("dial", alice.node.connect(bob_addr.clone()))
        .await
        .expect("failed to dial");

    alice.node.shutdown().await;

    tokio::time::timeout(TIMEOUT, session.closed())
        .await
        .expect("the session did not close when the transport shut down");

    let err = step("connecting after shutdown", alice.node.connect(bob_addr))
        .await
        .expect_err("connecting after shutdown should fail");
    assert!(matches!(err, Error::ShutDown { .. }), "{err:#}");
    assert!(
        alice.node.sessions().get().is_empty(),
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

    let mut sessions = bob.node.sessions();
    let dialed = step("dial", alice.node.connect(bob.endpoint.addr()))
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
            publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
        let _bob_cam = publish(&bob.node, bob.path("cam"), Audience::Granted).expect("publish");

        let (at_alice, at_bob) = step("both dial at once", async {
            tokio::join!(
                alice.node.connect(bob.endpoint.addr()),
                bob.node.connect(alice.endpoint.addr()),
            )
        })
        .await;
        let at_alice = at_alice.expect("alice's dial failed");
        let at_bob = at_bob.expect("bob's dial failed");
        assert_eq!(at_alice.peer(), bob.id());
        assert_eq!(at_bob.peer(), alice.id());

        for (node, peer, session) in [(&alice, &bob, &at_alice), (&bob, &alice, &at_bob)] {
            let again = step("connect again", node.node.connect(peer.endpoint.addr()))
                .await
                .expect("the second connect failed");
            assert_eq!(&again, session, "connect handed out another session");
            let with_peer: Vec<_> = node
                .node
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
            bob.node
                .subscribe(alice.path("cam"), Reach::Direct(alice.id())),
        )
        .await
        .expect("bob's subscribe failed");
        read_counter(&from_alice.broadcast()).await;
        let from_bob = step(
            "alice subscribes to bob",
            alice
                .node
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

/// A second session with a peer serves the peer's broadcasts only once the first ends.
///
/// Both sessions of a simultaneous dial announce the peer's broadcasts, and
/// moq-lite-06 tells the two announcements of one path apart as two
/// instances. Had the second route ranked first, moq would take it for the
/// peer replacing the broadcast and fail the track requests in flight on the
/// first session as unroutable, which is how the simultaneous dial test
/// failed. The second session here is dialed by hand, costs less than the
/// first, and publishes a broadcast of its own, so a reader can tell which
/// session serves it.
#[tokio::test]
#[traced_test]
async fn a_second_session_with_a_peer_takes_over_its_routes_only_once_the_first_ends() {
    const SECOND_FIRST: u64 = 1_000_000;
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let path = alice.path("cam");
    let _alice_cam = publish(&alice.node, &path, Audience::Granted).expect("publish");
    let _bob_cam = publish(&bob.node, bob.path("cam"), Audience::Granted).expect("publish");

    let expensive = ConnectOptions {
        cost: Some(100),
        ..Default::default()
    };
    let first = step(
        "dial",
        alice.node.connect_with(bob.endpoint.addr(), expensive),
    )
    .await
    .expect("alice's dial failed");
    // Bob subscribing before he lists the session would dial a second one.
    eventually("bob lists the session", || sessions_with(&bob, &alice) == 1).await;
    let subscribed = step(
        "bob subscribes to alice",
        bob.node.subscribe(path.clone(), Reach::Direct(alice.id())),
    )
    .await
    .expect("bob's subscribe failed");
    let held = subscribed.broadcast();
    let counter = read_counter(&held).await;
    assert!(
        counter < SECOND_FIRST,
        "read {counter} from the first session"
    );

    // A second session from alice's endpoint, announcing a broadcast of its
    // own at the same path, which tells the two sessions' broadcasts apart.
    let second_origin = origin(7);
    let second_route = second_origin
        .route(path.as_str(), RouteOptions::default())
        .expect("route");
    let _second_cam = TestBroadcast::writing(second_route.broadcast().clone(), SECOND_FIRST);
    let seen_by_second = origin(8);
    let _second = step(
        "second session",
        dial(
            &alice.endpoint,
            bob.endpoint.addr(),
            second_origin.consume(),
            seen_by_second.clone(),
        ),
    )
    .await;
    // Bob's announcement over the second session shows that bob runs it.
    step(
        "bob announces over the second session",
        seen_by_second
            .consume()
            .request(bob.path("cam").as_str(), None),
    )
    .await
    .expect("bob's broadcast never came over the second session");
    // Long enough for bob to take in alice's announcement too, many times over.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let with_alice = sessions_with(&bob, &alice);
    assert_eq!(with_alice, 2, "bob has {with_alice} sessions with alice");

    let counter = read_counter(&held).await;
    assert!(
        counter < SECOND_FIRST,
        "the held broadcast read {counter} from the second session"
    );
    let counter = read_counter(&subscribed.broadcast()).await;
    assert!(
        counter < SECOND_FIRST,
        "the subscription read {counter} from the second session"
    );

    first.close(moq_net::Error::Cancel);
    step("the second session takes over", async {
        loop {
            if let Some(counter) = try_read_counter(&subscribed.broadcast()).await
                && counter >= SECOND_FIRST
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Returns how many sessions `node` lists with `peer`.
fn sessions_with(node: &Peer, peer: &Peer) -> usize {
    node.node
        .sessions()
        .get()
        .into_iter()
        .filter(|session| session.peer() == peer.id())
        .count()
}

/// Returns an origin that runs on its own, with routes attributed to `hop`.
fn origin(hop: u64) -> origin::Producer {
    moq_tokio::origin::spawn_config(origin::Config::new(
        NonZeroU64::new(hop).expect("a non-zero hop"),
    ))
}

/// A MoQ session and the task that drives it.
struct RawSession {
    _session: moq_net::Session,
    _driver: n0_future::task::AbortOnDropHandle<()>,
}

/// Dials `peer` from `endpoint` outside any node, publishing `publisher` and
/// writing what the peer announces to `subscriber`.
async fn dial(
    endpoint: &iroh::Endpoint,
    peer: iroh::EndpointAddr,
    publisher: origin::Consumer,
    subscriber: origin::Producer,
) -> RawSession {
    let transport = moq_transport_iroh::Transport::connect(endpoint, peer)
        .await
        .expect("dial failed");
    let (session, driver) = moq_net::Client::new()
        .with_publisher(publisher)
        .with_subscriber(subscriber)
        .connect(moq_net::time::Instant::now(), transport)
        .await
        .expect("handshake failed");
    let driver = tokio::spawn(async move {
        moq_net::time::run(driver).await;
    });
    RawSession {
        _session: session,
        _driver: n0_future::task::AbortOnDropHandle::new(driver),
    }
}

/// Reads one frame of the test track, or returns `None` if that fails.
async fn try_read_counter(broadcast: &broadcast::Consumer) -> Option<u64> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut subscriber = broadcast
            .track("video")
            .ok()?
            .subscribe(subscription())
            .await
            .ok()?;
        loop {
            let mut group = subscriber.recv_group().await.ok()??;
            if let Some(frame) = group.read_frame().await.ok()? {
                let bytes: [u8; 8] = frame.payload[..].try_into().ok()?;
                return Some(u64::from_be_bytes(bytes));
            }
        }
    })
    .await
    .ok()
    .flatten()
}
