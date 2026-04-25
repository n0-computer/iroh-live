//! Integration tests for relay hints carried through the room's
//! gossip [`PeerState`] message.
//!
//! These tests verify the announce side: a publisher that calls
//! [`Room::enable_relay`](iroh_live::rooms::Room::enable_relay)
//! advertises its [`RelayOffer`] to other peers via the gossip
//! KV. Subscribers see the hint in
//! [`RoomEvent::RemoteAnnounced`](iroh_live::rooms::RoomEvent::RemoteAnnounced).
//!
//! The relay-side path (actually subscribing through a relay) is
//! covered by `iroh-live-relay/tests/relay_bridge.rs`. Here we
//! exercise the announcement and the room's reaction to relay
//! changes without requiring a real moq-relay server.

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup};
use iroh_live::{
    Live, RelayOffer,
    rooms::{Room, RoomEvent, RoomTicket},
};
use n0_tracing_test::traced_test;
use tracing::info;

const TIMEOUT: Duration = Duration::from_secs(15);

async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

fn live_with_gossip(ep: Endpoint) -> Live {
    Live::builder(ep).with_router().with_gossip().spawn()
}

async fn two_peers() -> (Live, Room, Live, Room) {
    let live_a = live_with_gossip(endpoint().await);
    let ticket = RoomTicket::new(
        iroh_gossip::TopicId::from_bytes(rand::random()),
        vec![live_a.endpoint().id()],
    );
    let room_a = Room::new(&live_a, ticket.clone()).await.expect("room_a");
    let live_b = live_with_gossip(endpoint().await);
    let room_b = Room::new(&live_b, ticket).await.expect("room_b");
    (live_a, room_a, live_b, room_b)
}

async fn wait_remote_announced_with_relay(room: &mut Room, expected_relay: Option<&RelayOffer>) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), room.recv()).await {
            Ok(Ok(RoomEvent::RemoteAnnounced { relay, .. })) => {
                if relay.as_ref() == expected_relay {
                    return;
                }
                info!(?relay, "relay hint not yet expected, continuing");
            }
            Ok(Ok(other)) => info!("skipping {other:?}"),
            Ok(Err(e)) => panic!("recv error: {e:#}"),
            Err(_) => continue,
        }
    }
    panic!("timed out waiting for relay hint match {expected_relay:?}");
}

/// A publisher's relay hint propagates to peers via gossip.
#[tokio::test]
#[traced_test]
async fn enable_relay_propagates_hint_via_gossip() {
    let (live_a, room_a, live_b, mut room_b) = two_peers().await;

    let offer = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/room/a".into(),
        jwt: Some("token-for-a".into()),
    };
    room_a.enable_relay(offer.clone()).await.expect("enable");

    wait_remote_announced_with_relay(&mut room_b, Some(&offer)).await;

    live_a.shutdown().await;
    live_b.shutdown().await;
}

/// Disabling the relay clears the hint in the next announcement.
#[tokio::test]
#[traced_test]
async fn disable_relay_clears_hint() {
    let (live_a, room_a, live_b, mut room_b) = two_peers().await;

    let offer = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/room/a".into(),
        jwt: None,
    };
    room_a.enable_relay(offer.clone()).await.expect("enable");
    wait_remote_announced_with_relay(&mut room_b, Some(&offer)).await;

    room_a.disable_relay().await.expect("disable");
    wait_remote_announced_with_relay(&mut room_b, None).await;

    live_a.shutdown().await;
    live_b.shutdown().await;
}

/// Re-enabling with a different relay swaps the advertised hint.
#[tokio::test]
#[traced_test]
async fn relay_hint_replacement_propagates() {
    let (live_a, room_a, live_b, mut room_b) = two_peers().await;

    let offer1 = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/relay/one".into(),
        jwt: None,
    };
    let offer2 = RelayOffer {
        endpoint: iroh::SecretKey::generate().public(),
        path: "/relay/two".into(),
        jwt: None,
    };
    room_a.enable_relay(offer1.clone()).await.expect("enable 1");
    wait_remote_announced_with_relay(&mut room_b, Some(&offer1)).await;
    room_a.enable_relay(offer2.clone()).await.expect("enable 2");
    wait_remote_announced_with_relay(&mut room_b, Some(&offer2)).await;

    live_a.shutdown().await;
    live_b.shutdown().await;
}
