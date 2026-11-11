//! End-to-end tests over a real QUIC connection between two iroh endpoints.
//!
//! The sources are generated, so the tests need no camera, microphone or
//! speaker. The codecs and the transport are real.

#![cfg(feature = "media")]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::protocol::Router;
use iroh_live::{
    Node,
    calls::{Calls, HungUp},
    layout::LiveLayout,
    media::{
        AudioEncoding, AudioSource, AudioTarget, LocalBroadcast, NetworkSample, Player,
        PlayerConfig, Rate, RemoteBroadcast, VideoEncoding, VideoRendition, VideoSource, audio,
        remote_broadcast,
        video::{FrameRate, Size, decode},
    },
};
use iroh_live_node::test_util::endpoint;
use n0_tracing_test::traced_test;
use n0_watcher::Watcher as _;
use tracing::{Instrument, info_span};

/// Generous, because tests run in parallel and openh264 encodes in software.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Mounts `node` on a new router, so peers can connect to it.
///
/// Keep the router for the whole test: dropping it stops accepting.
fn serve(node: &Node) -> Router {
    node.mount(Router::builder(node.endpoint().clone())).spawn()
}

/// Creates a node on the default layout.
async fn node() -> Node {
    Node::new(endpoint().await, LiveLayout::default().config(&[]))
}

/// Publishes a new media broadcast as `name` on `node`.
fn publish(node: &Node, name: &str) -> LocalBroadcast {
    LiveLayout::default()
        .publish(node, name)
        .expect("failed to publish")
}

/// Subscribes `node` to the broadcast `publisher` publishes as `name`.
async fn subscribe(node: &Node, publisher: &Node, name: &str) -> RemoteBroadcast {
    let ticket = LiveLayout::default()
        .ticket(publisher.id(), name)
        .expect("a valid broadcast name");
    let subscription = node
        .subscribe_ticket(&ticket)
        .await
        .expect("failed to subscribe");
    remote_broadcast(&subscription)
}

/// Sets a generated pattern of `size` on `broadcast`, as one rendition.
fn set_pattern(broadcast: &LocalBroadcast, size: Size) {
    let source = VideoSource::test_pattern(size, FrameRate::new(30, 1).expect("a valid rate"))
        .expect("the pattern thread starts");
    broadcast
        .set_video(source, VideoEncoding::single(VideoRendition::new("video")))
        .expect("failed to set video");
}

