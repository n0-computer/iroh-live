//! Shared harness for the rooms tests.

#![allow(dead_code, reason = "each test file only uses a subset of the harness")]

use std::time::Duration;

use iroh_live_node::{
    Grant, Layout, NodeConfig,
    policy::{self, PeerRequest},
    test_util::TestNode,
};
use iroh_live_rooms::{Room, RoomState, RoomTicket, Rooms, RoomsLayout};
use n0_watcher::Watcher;

/// Generous, because the suite shares a machine with whatever else is running.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// A [`TestNode`] with rooms, whose router accepts both.
#[derive(Debug, derive_more::Deref)]
pub(crate) struct Peer {
    #[deref]
    test: TestNode,
    pub(crate) rooms: Rooms,
}

impl Peer {
    /// Spawns a peer whose peers may publish only under their room scope.
    pub(crate) async fn spawn() -> Self {
        let config = NodeConfig::default().with_policy(policy::from_fn(|request: &PeerRequest| {
            let scope = RoomsLayout.scope(request.peer());
            Ok(Grant::publish_under(scope).into())
        }));
        let mut rooms = None;
        let test = TestNode::spawn_with(config, |node, router| {
            rooms.insert(Rooms::new(node)).mount(router)
        })
        .await;
        Self {
            test,
            rooms: rooms.expect("mounted"),
        }
    }

    /// Joins the room `ticket` names under `name`.
    pub(crate) async fn join(&self, ticket: &RoomTicket, name: &str) -> Room {
        tokio::time::timeout(TIMEOUT, self.rooms.join(ticket, Some(name.to_owned())))
            .await
            .expect("timed out joining")
            .expect("failed to join the room")
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
