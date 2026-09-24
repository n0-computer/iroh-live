//! Integration tests for rooms over real QUIC connections: membership, on-demand
//! subscription, chat, leaving, and the privacy of room broadcasts.
//!
//! Nothing here touches media: broadcasts carry a plain data track with
//! hand-written frames, since `iroh-rooms` does not depend on the media crate.

mod common;

use std::time::{Duration, SystemTime};

use common::{Peer, TIMEOUT, two_peers_in_room, wait_for_state};
use iroh_moq::Reach;
use iroh_rooms::{ChatError, Error};
use moq_net::{Timestamp, broadcast, track};
use n0_future::task::AbortOnDropHandle;
use n0_tracing_test::traced_test;

/// The name of the plain data track used in place of a media track.
const DATA_TRACK: &str = "data";

/// A broadcast with a data track that writes an incrementing counter every
/// 20 ms, standing in for a media encoder.
fn counter_broadcast() -> (broadcast::Producer, AbortOnDropHandle<()>) {
    let broadcast = broadcast::Info::new().produce();
    let mut track = broadcast
        .create_track(DATA_TRACK, track::Info::default())
        .expect("data track");
    let writer = tokio::spawn(async move {
        for counter in 0u32.. {
            if track
                .write_frame(Timestamp::now(), counter.to_be_bytes().to_vec())
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    (broadcast, AbortOnDropHandle::new(writer))
}

/// Two members see each other, their display names, and what each publishes.
#[tokio::test]
#[traced_test]
async fn members_see_each_other_and_their_broadcasts() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let (cam_a, _writer_a) = counter_broadcast();
    let (cam_b, _writer_b) = counter_broadcast();
    room_a.publish("cam", &cam_a).expect("publish");
    room_b.publish("cam", &cam_b).expect("publish");

    let (a, b) = (peer_a.id(), peer_b.id());
    let seen_by_b = wait_for_state(&room_b, "b sees a's cam", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;
    assert_eq!(seen_by_b.peers[&a].display_name.as_deref(), Some("alice"));
    assert!(
        !seen_by_b.peers.contains_key(&b),
        "a room does not list its own member"
    );
    wait_for_state(&room_a, "a sees b's cam", |state| {
        state
            .peers
            .get(&b)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// A member subscribes to another's broadcast on demand and reads it.
#[tokio::test]
#[traced_test]
async fn a_member_subscribes_on_demand() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let (cam, _writer) = counter_broadcast();
    room_a.publish("cam", &cam).expect("publish");
    let a = peer_a.id();
    wait_for_state(&room_b, "b sees a's cam", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;

    let subscription = tokio::time::timeout(TIMEOUT, room_b.subscribe(a, "cam"))
        .await
        .expect("timed out subscribing")
        .expect("subscribe");
    let mut track = subscription
        .as_moq()
        .track(DATA_TRACK)
        .expect("data track")
        .subscribe(None)
        .await
        .expect("subscribe to the track")
        .ordered();
    let mut previous = None;
    for index in 0..5 {
        let mut group = tokio::time::timeout(TIMEOUT, track.next_group())
            .await
            .unwrap_or_else(|_| panic!("timed out on frame {index}"))
            .expect("track failed")
            .expect("track ended");
        let frame = group.read_frame().await.expect("group").expect("a frame");
        let value = u32::from_be_bytes(frame.payload[..].try_into().expect("4 bytes"));
        if let Some(previous) = previous {
            assert_eq!(value, previous + 1, "frame {index}: a contiguous counter");
        }
        previous = Some(value);
    }

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Chat reaches the other member with its sender and send time, and needs no
/// broadcast at all.
#[tokio::test]
#[traced_test]
async fn chat_reaches_the_other_member() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let a = peer_a.id();
    let mut chat_b = room_b.chat();
    wait_for_state(&room_b, "b sees a", |state| state.peers.contains_key(&a)).await;

    // The reader subscribes to A's chat once it sees A; a message sent before
    // that is not replayed, so keep sending until one lands.
    let before = SystemTime::now() - Duration::from_secs(1);
    let message = tokio::time::timeout(TIMEOUT, async {
        loop {
            room_a.send_chat("hello from alice").await.expect("send");
            if let Ok(Ok(message)) =
                tokio::time::timeout(Duration::from_millis(500), chat_b.recv()).await
            {
                return message;
            }
        }
    })
    .await
    .expect("no chat arrived");
    assert_eq!(message.from, a);
    assert_eq!(message.text, "hello from alice");
    assert!(message.sent_at >= before, "{:?}", message.sent_at);

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Ending a broadcast changes what a member publishes, not whether it is in the
/// room.
#[tokio::test]
#[traced_test]
async fn ending_a_broadcast_keeps_the_member() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let (cam, writer) = counter_broadcast();
    room_a.publish("cam", &cam).expect("publish");
    let a = peer_a.id();
    wait_for_state(&room_b, "b sees a's cam", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;

    drop(writer);
    cam.finish();
    let state = wait_for_state(&room_b, "a's cam is gone", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.is_empty())
    })
    .await;
    assert!(state.peers.contains_key(&a), "a is still a member");

    // And the name is free to publish again.
    let (again, _writer) = counter_broadcast();
    room_a.publish("cam", &again).expect("publish again");

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// A member that leaves is gone at once for the others, not after its lease
/// runs out, and its own handles stop working.
#[tokio::test]
#[traced_test]
async fn leaving_is_seen_at_once() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let a = peer_a.id();
    wait_for_state(&room_b, "b sees a", |state| state.peers.contains_key(&a)).await;

    let mut chat_a = room_a.chat();
    room_a.leave().await;
    wait_for_state(&room_b, "a is gone", |state| !state.peers.contains_key(&a)).await;

    assert_eq!(chat_a.recv().await, Err(ChatError::Closed));
    let (cam, _writer) = counter_broadcast();
    let err = room_a
        .publish("cam", &cam)
        .expect_err("publish after leaving");
    assert!(matches!(err, Error::Left { .. }), "{err:#}");
    // Idempotent.
    room_a.leave().await;

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// A room broadcast is offered to members only: a peer outside the room that
/// connects to a member does not get it, while the member's public broadcast
/// is there for it.
#[tokio::test]
#[traced_test]
async fn room_broadcasts_are_private() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let (cam, _writer) = counter_broadcast();
    let publication = room_a.publish("cam", &cam).expect("publish");
    let (public, _public_writer) = counter_broadcast();
    let public = peer_a
        .moq
        .publish("public", &public, iroh_moq::Audience::Everyone)
        .expect("publish");
    let a = peer_a.id();
    wait_for_state(&room_b, "b sees a's cam", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;
    room_b
        .subscribe(a, "cam")
        .await
        .expect("a member subscribes");

    let outsider = Peer::spawn().await;
    tokio::time::timeout(
        TIMEOUT,
        outsider.moq.subscribe(public.path(), Reach::Direct),
    )
    .await
    .expect("timed out")
    .expect("the public broadcast is there for anyone");
    let private = tokio::time::timeout(
        Duration::from_secs(2),
        outsider.moq.subscribe(publication.path(), Reach::Direct),
    )
    .await;
    assert!(
        private.is_err(),
        "a peer outside the room resolved a room broadcast"
    );

    outsider.shutdown().await;
    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Names starting with a dot are the room's own.
#[tokio::test]
#[traced_test]
async fn reserved_names_are_refused() {
    let peer = Peer::spawn().await;
    let room = peer.join(&iroh_rooms::RoomTicket::generate(), "solo").await;
    let (cam, _writer) = counter_broadcast();
    for name in ["", ".chat", ".x"] {
        let err = room.publish(name, &cam).expect_err("a reserved name");
        assert!(matches!(err, Error::InvalidName { .. }), "{err:#}");
    }
    peer.shutdown().await;
}
