//! Session lifecycle over a real QUIC connection between two iroh endpoints.
//!
//! What the transport promises and this covers: one session per peer however
//! many callers ask for it, a session the accepting side can reach as well as
//! the dialling one, and a shutdown that closes both and opens no more.

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets, protocol::Router};
use iroh_moq::Moq;
use n0_tracing_test::traced_test;

/// Generous, because the suite shares a machine with whatever else is running.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Binds an endpoint against a shared in-memory address lookup, so peers in one
/// test process reach each other without a discovery service.
async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("failed to bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

/// A node that accepts MoQ, with the router that makes it do so.
struct Node {
    endpoint: Endpoint,
    moq: Moq,
    router: Router,
}

impl Node {
    async fn spawn() -> Self {
        let endpoint = endpoint().await;
        let moq = Moq::new(endpoint.clone());
        let mut router = Router::builder(endpoint.clone());
        // Every ALPN this build speaks, which is what `Live::register_protocols`
        // mounts, so the tests negotiate the way an application does.
        for alpn in iroh_moq::alpns() {
            router = router.accept(alpn, moq.protocol_handler());
        }
        Self {
            endpoint,
            moq,
            router: router.spawn(),
        }
    }

    async fn shutdown(self) {
        self.moq.shutdown();
        self.router.shutdown().await.expect("router task panicked");
        self.endpoint.close().await;
    }
}

/// Two calls for one peer share a session rather than opening a second
/// connection, which is what keeps a node from accumulating one connection per
/// broadcast it subscribes to.
#[tokio::test]
#[traced_test]
async fn connecting_twice_to_a_peer_reuses_the_session() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let first = tokio::time::timeout(TIMEOUT, alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("timed out dialling")
        .expect("failed to dial");
    let second = tokio::time::timeout(TIMEOUT, alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("timed out on the second dial")
        .expect("the second dial failed");

    assert_eq!(
        first.conn().stable_id(),
        second.conn().stable_id(),
        "the second connect opened a second connection",
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Concurrent calls for one peer coalesce onto a single dial, which the reuse
/// above cannot cover: neither caller can find a session to reuse, because
/// neither has finished opening one.
#[tokio::test]
#[traced_test]
async fn concurrent_connects_to_a_peer_share_one_dial() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let addr = bob.endpoint.addr();
    let (first, second) = tokio::time::timeout(
        TIMEOUT,
        futures_lite::future::zip(alice.moq.connect(addr.clone()), alice.moq.connect(addr)),
    )
    .await
    .expect("timed out dialling");

    let first = first.expect("failed to dial");
    let second = second.expect("the concurrent dial failed");
    assert_eq!(
        first.conn().stable_id(),
        second.conn().stable_id(),
        "two concurrent connects opened two connections",
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// The accepting side reaches its session too, which is what a node answering a
/// call needs: it never dialled, so `connect` is not how it gets there.
#[tokio::test]
#[traced_test]
async fn an_accepted_session_reaches_the_incoming_stream() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let mut incoming = bob.moq.incoming_sessions();
    let dialed = tokio::time::timeout(TIMEOUT, alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("timed out dialling")
        .expect("failed to dial");

    let accepted = tokio::time::timeout(TIMEOUT, incoming.next())
        .await
        .expect("timed out waiting for the incoming session")
        .expect("the incoming session stream ended");

    assert_eq!(accepted.remote_id(), alice.endpoint.id());
    assert!(dialed.dialed(), "the dialling side should report dialled");
    assert!(
        !accepted.dialed(),
        "the accepting side should not report dialled",
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Shutting the transport down closes the sessions it holds and opens no more,
/// rather than handing out a session on a connection that is going away.
#[tokio::test]
#[traced_test]
async fn shutdown_closes_sessions_and_refuses_new_ones() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let bob_addr = bob.endpoint.addr();

    let session = tokio::time::timeout(TIMEOUT, alice.moq.connect(bob_addr.clone()))
        .await
        .expect("timed out dialling")
        .expect("failed to dial");

    alice.moq.shutdown();

    tokio::time::timeout(TIMEOUT, session.closed())
        .await
        .expect("the session did not close when the transport shut down");

    let err = tokio::time::timeout(TIMEOUT, alice.moq.connect(bob_addr))
        .await
        .expect("connecting after shutdown hung")
        .expect_err("connecting after shutdown should fail");
    assert!(
        matches!(err, iroh_moq::Error::ShutDown { .. }),
        "expected a shutdown error, got {err:#}",
    );

    alice.shutdown().await;
    bob.shutdown().await;
}