/// Waits until the player has shown a frame.
async fn first_frame(player: &Player) {
    tokio::time::timeout(TIMEOUT, player.video().next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("video ended");
}

/// Decoded frames arrive with non-zero dimensions and non-decreasing timestamps.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_video() {
    let (publisher, _broadcast, _router) = async {
        let node = node().await;
        let router = serve(&node);
        let broadcast = publish(&node, "test-stream");
        set_pattern(&broadcast, Size::new(320, 240));
        (node, broadcast, router)
    }
    .instrument(info_span!("publisher"))
    .await;
    let ticket = LiveLayout::default()
        .ticket(publisher.id(), "test-stream")
        .expect("a valid broadcast name");

    let subscriber = async move {
        let node = node().await;
        let subscription = node
            .subscribe_ticket(&ticket)
            .await
            .expect("failed to subscribe");
        let player = remote_broadcast(&subscription)
            .play(PlayerConfig::default())
            .expect("failed to play");
        let mut frames = player.video();

        let mut previous = None;
        for index in 0..5 {
            let frame = tokio::time::timeout(TIMEOUT, frames.next())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for frame {index}"))
                .unwrap_or_else(|| panic!("video ended before frame {index}"));

            let size = frame.size();
            assert!(
                size.width > 0 && size.height > 0,
                "frame {index}: expected non-zero dimensions, got {size}",
            );
            if let Some(previous) = previous {
                assert!(
                    frame.timestamp >= previous,
                    "frame {index}: timestamp {:?} precedes {previous:?}",
                    frame.timestamp,
                );
            }
            previous = Some(frame.timestamp);
        }

        node
    }
    .instrument(info_span!("subscriber"))
    .await;

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Waits until `calls` lists `peer` as ringing, or no longer does.
async fn rings(calls: &Calls, peer: &Node, ringing: bool) {
    tokio::time::timeout(TIMEOUT, async {
        while calls.ringing().get().contains(&peer.id()) != ringing {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the ring never turned {ringing}"));
}

/// Starts the call service of `node`, with a test pattern on its side.
fn call_side(node: &Node) -> (Calls, LocalBroadcast) {
    let (calls, side): (Calls, LocalBroadcast) =
        Calls::new(node, &LiveLayout::default()).expect("failed to start calls");
    set_pattern(&side, Size::new(320, 240));
    (calls, side)
}

/// The call convention: a call rings, the answer plays, and a hang-up ends it.
///
/// The session stays open, so a second call over it rings again.
#[tokio::test]
#[traced_test]
async fn a_call_with_media_rings_answers_and_hangs_up() {
    let alice = node().await;
    let _alice_router = serve(&alice);
    let bob = node().await;
    let _bob_router = serve(&bob);
    let (alice_calls, _alice_side) = call_side(&alice);
    let (bob_calls, _bob_side) = call_side(&bob);

    // One call side serves both calls: each rings and hangs up by audience.
    for _ in 0..2 {
        let (at_alice, at_bob) = tokio::join!(
            async {
                tokio::time::timeout(TIMEOUT, alice_calls.call(bob.id()))
                    .await
                    .expect("timed out reaching the other side")
                    .expect("failed to reach the other side")
            },
            async {
                rings(&bob_calls, &alice, true).await;
                tokio::time::timeout(TIMEOUT, bob_calls.answer(alice.id()))
                    .await
                    .expect("timed out answering")
                    .expect("failed to answer")
            },
        );
        let player = remote_broadcast(at_bob.subscription())
            .play(PlayerConfig::default())
            .expect("failed to play");
        first_frame(&player).await;
        // Kept past the hang-up, to see Bob's side leave Alice's route table.
        let bob_side = at_alice.subscription().clone();

        // Alice hangs up: Bob's read ends and the ring stops.
        at_alice.hang_up();
        let hung_up = tokio::time::timeout(TIMEOUT, at_bob.ended())
            .await
            .expect("bob did not notice the hang-up");
        assert_eq!(hung_up, HungUp::Withdrawn);
        drop((at_bob, player));
        // The next round rings anew only once both withdrawals have arrived:
        // Alice's at Bob, and Bob's, from his end of the call, at Alice.
        rings(&bob_calls, &alice, false).await;
        tokio::time::timeout(TIMEOUT, bob_side.closed())
            .await
            .expect("bob's side stayed in alice's route table");
        drop(bob_side);
    }
    assert_eq!(alice.sessions().get().len(), 1, "the session was replaced");

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Opus audio crosses the transport and decodes.
///
/// Plays into a null output, so the test needs no audio device.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_audio() {
    let publisher = node().await;
    let _router = serve(&publisher);
    let broadcast = publish(&publisher, "av-stream");
    set_pattern(&broadcast, Size::new(320, 240));
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("failed to set audio");

    let subscriber = node().await;
    let remote = subscribe(&subscriber, &publisher, "av-stream").await;
    let output = AudioTarget::null();
    let player = remote
        .play(PlayerConfig {
            audio: Some(output.clone()),
            ..PlayerConfig::default()
        })
        .expect("failed to play");

    tokio::time::timeout(TIMEOUT, async {
        while player.stats().audio.is_none_or(|audio| audio.frames == 0) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("timed out waiting for decoded audio");

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Heavy loss in the network samples moves the player down the ladder.
///
/// The samples are made up and fed in through `set_network`.
#[tokio::test]
#[traced_test]
async fn adaptive_rendition_switching() {
    let publisher = node().await;
    let _router = serve(&publisher);
    let broadcast = publish(&publisher, "adaptive-stream");
    broadcast
        .set_video(
            VideoSource::test_pattern(Size::new(640, 480), FrameRate::new(30, 1).expect("valid"))
                .expect("the pattern thread starts"),
            VideoEncoding::ladder([
                VideoRendition {
                    bitrate: Some(Rate::from_bps(2_000_000)),
                    ..VideoRendition::new("high")
                },
                VideoRendition {
                    size: Some(Size::new(320, 240)),
                    bitrate: Some(Rate::from_bps(200_000)),
                    ..VideoRendition::new("low")
                },
            ]),
        )
        .expect("failed to set video");

    let subscriber = node().await;
    let remote = subscribe(&subscriber, &publisher, "adaptive-stream").await;

    // Replaces the signals the subscription attached. Starts healthy.
    let sample = Arc::new(Mutex::new(NetworkSample {
        rtt: Some(Duration::from_millis(20)),
        min_rtt: Some(Duration::from_millis(20)),
        loss: Some(0.0),
        delivery: Some(10_000_000),
        ..NetworkSample::default()
    }));
    let reader = sample.clone();
    remote.set_network(move || *reader.lock().expect("poisoned"));

    let mut catalog = remote.catalog();
    tokio::time::timeout(
        TIMEOUT,
        catalog.wait_for(|catalog| {
            catalog
                .as_ref()
                .is_some_and(|catalog| catalog.video.renditions.len() >= 2)
        }),
    )
    .await
    .expect("timed out waiting for both renditions")
    .expect("the broadcast is alive");

    let player = remote
        .play(PlayerConfig::default())
        .expect("failed to play");
    first_frame(&player).await;
    // Starting at the top makes the drop below a real move.
    tokio::time::timeout(
        TIMEOUT,
        player
            .status()
            .wait_for(|status| status.rendition.as_deref() == Some("high")),
    )
    .await
    .expect("timed out waiting for the top rendition")
    .expect("the player is alive");

    // A quarter of the packets lost is past the emergency threshold.
    *sample.lock().expect("poisoned") = NetworkSample {
        rtt: Some(Duration::from_millis(200)),
        min_rtt: Some(Duration::from_millis(20)),
        loss: Some(0.25),
        delivery: Some(100_000),
        ..NetworkSample::default()
    };

    // The `low` encoder starts only when subscribed, so the switch waits for
    // an openh264 open and a keyframe. That is slow under a parallel test run.
    let mut status = player.status();
    tokio::time::timeout(
        TIMEOUT,
        status.wait_for(|status| status.rendition.as_deref() == Some("low")),
    )
    .await
    .expect("timed out waiting for a rendition downgrade to `low`")
    .expect("the player is alive");

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Changing the decoder backend while playing rebuilds the decoder.
///
/// Uses the software backend because every CI host has it. Checks that the new
/// decoder produces frames, not only that it was asked for.
#[tokio::test]
#[traced_test]
async fn changing_the_decoder_backend_rebuilds_it() {
    let publisher = node().await;
    let _router = serve(&publisher);
    let broadcast = publish(&publisher, "decoder-stream");
    set_pattern(&broadcast, Size::new(320, 240));

    let subscriber = node().await;
    let remote = subscribe(&subscriber, &publisher, "decoder-stream").await;
    let player = remote
        .play(PlayerConfig::default())
        .expect("failed to play");
    first_frame(&player).await;

    // A rebuild warms up a replacement, which takes over on a decoded frame.
    // Where `Auto` runs a hardware decoder, the name changes. Where it already
    // runs openh264, only the replacement on its way shows the rebuild.
    let mut status = player.status();
    let was_software = status.borrow().decoder.as_deref() == Some("openh264");
    player.set_decoder(decode::Kind::Software);
    let rebuilt = tokio::time::timeout(TIMEOUT, async {
        if was_software {
            status
                .wait_for(|status| status.switching_to.is_some())
                .await
                .expect("the player is alive");
        }
        status
            .wait_for(|status| {
                status.switching_to.is_none() && status.decoder.as_deref() == Some("openh264")
            })
            .await
            .expect("the player is alive");
    })
    .await;
    assert!(
        rebuilt.is_ok(),
        "timed out waiting for the rebuilt decoder to take over: {:?}",
        *status.borrow()
    );
    let status = status.borrow().clone();
    assert!(status.switch_error.is_none(), "{:?}", status.switch_error);
    assert_eq!(status.decoder.as_deref(), Some("openh264"));

    let mut frames = player.video();
    tokio::time::timeout(TIMEOUT, frames.next())
        .await
        .expect("timed out waiting for a frame from the rebuilt decoder")
        .expect("video ended after the rebuild");

    publisher.shutdown().await;
    subscriber.shutdown().await;
}
