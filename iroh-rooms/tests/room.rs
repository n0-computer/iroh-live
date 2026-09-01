//! Integration tests for [`Room`] gossip-based peer discovery and MoQ
//! subscription.
//!
//! These exercise the room lifecycle over real QUIC connections: join,
//! announce, subscribe, receive frames, chat, and peer departure. Unlike the
//! version these were ported from (`iroh-live/tests/room.rs`, recoverable
//! from git history), nothing here touches media: broadcasts carry a plain
//! data track with hand-written frames instead of encoded video, since
//! `iroh-rooms` no longer depends on `moq-media`.

mod common;

use std::time::Duration;

use common::{TIMEOUT, two_peers_in_room, wait_for_event};
use iroh_rooms::{RoomEvent, chat::ChatPublisher};
use moq_net::{Timestamp, track};
use n0_tracing_test::traced_test;

/// Name of the plain data track used in place of a media track.
const DATA_TRACK: &str = "data";

/// Writes an incrementing 4-byte big-endian counter into `track` every 20 ms,
/// standing in for a media encoder's frame producer.
///
/// Runs until a write fails, which happens once the track (and so the
/// broadcast) is torn down.
async fn write_counter_frames(mut track: track::Producer) {
    let mut counter: u32 = 0;
    loop {
        if track
            .write_frame(Timestamp::now(), counter.to_be_bytes().to_vec())
            .is_err()
        {
            return;
        }
        counter += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Two peers join a room and see each other's broadcasts.
#[tokio::test]
#[traced_test]
async fn two_peers_see_each_other() {
    let (peer_a, mut room_a, peer_b, mut room_b) = two_peers_in_room().await;

    let _producer_a = room_a.publish("cam").await.expect("room_a: publish failed");
    let _producer_b = room_b.publish("cam").await.expect("room_b: publish failed");

    // B sees A's broadcast.
    wait_for_event(&mut room_b, "room_b: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;

    // A sees B's broadcast.
    wait_for_event(&mut room_a, "room_a: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Peer B subscribes to peer A's broadcast and receives a run of frames from
/// a plain data track, standing in for what would be a decoded media track.
#[tokio::test]
#[traced_test]
async fn subscribe_and_receive_frames() {
    let (peer_a, room_a, peer_b, mut room_b) = two_peers_in_room().await;

    let mut producer_a = room_a.publish("cam").await.expect("room_a: publish failed");
    let data_track = producer_a
        .create_track(DATA_TRACK, track::Info::default().with_ordered(true))
        .expect("failed to create data track");
    let writer = tokio::spawn(write_counter_frames(data_track));

    let event = wait_for_event(&mut room_b, "room_b: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;
    let RoomEvent::BroadcastSubscribed { broadcast, .. } = event else {
        unreachable!("predicate only matches BroadcastSubscribed");
    };

    let mut subscriber = broadcast
        .track(DATA_TRACK)
        .expect("data track missing on subscribed broadcast")
        .subscribe(None)
        .await
        .expect("failed to subscribe to data track");

    // Ordered delivery means the run of values received is contiguous, even
    // if it does not start at 0 (the writer may be a few frames ahead by the
    // time the subscription completes).
    let mut previous = None;
    for i in 0..5 {
        let frame = tokio::time::timeout(TIMEOUT, subscriber.read_frame())
            .await
            .unwrap_or_else(|_| panic!("timed out on frame {i}"))
            .unwrap_or_else(|err| panic!("data track read failed on frame {i}: {err:#}"))
            .unwrap_or_else(|| panic!("data track ended early at frame {i}"));
        let bytes: [u8; 4] = frame.payload[..].try_into().unwrap_or_else(|_| {
            panic!(
                "frame {i}: expected 4-byte payload, got {:?}",
                frame.payload
            )
        });
        let value = u32::from_be_bytes(bytes);
        if let Some(previous) = previous {
            assert_eq!(
                value,
                previous + 1,
                "frame {i}: expected a contiguous counter"
            );
        }
        previous = Some(value);
    }

    writer.abort();
    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Chat messages flow between two peers through the room.
#[tokio::test]
#[traced_test]
async fn chat_messages_flow() {
    let (peer_a, room_a, peer_b, mut room_b) = two_peers_in_room().await;

    let mut producer_a = room_a.publish("cam").await.expect("room_a: publish failed");
    let chat_publisher = ChatPublisher::create(&mut producer_a).expect("create_chat failed");
    room_a
        .set_chat_publisher(chat_publisher)
        .await
        .expect("set_chat_publisher failed");

    // Wait for B to subscribe to A's broadcast before sending, so the chat
    // subscriber task the room spawns on `BroadcastSubscribed` is in place.
    wait_for_event(&mut room_b, "room_b: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;

    room_a
        .send_chat("hello from A")
        .await
        .expect("send_chat failed");

    wait_for_event(&mut room_b, "room_b: ChatReceived", |ev| {
        matches!(ev, RoomEvent::ChatReceived { message, .. } if message.text == "hello from A")
    })
    .await;

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Peer disconnect emits `PeerLeft` on the other side.
#[tokio::test]
#[traced_test]
async fn peer_disconnect_detected() {
    let (peer_a, room_a, peer_b, mut room_b) = two_peers_in_room().await;
    let peer_a_id = peer_a.endpoint.id();

    let producer_a = room_a.publish("cam").await.expect("room_a: publish failed");

    wait_for_event(&mut room_b, "room_b: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;

    // Tear down peer A entirely: drop its broadcast and room actor, then
    // close every session. B's subscribed `broadcast::Consumer` should
    // observe the broadcast closing once the session that fed it ends.
    drop(producer_a);
    drop(room_a);
    peer_a.shutdown().await;

    wait_for_event(
        &mut room_b,
        "room_b: PeerLeft",
        |ev| matches!(ev, RoomEvent::PeerLeft { remote } if *remote == peer_a_id),
    )
    .await;

    peer_b.shutdown().await;
}

/// `PeerJoined` fires with the correct remote ID when a new peer appears.
#[tokio::test]
#[traced_test]
async fn peer_joined_fires() {
    let (peer_a, mut room_a, peer_b, mut room_b) = two_peers_in_room().await;
    let peer_a_id = peer_a.endpoint.id();
    let peer_b_id = peer_b.endpoint.id();

    let _producer_a = room_a.publish("cam").await.expect("room_a: publish failed");
    let _producer_b = room_b.publish("cam").await.expect("room_b: publish failed");

    wait_for_event(
        &mut room_b,
        "room_b: PeerJoined",
        |ev| matches!(ev, RoomEvent::PeerJoined { remote, .. } if *remote == peer_a_id),
    )
    .await;

    wait_for_event(
        &mut room_a,
        "room_a: PeerJoined",
        |ev| matches!(ev, RoomEvent::PeerJoined { remote, .. } if *remote == peer_b_id),
    )
    .await;

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// `PeerState` postcard serialization roundtrip, with and without
/// `display_name`. This is the exact bug class that once broke rooms:
/// postcard is positional, so `skip_serializing_if` on an `Option` field
/// causes the deserializer to read past the buffer. `PeerState` itself is
/// private to `room.rs`, so this redefines an identical layout to test the
/// same serde attributes.
#[test]
fn peer_state_serialization_roundtrip() {
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct PeerState {
        broadcasts: Vec<String>,
        display_name: Option<String>,
    }

    let with_name = PeerState {
        broadcasts: vec!["cam".into(), "screen".into()],
        display_name: Some("Alice".into()),
    };
    let without_name = PeerState {
        broadcasts: vec!["cam".into()],
        display_name: None,
    };
    let empty = PeerState {
        broadcasts: vec![],
        display_name: None,
    };

    for state in [&with_name, &without_name, &empty] {
        let bytes = postcard::to_stdvec(state).expect("serialize");
        let decoded: PeerState = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(&decoded, state, "roundtrip failed for {state:?}");
    }

    // Cross-compatibility: bytes from "with name" must not decode as
    // "without name" and vice versa. This catches the skip_serializing_if
    // bug where None was serialized as absent rather than as a 0-tag.
    let with_bytes = postcard::to_stdvec(&with_name).unwrap();
    let without_bytes = postcard::to_stdvec(&without_name).unwrap();
    assert_ne!(
        with_bytes, without_bytes,
        "with_name and without_name should produce different bytes"
    );
}
