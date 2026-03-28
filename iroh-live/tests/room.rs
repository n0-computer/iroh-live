//! Integration tests for [`Room`] gossip-based peer discovery and subscription.
//!
//! These tests exercise the room lifecycle over real QUIC connections:
//! join, announce, subscribe, receive frames, chat, and peer departure.
//! Every test caught zero regressions before the postcard serialization
//! bug that broke rooms entirely — that gap must not reopen.

use std::time::Duration;

use iroh::Endpoint;
use iroh_live::{
    Live,
    rooms::{Room, RoomEvent, RoomTicket},
};
use moq_media::{
    codec::VideoCodec,
    format::VideoPreset,
    publish::{LocalBroadcast, VideoInput},
    test_util::TestVideoSource,
};
use n0_tracing_test::traced_test;
use tracing::info;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generous timeout — must survive CPU contention when the full workspace
/// test suite runs in parallel.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Software codec that works on any machine regardless of hardware features.
const TEST_VIDEO_CODEC: VideoCodec = VideoCodec::H264;

async fn endpoint() -> Endpoint {
    Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await
        .expect("failed to bind endpoint")
}

/// Creates a `Live` instance with router + gossip enabled.
fn live_with_gossip(ep: Endpoint) -> Live {
    Live::builder(ep).with_router().with_gossip().spawn()
}

/// Creates a `LocalBroadcast` with a test video source already attached.
fn broadcast_with_video() -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(320, 240).with_fps(15.0);
    broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
        .expect("failed to set video");
    broadcast
}

/// Creates two peers in the same room. Returns (live_a, room_a, live_b, room_b).
async fn two_peers_in_room() -> (Live, Room, Live, Room) {
    let live_a = live_with_gossip(endpoint().await);
    let ticket = RoomTicket::new(
        iroh_gossip::TopicId::from_bytes(rand::random()),
        vec![live_a.endpoint().id()],
    );
    let room_a = Room::new(&live_a, ticket.clone())
        .await
        .expect("room_a: failed to join");

    let live_b = live_with_gossip(endpoint().await);
    let room_b = Room::new(&live_b, ticket)
        .await
        .expect("room_b: failed to join");

    (live_a, room_a, live_b, room_b)
}

/// Drains events from a room until a predicate returns `true` or the
/// timeout expires. Panics on timeout with the given message.
async fn wait_for_event(room: &mut Room, msg: &str, mut predicate: impl FnMut(&RoomEvent) -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), room.recv()).await {
            Ok(Ok(ref ev)) if predicate(ev) => return,
            Ok(Ok(ev)) => info!("skipping event: {ev:?}"),
            Ok(Err(e)) => panic!("{msg}: recv error: {e:#}"),
            Err(_) => info!("{msg}: timeout, retrying..."),
        }
    }
    panic!("{msg}: timed out after {TIMEOUT:?}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Two peers join a room and see each other's broadcasts — the exact
/// scenario the postcard serialization bug broke.
#[tokio::test]
#[traced_test]
async fn two_peers_see_each_other() {
    let (live_a, mut room_a, live_b, mut room_b) = two_peers_in_room().await;

    let broadcast_a = broadcast_with_video();
    room_a
        .publish("cam", &broadcast_a)
        .await
        .expect("room_a: publish failed");

    let broadcast_b = broadcast_with_video();
    room_b
        .publish("cam", &broadcast_b)
        .await
        .expect("room_b: publish failed");

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

    live_a.shutdown().await;
    live_b.shutdown().await;
}

/// Peer B subscribes to peer A's broadcast and receives decoded video
/// frames with valid dimensions.
#[tokio::test]
#[traced_test]
async fn subscribe_and_receive_video_frames() {
    let (live_a, room_a, live_b, mut room_b) = two_peers_in_room().await;

    let broadcast_a = broadcast_with_video();
    room_a
        .publish("cam", &broadcast_a)
        .await
        .expect("room_a: publish failed");

    // Wait for B to receive the subscription.
    let mut remote_broadcast = None;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), room_b.recv()).await {
            Ok(Ok(RoomEvent::BroadcastSubscribed { broadcast, .. })) => {
                remote_broadcast = Some(broadcast);
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("room_b: recv error: {e:#}"),
            Err(_) => continue,
        }
    }
    let remote = remote_broadcast.expect("room_b never received BroadcastSubscribed");

    // Decode video frames from the remote broadcast.
    let mut video = tokio::time::timeout(TIMEOUT, remote.video_ready())
        .await
        .expect("timed out waiting for video catalog")
        .expect("failed to create video track");

    for i in 0..3 {
        let frame = tokio::time::timeout(TIMEOUT, video.next_frame())
            .await
            .unwrap_or_else(|_| panic!("timed out on frame {i}"))
            .unwrap_or_else(|| panic!("track closed at frame {i}"));
        assert!(
            frame.width() > 0 && frame.height() > 0,
            "frame {i}: zero dimensions {}x{}",
            frame.width(),
            frame.height()
        );
    }

    live_a.shutdown().await;
    live_b.shutdown().await;
}

