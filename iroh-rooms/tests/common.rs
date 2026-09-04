//! Shared harness for `iroh-rooms` integration tests.
//!
//! Builds a peer from scratch (endpoint, router, MoQ transport, gossip) since
//! `iroh-rooms` no longer depends on `iroh_live::Live` for any of it. This file
//! carries no `#[test]` items of its own; it becomes its own (empty) test
//! binary under cargo's test autodiscovery, which is expected.

#![allow(dead_code, reason = "each test file only uses a subset of the harness")]

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets, protocol::Router};
use iroh_gossip::{Gossip, TopicId};
use iroh_moq::Moq;
use iroh_rooms::{Room, RoomEvent, RoomTicket};

/// Generous timeout: must survive CPU contention when the full workspace test
/// suite runs in parallel.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// Binds an endpoint against a shared in-memory address lookup, so peers in
/// the same test process can dial each other without a real discovery
/// service.
pub(crate) async fn endpoint() -> Endpoint {
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

/// A fully wired peer: an endpoint, a router accepting both MoQ and gossip
/// connections, the MoQ transport, and gossip itself.
///
/// This is the minimum an application needs to hand [`Room::new`] its three
/// arguments; `iroh-rooms` builds none of it itself.
#[derive(Debug)]
pub(crate) struct Peer {
    pub(crate) endpoint: Endpoint,
    pub(crate) moq: Moq,
    pub(crate) gossip: Gossip,
    router: Router,
}

impl Peer {
    /// Binds a fresh endpoint and wires up MoQ and gossip on top of it.
    pub(crate) async fn spawn() -> Self {
        let endpoint = endpoint().await;
        let moq = Moq::new(endpoint.clone());
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(iroh_moq::ALPN, moq.protocol_handler())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .spawn();
        Self {
            endpoint,
            moq,
            gossip,
            router,
        }
    }

    /// Joins the room named by `ticket`.
    pub(crate) async fn join_room(&self, ticket: RoomTicket) -> Room {
        Room::new(&self.endpoint, &self.moq, &self.gossip, ticket)
            .await
            .expect("failed to join room")
    }

    /// Shuts down the router and MoQ transport, then closes the endpoint.
    ///
    /// Tears down every session with this peer, which is what the
    /// disconnect-detection tests rely on to trigger `PeerLeft` on the other
    /// side.
    pub(crate) async fn shutdown(self) {
        self.moq.shutdown();
        self.router.shutdown().await.expect("router task panicked");
        self.endpoint.close().await;
    }
}

/// Creates two peers and a room shared between them behind a fresh ticket.
pub(crate) async fn two_peers_in_room() -> (Peer, Room, Peer, Room) {
    let peer_a = Peer::spawn().await;
    let ticket = RoomTicket::new(
        TopicId::from_bytes(rand::random()),
        vec![peer_a.endpoint.id()],
    );
    let room_a = peer_a.join_room(ticket.clone()).await;

    let peer_b = Peer::spawn().await;
    let room_b = peer_b.join_room(ticket).await;

    (peer_a, room_a, peer_b, room_b)
}

/// Drains events from `room` until `predicate` matches one, or `TIMEOUT`
/// elapses. Panics with `msg` on timeout or on a stream error.
///
/// Returns the matching event so callers can pull data out of it, such as the
/// `broadcast::Consumer` carried by `BroadcastSubscribed`.
pub(crate) async fn wait_for_event(
    room: &mut Room,
    msg: &str,
    mut predicate: impl FnMut(&RoomEvent) -> bool,
) -> RoomEvent {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), room.recv()).await {
            Ok(Ok(event)) if predicate(&event) => return event,
            Ok(Ok(event)) => tracing::info!("skipping event: {event:?}"),
            Ok(Err(err)) => panic!("{msg}: recv error: {err:#}"),
            Err(_) => tracing::info!("{msg}: timeout, retrying..."),
        }
    }
    panic!("{msg}: timed out after {TIMEOUT:?}");
}
