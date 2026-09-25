//! Shared harness for the rooms tests.

#![allow(dead_code, reason = "each test file only uses a subset of the harness")]

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets, protocol::Router};
use iroh_live_rooms::{Room, RoomState, RoomTicket, Rooms};
use iroh_moq::{Moq, MoqConfig};
use n0_watcher::Watcher;

/// Generous, because the suite shares a machine with whatever else is running.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// Binds an endpoint against a shared in-memory address lookup.
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

/// A peer with a MoQ node, rooms, and a router that accepts both.
#[derive(Debug)]
pub(crate) struct Peer {
    pub(crate) endpoint: Endpoint,
    pub(crate) moq: Moq,
    pub(crate) rooms: Rooms,
    router: Router,
}

impl Peer {
    pub(crate) async fn spawn() -> Self {
        let endpoint = endpoint().await;
        let moq = Moq::new(endpoint.clone(), MoqConfig::default());
        let rooms = Rooms::new(&moq);
        let router = moq
            .mount(Router::builder(endpoint.clone()))
            .accept(iroh_live_rooms::ALPN, rooms.protocol_handler())
            .spawn();
        Self {
            endpoint,
            moq,
            rooms,
            router,
        }
    }

    pub(crate) fn id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }

    /// Joins the room `ticket` names under `name`.
    pub(crate) async fn join(&self, ticket: &RoomTicket, name: &str) -> Room {
        tokio::time::timeout(TIMEOUT, self.rooms.join(ticket, Some(name.to_owned())))
            .await
            .expect("timed out joining")
            .expect("failed to join the room")
    }

    /// Shuts down the MoQ node and the router, then closes the endpoint.
    pub(crate) async fn shutdown(self) {
        self.moq.shutdown().await;
        self.router.shutdown().await.expect("router task panicked");
        self.endpoint.close().await;
    }
}

/// Creates two peers in one fresh room, the second bootstrapping from the first.
pub(crate) async fn two_peers_in_room() -> (Peer, Room, Peer, Room) {
    let peer_a = Peer::spawn().await;
    let room_a = peer_a.join(&RoomTicket::generate(), "alice").await;
    let peer_b = Peer::spawn().await;
    let room_b = peer_b.join(&room_a.ticket(), "bob").await;
    (peer_a, room_a, peer_b, room_b)
}

/// Waits until `room`'s state satisfies `predicate`, and returns that state.
pub(crate) async fn wait_for_state(
    room: &Room,
    what: &str,
    mut predicate: impl FnMut(&RoomState) -> bool,
) -> RoomState {
    let mut state = room.state();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let current = state.get();
            if predicate(&current) {
                return current;
            }
            state.updated().await.expect("room gone");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for: {what}"))
}