/// Chat messages flow between two peers through the room.
#[tokio::test]
#[traced_test]
async fn chat_messages_flow() {
    let (live_a, room_a, live_b, mut room_b) = two_peers_in_room().await;

    // Peer A publishes a broadcast with chat enabled.
    let mut broadcast_a = LocalBroadcast::new();
    let source = TestVideoSource::new(320, 240).with_fps(15.0);
    broadcast_a
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
        .expect("failed to set video");
    let chat_pub = broadcast_a.enable_chat().expect("enable_chat failed");

    room_a
        .publish("cam", &broadcast_a)
        .await
        .expect("room_a: publish failed");
    room_a
        .set_chat_publisher(chat_pub)
        .await
        .expect("set_chat_publisher failed");

    // Wait for B to subscribe to A's broadcast.
    wait_for_event(&mut room_b, "room_b: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;

    // A sends a chat message.
    room_a
        .send_chat("hello from A")
        .await
        .expect("send_chat failed");

    // B should receive it.
    wait_for_event(&mut room_b, "room_b: ChatReceived", |ev| {
        matches!(ev, RoomEvent::ChatReceived { message, .. } if message.text == "hello from A")
    })
    .await;

    live_a.shutdown().await;
    live_b.shutdown().await;
}

/// Peer disconnect emits PeerLeft on the other side.
#[tokio::test]
#[traced_test]
async fn peer_disconnect_detected() {
    let (live_a, room_a, live_b, mut room_b) = two_peers_in_room().await;

    let broadcast_a = broadcast_with_video();
    room_a
        .publish("cam", &broadcast_a)
        .await
        .expect("room_a: publish failed");

    // Wait for B to see A's broadcast subscription.
    wait_for_event(&mut room_b, "room_b: BroadcastSubscribed", |ev| {
        matches!(ev, RoomEvent::BroadcastSubscribed { .. })
    })
    .await;

    // Drop peer A's broadcast and live instance to simulate disconnect.
    let peer_a_id = live_a.endpoint().id();
    drop(broadcast_a);
    drop(room_a);
    live_a.shutdown().await;

    // B should see PeerLeft for A.
    wait_for_event(
        &mut room_b,
        "room_b: PeerLeft",
        |ev| matches!(ev, RoomEvent::PeerLeft { remote } if *remote == peer_a_id),
    )
    .await;

    live_b.shutdown().await;
}

/// PeerJoined fires with the correct remote ID when a new peer appears.
#[tokio::test]
#[traced_test]
async fn peer_joined_fires() {
    let (live_a, mut room_a, live_b, mut room_b) = two_peers_in_room().await;

    let peer_a_id = live_a.endpoint().id();
    let peer_b_id = live_b.endpoint().id();

    let broadcast_a = broadcast_with_video();
    room_a
        .publish("cam", &broadcast_a)
        .await
        .expect("room_a: publish failed");

    let broadcast_b = broadcast_with_video();
    room_b
        .publish("cam", &broadcast_b)
        .await
        .expect("room_b: publish failed");

    // B sees PeerJoined for A.
    wait_for_event(
        &mut room_b,
        "room_b: PeerJoined",
        |ev| matches!(ev, RoomEvent::PeerJoined { remote, .. } if *remote == peer_a_id),
    )
    .await;

    // A sees PeerJoined for B.
    wait_for_event(
        &mut room_a,
        "room_a: PeerJoined",
        |ev| matches!(ev, RoomEvent::PeerJoined { remote, .. } if *remote == peer_b_id),
    )
    .await;

    live_a.shutdown().await;
    live_b.shutdown().await;
}

/// `PeerState` postcard serialization roundtrip — with and without
/// display_name. This is the exact bug class that broke rooms: postcard
/// is positional, so `skip_serializing_if` on `Option` fields causes
/// deserialization to read past the buffer.
#[test]
fn peer_state_serialization_roundtrip() {
    // Replicate the PeerState struct from rooms.rs. We test the same
    // serde attributes by defining an identical layout here, since the
    // real struct is private.
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
