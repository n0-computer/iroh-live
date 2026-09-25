//! End-to-end tests over a real QUIC connection between two iroh endpoints.
//!
//! The sources are generated, so the tests need no camera, microphone or
//! speaker. The codecs and the transport are real.

#![cfg(feature = "media")]

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use iroh::{Endpoint, EndpointId, address_lookup::MemoryLookup, endpoint::presets};
use iroh_live::{Audience, BroadcastTicket, CALL, Live, Publication, Reach, Subscription};
use iroh_live_media::{
    AudioEncoding, AudioOutput, AudioSource, Bitrate, LocalBroadcast, NetworkSample, Player,
    PlayerConfig, RemoteBroadcast, VideoEncoding, VideoRendition, VideoSource, audio,
    video::{Rate, Size, decode},
};
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher as _};
use tracing::{Instrument, info_span};

/// Generous, because tests run in parallel and openh264 encodes in software.
const TIMEOUT: Duration = Duration::from_secs(30);

async fn endpoint() -> Endpoint {
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

/// Publishes a new media broadcast as `name` on `live`.
fn publish(live: &Live, name: &str) -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    live.publish(name, &broadcast).expect("failed to publish");
    broadcast
}

/// Subscribes `live` to the broadcast `publisher` publishes as `name`.
async fn subscribe(live: &Live, publisher: &Live, name: &str) -> RemoteBroadcast {
    let subscription = live
        .subscribe(&BroadcastTicket::new(publisher.endpoint().id(), name))
        .await
        .expect("failed to subscribe");
    live.remote_broadcast(&subscription)
}

