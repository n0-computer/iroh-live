//! Rooms over real QUIC connections.
//!
//! Broadcasts carry a plain data track, since rooms know nothing of media.

mod common;

use std::time::Duration;

use common::{Peer, TIMEOUT, two_peers_in_room, wait_for_state};
use iroh_live_rooms::{Error, RoomTicket};
use iroh_moq::Reach;
use moq_net::{Timestamp, broadcast, track};
use n0_future::task::AbortOnDropHandle;
use n0_tracing_test::traced_test;

/// The name of the data track.
const DATA_TRACK: &str = "data";

/// A broadcast whose data track writes a counter every 20 ms.
fn counter_broadcast() -> (broadcast::Producer, AbortOnDropHandle<()>) {
    counting_from(0)
}

/// A counter broadcast that starts at `first`, to tell two apart.
fn counting_from(first: u32) -> (broadcast::Producer, AbortOnDropHandle<()>) {
    let broadcast = broadcast::Info::new().produce();
    let mut track = broadcast
        .create_track(DATA_TRACK, track::Info::default())
        .expect("data track");
    let writer = tokio::spawn(async move {
        for counter in first.. {
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

/// Ending a broadcast leaves the member in the room.
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

/// A member that leaves is gone at once for the others, and its handles stop working.
#[tokio::test]
#[traced_test]
async fn leaving_is_seen_at_once() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let a = peer_a.id();
    wait_for_state(&room_b, "b sees a", |state| state.peers.contains_key(&a)).await;

    room_a.leave().await;
    wait_for_state(&room_b, "a is gone", |state| !state.peers.contains_key(&a)).await;

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

/// A room broadcast reaches members only, while a public one reaches anyone.
#[tokio::test]
#[traced_test]
async fn room_broadcasts_are_private() {
    let (peer_a, room_a, peer_b, room_b) = two_peers_in_room().await;
    let (cam, _writer) = counter_broadcast();
    let publication = room_a.publish("cam", &cam).expect("publish");
    let a = peer_a.id();
    // In a's own scope, outside the room.
    let (public, _public_writer) = counter_broadcast();
    let public = peer_a
        .moq
        .publish(
            format!("rooms/elsewhere/{a}/public"),
            &public,
            iroh_moq::Audience::Everyone,
        )
        .expect("publish");
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
        outsider.moq.subscribe(public.path(), Reach::Direct(a)),
    )
    .await
    .expect("timed out")
    .expect("the public broadcast is there for anyone");
    let private = tokio::time::timeout(
        Duration::from_secs(3),
        outsider.moq.subscribe(publication.path(), Reach::Direct(a)),
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

/// A broadcast name must be one path segment.
#[tokio::test]
#[traced_test]
async fn invalid_names_are_refused() {
    let peer = Peer::spawn().await;
    let room = peer.join(&RoomTicket::generate(), "solo").await;
    let (cam, _writer) = counter_broadcast();
    for name in ["", "someone/cam"] {
        let err = room.publish(name, &cam).expect_err("an invalid name");
        assert!(matches!(err, Error::InvalidName { .. }), "{err:#}");
    }
    peer.shutdown().await;
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
    room_a.leave().await;
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
    let lists_cam = |state: &iroh_live_rooms::RoomState| {
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

/// Dropping a room without leaving frees its names for a rejoin.
#[tokio::test]
#[traced_test]
async fn dropping_a_room_frees_its_names() {
    let peer = Peer::spawn().await;
    let ticket = RoomTicket::generate();
    let room = peer.join(&ticket, "alice").await;
    let (cam, _writer) = counter_broadcast();
    let publication = room.publish("cam", &cam).expect("publish");

    drop(room);
    tokio::time::timeout(TIMEOUT, publication.withdrawn())
        .await
        .expect("the dropped room's broadcast stayed published");
    let room = peer.join(&ticket, "alice").await;
    room.publish("cam", &cam).expect("publish after rejoining");

    peer.shutdown().await;
}

/// A counter offset that tells a forged broadcast's frames from the real one's.
const FORGED: u32 = 1_000_000;

/// A member cannot publish at another member's room path.
///
/// Each member's grant keeps it to its own room scope, so the forgery never
/// enters the table, and the room reads the real broadcast.
#[tokio::test]
#[traced_test]
async fn a_member_cannot_forge_anothers_broadcast() {
    let (peer_b, room_b, peer_c, room_c) = two_peers_in_room().await;
    let mallory = Peer::spawn().await;
    let _room_m = mallory.join(&room_b.ticket(), "mallory").await;
    let b = peer_b.id();
    let path = format!("rooms/{}/{b}/cam", room_b.ticket().topic_id());
    let (forged, _forger) = counting_from(FORGED);
    let _forgery = mallory
        .moq
        .publish(path.as_str(), &forged, iroh_moq::Audience::Everyone)
        .expect("publish at b's path");
    tokio::time::timeout(TIMEOUT, mallory.moq.connect(peer_c.endpoint.addr()))
        .await
        .expect("timed out connecting")
        .expect("connect");

    let routed = tokio::time::timeout(
        Duration::from_secs(2),
        peer_c.moq.origin().routed(path.as_str()),
    )
    .await;
    assert!(routed.is_err(), "the forgery reached c's table");

    let (cam, _writer) = counter_broadcast();
    room_b.publish("cam", &cam).expect("publish");
    wait_for_state(&room_c, "c sees b's cam", |state| {
        state
            .peers
            .get(&b)
            .is_some_and(|peer| peer.broadcasts.contains("cam"))
    })
    .await;
    let subscription = tokio::time::timeout(TIMEOUT, room_c.subscribe(b, "cam"))
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
    let mut group = tokio::time::timeout(TIMEOUT, track.next_group())
        .await
        .expect("timed out reading")
        .expect("track failed")
        .expect("track ended");
    let frame = group.read_frame().await.expect("group").expect("a frame");
    let value = u32::from_be_bytes(frame.payload[..].try_into().expect("4 bytes"));
    assert!(value < FORGED, "c read the forgery");

    mallory.shutdown().await;
    peer_b.shutdown().await;
    peer_c.shutdown().await;
}
