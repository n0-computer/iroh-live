//! End-to-end tests over a real QUIC connection between two iroh endpoints.
//!
//! The sources are generated, so the tests need no camera, microphone or
//! speaker. The codecs and the transport are real.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets};
use iroh_live::{BroadcastTicket, CALL, Call, Live};
use iroh_live_media::{
    AudioEncoding, AudioOutput, AudioSource, Bitrate, LocalBroadcast, NetworkSample, Player,
    PlayerConfig, RemoteBroadcast, VideoEncoding, VideoRendition, VideoSource, audio,
    video::{Rate, Size, decode},
};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher as _;
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
    live.subscribe(&BroadcastTicket::new(publisher.endpoint().id(), name))
        .await
        .expect("failed to subscribe")
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
        let remote = live.subscribe(&ticket).await.expect("failed to subscribe");
        let player = remote
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

/// Each side of a call reads the other's picture over the session one dialed.
#[tokio::test]
#[traced_test]
async fn a_call_reads_the_other_side() {
    let alice = Live::builder(endpoint().await).with_router().spawn();
    let bob = Live::builder(endpoint().await).with_router().spawn();
    let _alice_side = publish(&alice, CALL);
    let bob_side = publish(&bob, CALL);
    set_pattern(&bob_side, Size::new(320, 240));

    let call = Call::dial(&alice, bob.endpoint().id())
        .await
        .expect("failed to dial");
    assert_eq!(call.session().remote_id(), bob.endpoint().id());
    let player = call
        .remote()
        .play(PlayerConfig::default())
        .expect("failed to play");
    first_frame(&player).await;

    call.close();
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
    tokio::time::timeout(TIMEOUT, async {
        while catalog
            .get()
            .is_none_or(|catalog| catalog.video.renditions.len() < 2)
        {
            catalog.updated().await.expect("the broadcast is alive");
        }
    })
    .await
    .expect("timed out waiting for both renditions");

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
    tokio::time::timeout(TIMEOUT, async {
        while status.get().rendition.as_deref() != Some("low") {
            status.updated().await.expect("the player is alive");
        }
    })
    .await
    .expect("timed out waiting for a rendition downgrade to `low`");

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

    player.set_decoder(decode::Kind::Software);

    // A replacement takes over only on a decoded frame.
    let mut status = player.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get().decoder.as_deref() != Some("openh264") {
            status.updated().await.expect("the player is alive");
        }
    })
    .await
    .expect("timed out waiting for the software decoder to take over");

    let mut frames = player.video();
    tokio::time::timeout(TIMEOUT, frames.next())
        .await
        .expect("timed out waiting for a frame from the rebuilt decoder")
        .expect("video ended after the rebuild");

    publisher.shutdown().await;
    subscriber.shutdown().await;
}