/// Sets a generated pattern of `size` on `broadcast`, as one rendition.
fn set_pattern(broadcast: &LocalBroadcast, size: Size) {
    let source = VideoSource::test_pattern(size, Rate::new(30, 1).expect("a valid rate"));
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
    let (publisher, _broadcast) = async {
        let live = Live::builder(endpoint().await).with_router().spawn();
        let broadcast = publish(&live, "test-stream");
        set_pattern(&broadcast, Size::new(320, 240));
        (live, broadcast)
    }
    .instrument(info_span!("publisher"))
    .await;
    let ticket = BroadcastTicket::new(publisher.endpoint().id(), "test-stream");

    let subscriber = async move {
        let live = Live::builder(endpoint().await).spawn();
        let subscription = live.subscribe(&ticket).await.expect("failed to subscribe");
        let player = live
            .remote_broadcast(&subscription)
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

        live
    }
    .instrument(info_span!("subscriber"))
    .await;

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Waits until `updates` announces `peer`'s `call` path (`active`) or retracts it.
async fn ringing(updates: &mut moq_net::announce::Consumer, peer: &Live, active: bool) {
    let path = BroadcastTicket::new(peer.endpoint().id(), CALL).path();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let update = updates.next().await.expect("the route table closed");
            if update.prefix == path && update.kind.is_active() == active {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the ring never turned {active}"));
}

/// Offers `side` as `live`'s call side to `peer` only.
fn offer(
    live: &Live,
    side: &LocalBroadcast,
    peer: &Live,
) -> (Publication, Watchable<BTreeSet<EndpointId>>) {
    let audience = Watchable::new(BTreeSet::from([peer.endpoint().id()]));
    let publication = live
        .moq()
        .publish(
            live.ticket(CALL).path(),
            side,
            Audience::Peers(audience.watch()),
        )
        .expect("failed to offer");
    (publication, audience)
}

/// Subscribes `live` to `peer`'s call side.
async fn answer(live: &Live, peer: &Live) -> Subscription {
    let path = BroadcastTicket::new(peer.endpoint().id(), CALL).path();
    tokio::time::timeout(
        TIMEOUT,
        live.moq()
            .subscribe(path, Reach::Direct(peer.endpoint().id())),
    )
    .await
    .expect("timed out reaching the other side")
    .expect("failed to reach the other side")
}

/// The call convention: an offer rings, the answer plays, and a hang-up ends it.
///
/// The session stays open, so a second call over it rings again.
#[tokio::test]
#[traced_test]
async fn a_call_rings_answers_and_hangs_up() {
    let alice = Live::builder(endpoint().await).with_router().spawn();
    let bob = Live::builder(endpoint().await).with_router().spawn();
    let (alice_side, bob_side) = (LocalBroadcast::new(), LocalBroadcast::new());
    set_pattern(&alice_side, Size::new(320, 240));
    set_pattern(&bob_side, Size::new(320, 240));
    let mut bob_table = bob.moq().origin().announced();

    for _ in 0..2 {
        let alice_offer = offer(&alice, &alice_side, &bob);
        let dialing = tokio::spawn({
            let (alice, bob) = (alice.clone(), bob.clone());
            async move { answer(&alice, &bob).await }
        });
        ringing(&mut bob_table, &alice, true).await;
        let bob_offer = offer(&bob, &bob_side, &alice);
        let at_bob = answer(&bob, &alice).await;
        let at_alice = dialing.await.expect("the dial panicked");
        let player = bob
            .remote_broadcast(&at_bob)
            .play(PlayerConfig::default())
            .expect("failed to play");
        first_frame(&player).await;

        // Alice hangs up: Bob's read ends and the ring stops.
        alice_offer.0.unpublish();
        tokio::time::timeout(TIMEOUT, async {
            while !at_bob.as_moq().is_closed() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("bob did not notice the hang-up");
        ringing(&mut bob_table, &alice, false).await;
        bob_offer.0.unpublish();
        drop((at_alice, player));
    }
    assert_eq!(
        alice.moq().sessions().get().len(),
        1,
        "the session was replaced"
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Opus audio crosses the transport and decodes.
///
/// Plays into a null output, so the test needs no audio device.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_audio() {
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publish(&publisher, "av-stream");
    set_pattern(&broadcast, Size::new(320, 240));
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("failed to set audio");

    let subscriber = Live::builder(endpoint().await).spawn();
    let remote = subscribe(&subscriber, &publisher, "av-stream").await;
    let output = AudioOutput::null();
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
/// The samples are made up and fed in through `with_network`.
#[tokio::test]
#[traced_test]
async fn adaptive_rendition_switching() {
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publish(&publisher, "adaptive-stream");
    broadcast
        .set_video(
            VideoSource::test_pattern(Size::new(640, 480), Rate::new(30, 1).expect("valid")),
            VideoEncoding::ladder([
                VideoRendition {
                    bitrate: Some(Bitrate::from_bps(2_000_000)),
                    ..VideoRendition::new("high")
                },
                VideoRendition {
                    size: Some(Size::new(320, 240)),
                    bitrate: Some(Bitrate::from_bps(200_000)),
                    ..VideoRendition::new("low")
                },
            ]),
        )
        .expect("failed to set video");

    let subscriber = Live::builder(endpoint().await).spawn();
    let remote = subscribe(&subscriber, &publisher, "adaptive-stream").await;

    // Replaces the signals the subscription attached. Starts healthy.
    let sample = Arc::new(Mutex::new(NetworkSample {
        rtt: Some(Duration::from_millis(20)),
        min_rtt: Some(Duration::from_millis(20)),
        loss: Some(0.0),
        delivery: Some(Bitrate::from_bps(10_000_000)),
        ..NetworkSample::default()
    }));
    let reader = sample.clone();
    let remote = remote.with_network(move || *reader.lock().expect("poisoned"));

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
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("timed out waiting for the top rendition")
        .expect("a healthy link starts at the top");

    // A quarter of the packets lost is past the emergency threshold.
    *sample.lock().expect("poisoned") = NetworkSample {
        rtt: Some(Duration::from_millis(200)),
        min_rtt: Some(Duration::from_millis(20)),
        loss: Some(0.25),
        delivery: Some(Bitrate::from_bps(100_000)),
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
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publish(&publisher, "decoder-stream");
    set_pattern(&broadcast, Size::new(320, 240));

    let subscriber = Live::builder(endpoint().await).spawn();
    let remote = subscribe(&subscriber, &publisher, "decoder-stream").await;
    let player = remote
        .play(PlayerConfig::default())
        .expect("failed to play");
    first_frame(&player).await;

    // `Auto` may already run openh264, so the decoder's name alone proves
    // nothing. A rebuild warms up a replacement, which takes over on a decoded
    // frame.
    let mut status = player.status();
    player.set_decoder(decode::Kind::Software);
    tokio::time::timeout(TIMEOUT, async {
        status
            .wait_for(|status| status.switching_to.is_some())
            .await
            .expect("the player is alive");
        status
            .wait_for(|status| status.switching_to.is_none())
            .await
            .expect("the player is alive");
    })
    .await
    .expect("timed out waiting for the rebuilt decoder to take over");
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
