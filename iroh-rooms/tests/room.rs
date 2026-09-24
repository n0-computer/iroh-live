//! Integration tests for rooms over real QUIC connections: membership, on-demand
//! subscription, chat, leaving, and the privacy of room broadcasts.
//!
//! Nothing here touches media: broadcasts carry a plain data track with
//! hand-written frames, since `iroh-rooms` does not depend on the media crate.

mod common;

use std::time::{Duration, SystemTime};

use common::{Peer, TIMEOUT, two_peers_in_room, wait_for_state};
use iroh_moq::Reach;
use iroh_rooms::{ChatError, Error, RoomTicket};
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

    // And the name is free to publish again, and comes back for the others.
    let (again, _writer) = counter_broadcast();
    room_a.publish("cam", &again).expect("publish again");
    wait_for_state(&room_b, "a's cam is back", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;

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
    // Neither at its path, nor at the older layout's path, nor the room's
    // chat, for longer than a direct subscribe waits before trying an alias.
    let session = tokio::time::timeout(TIMEOUT, outsider.moq.connect(a))
        .await
        .expect("timed out")
        .expect("connect");
    let topic = room_a.ticket().topic_id();
    let window = Duration::from_secs(3);
    let (private, legacy, chat) = tokio::join!(
        tokio::time::timeout(
            window,
            outsider.moq.subscribe(publication.path(), Reach::Direct)
        ),
        tokio::time::timeout(window, session.subscribe(format!("rooms/{topic}/cam"))),
        tokio::time::timeout(
            window,
            session.subscribe(format!("rooms/{topic}/{a}/.chat"))
        ),
    );
    assert!(
        private.is_err(),
        "a peer outside the room resolved a room broadcast"
    );
    assert!(legacy.is_err(), "the older layout's path leaked");
    assert!(chat.is_err(), "the room's chat leaked");

    outsider.shutdown().await;
    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Names starting with a dot are the room's own.
#[tokio::test]
#[traced_test]
async fn reserved_names_are_refused() {
    let peer = Peer::spawn().await;
    let room = peer.join(&RoomTicket::generate(), "solo").await;
    let (cam, _writer) = counter_broadcast();
    for name in ["", ".chat", ".x", "someone/cam"] {
        let err = room.publish(name, &cam).expect_err("a reserved name");
        assert!(matches!(err, Error::InvalidName { .. }), "{err:#}");
    }
    peer.shutdown().await;
}

/// A frame of the `chat.v2` track as the room writes it, for forging one.
#[derive(serde::Serialize)]
struct ChatFrame {
    text: String,
    sent_at_ms: u64,
    writer: u64,
}

/// A member cannot put words in another member's mouth: chat is read over each
/// member's own session, so a broadcast a third member places at another's
/// chat path is never read as theirs, and the room refuses a name that would
/// reach there.
#[tokio::test]
#[traced_test]
async fn a_member_cannot_forge_anothers_chat() {
    let alice = Peer::spawn().await;
    let room_a = alice.join(&RoomTicket::generate(), "alice").await;
    let mallory = Peer::spawn().await;
    let room_m = mallory.join(&room_a.ticket(), "mallory").await;
    let bob = Peer::spawn().await;
    let topic = room_a.ticket().topic_id();

    // Mallory writes a chat broadcast at the path of Bob's chat before Bob is
    // even there, to everyone who connects.
    let forged = broadcast::Info::new().produce();
    let mut track = forged
        .create_track(
            "chat.v2",
            track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    let _writer = AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            let sent_at_ms = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("after the epoch")
                .as_millis() as u64;
            let frame = ChatFrame {
                text: "forged".into(),
                sent_at_ms,
                writer: 1,
            };
            let bytes = postcard::to_stdvec(&frame).expect("encode");
            if track.write_frame(Timestamp::now(), bytes).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }));
    let _forged = mallory
        .moq
        .publish_at(
            format!("rooms/{topic}/{}/.chat", bob.id()),
            &forged,
            iroh_moq::Audience::Everyone,
        )
        .expect("publish at bob's chat path");
    let err = room_m
        .publish(&format!("{}/.chat", bob.id()), &forged)
        .expect_err("a name reaching into another member's path");
    assert!(matches!(err, Error::InvalidName { .. }), "{err:#}");
    wait_for_state(&room_a, "alice sees mallory", |state| {
        state.peers.contains_key(&mallory.id())
    })
    .await;

    let mut chat_a = room_a.chat();
    let room_b = bob.join(&room_a.ticket(), "bob").await;
    wait_for_state(&room_a, "alice sees bob", |state| {
        state.peers.contains_key(&bob.id())
    })
    .await;
    tokio::time::timeout(TIMEOUT, async {
        loop {
            room_b.send_chat("real").await.expect("send");
            if let Ok(Ok(message)) =
                tokio::time::timeout(Duration::from_millis(500), chat_a.recv()).await
            {
                assert_eq!(
                    message.text, "real",
                    "{} said {:?}",
                    message.from, message.text
                );
                return;
            }
        }
    })
    .await
    .expect("bob's chat never arrived");
    // A while longer, for a reader that would have picked the forged one.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok(message)) = tokio::time::timeout_at(deadline, chat_a.recv()).await {
        assert_ne!(
            message.text, "forged",
            "forged chat arrived as {}",
            message.from
        );
    }

    room_b.leave().await;
    bob.shutdown().await;
    mallory.shutdown().await;
    alice.shutdown().await;
}

/// A chat message arrives once: a member changing what it publishes does not
/// replay its last message, and a member joining does not get what was said
/// before it joined.
#[tokio::test]
#[traced_test]
async fn chat_is_not_replayed() {
    let alice = Peer::spawn().await;
    let room_a = alice.join(&RoomTicket::generate(), "alice").await;
    room_a.send_chat("before bob joined").await.expect("send");
    // Older than the room's allowance for clocks that disagree.
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let bob = Peer::spawn().await;
    let room_b = bob.join(&room_a.ticket(), "bob").await;
    let mut chat_b = room_b.chat();
    let a = alice.id();
    wait_for_state(&room_b, "b sees a", |state| state.peers.contains_key(&a)).await;

    let first = tokio::time::timeout(TIMEOUT, async {
        loop {
            room_a.send_chat("hello").await.expect("send");
            if let Ok(Ok(message)) =
                tokio::time::timeout(Duration::from_millis(500), chat_b.recv()).await
            {
                return message;
            }
        }
    })
    .await
    .expect("no chat arrived");
    assert_eq!(first.text, "hello", "a message from before joining arrived");
    while let Ok(Ok(message)) =
        tokio::time::timeout(Duration::from_millis(500), chat_b.recv()).await
    {
        assert_eq!(message.text, "hello");
    }

    // Publishing changes Alice's announcement; Bob must not hear her again.
    let (cam, _writer) = counter_broadcast();
    room_a.publish("cam", &cam).expect("publish");
    wait_for_state(&room_b, "b sees a's cam", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), chat_b.recv())
            .await
            .is_err(),
        "the last message was delivered again"
    );
    room_a.send_chat("after").await.expect("send");
    let message = tokio::time::timeout(TIMEOUT, chat_b.recv())
        .await
        .expect("timed out")
        .expect("a message");
    assert_eq!(message.text, "after");

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A member renaming itself is seen by the others.
#[tokio::test]
#[traced_test]
async fn a_new_display_name_is_seen() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let a = peer_a.id();
    room_a.set_display_name(Some("ada".into()));
    wait_for_state(&room_b, "b sees a's new name", |state| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.display_name.as_deref() == Some("ada"))
    })
    .await;
    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Leaving through one handle leaves for every clone of it.
#[tokio::test]
#[traced_test]
async fn leaving_through_one_handle_leaves_for_all() {
    let (peer_a, room_a, peer_b, _room_b) = two_peers_in_room().await;
    let other = room_a.clone();
    let mut chat = other.chat();
    room_a.leave().await;
    assert_eq!(chat.recv().await, Err(ChatError::Closed));
    let (cam, _writer) = counter_broadcast();
    let err = other
        .publish("cam", &cam)
        .expect_err("publish after leaving");
    assert!(matches!(err, Error::Left { .. }), "{err:#}");
    let err = other
        .subscribe(peer_b.id(), "cam")
        .await
        .expect_err("subscribe after leaving");
    assert!(matches!(err, Error::Left { .. }), "{err:#}");
    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// Unpublishing takes the name out of the member's announcement, and frees it.
#[tokio::test]
#[traced_test]
async fn unpublishing_takes_the_name_out_of_the_room() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let a = peer_a.id();
    let (cam, _writer) = counter_broadcast();
    let publication = room_a.publish("cam", &cam).expect("publish");
    let lists_cam = |state: &iroh_rooms::RoomState| {
        state
            .peers
            .get(&a)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    };
    wait_for_state(&room_b, "b sees a's cam", lists_cam).await;

    publication.unpublish();
    wait_for_state(&room_b, "a's cam is gone", |state| !lists_cam(state)).await;

    room_a.publish("cam", &cam).expect("publish again");
    wait_for_state(&room_b, "a's cam is back", lists_cam).await;

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}

/// A member that leaves is cut off from what it was reading.
#[tokio::test]
#[traced_test]
async fn a_member_that_leaves_is_cut_off() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let (a, b) = (peer_a.id(), peer_b.id());
    let (cam, _writer) = counter_broadcast();
    room_a.publish("cam", &cam).expect("publish");
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
    let mut reading = subscription
        .as_moq()
        .track(DATA_TRACK)
        .expect("data track")
        .subscribe(None)
        .await
        .expect("subscribe to the track");
    tokio::time::timeout(TIMEOUT, reading.recv_group())
        .await
        .expect("timed out reading")
        .expect("track failed")
        .expect("a group");

    room_b.leave().await;
    wait_for_state(&room_a, "b is gone", |state| !state.peers.contains_key(&b)).await;
    tokio::time::timeout(TIMEOUT, async {
        while let Ok(Some(_)) = reading.recv_group().await {}
    })
    .await
    .expect("a member that left kept reading");

    peer_a.shutdown().await;
    peer_b.shutdown().await;
}
