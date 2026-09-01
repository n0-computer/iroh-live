//! End-to-end tests over a real QUIC connection between two iroh endpoints.
//!
//! Every source here is generated rather than captured, so the tests need no
//! camera, microphone, or speaker. The codecs are real: openh264 encodes and
//! decodes, Opus encodes and decodes, and the bytes cross an actual transport.

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets};
use iroh_live::Live;
use moq_media::{
    adaptive::AdaptiveConfig, net::NetworkSignals, publish::VideoRendition, test_source,
    video::Size,
};
use n0_tracing_test::traced_test;
use tokio::sync::watch;
use tracing::{Instrument, info_span};

/// Generous, because the whole workspace test suite may be running in parallel
/// and openh264 is a software encoder.
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

/// Publishes video from one node and subscribes from another, checking that
/// decoded frames arrive with sane dimensions and non-decreasing timestamps.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_video() {
    let (publisher, _broadcast) = async {
        let live = Live::builder(endpoint().await).with_router().spawn();
        let broadcast = live.publish("test-stream").expect("failed to publish");
        broadcast
            .video()
            .set(test_source::video(Size::new(320, 240), 30))
            .expect("failed to set video");
        (live, broadcast)
    }
    .instrument(info_span!("publisher"))
    .await;
    let publisher_addr = publisher.endpoint().addr();

    let subscriber = async move {
        let live = Live::builder(endpoint().await).spawn();
        let sub = live
            .subscribe(publisher_addr, "test-stream")
            .await
            .expect("failed to subscribe");

        let track = tokio::time::timeout(TIMEOUT, sub.broadcast().video())
            .await
            .expect("timed out waiting for the video catalog")
            .expect("failed to open the video track");

        let mut previous = None;
        for index in 0..5 {
            let frame = tokio::time::timeout(TIMEOUT, track.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for frame {index}"))
                .unwrap_or_else(|| panic!("video track closed before frame {index}"));

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

/// Publishes audio alongside video and decodes the audio track directly.
///
/// The decode consumer rather than the playback engine, so the test needs no
/// output device: what it proves is that Opus packets crossed the transport and
/// decoded, which is the part that can break.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_audio() {
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publisher.publish("av-stream").expect("failed to publish");
    broadcast
        .video()
        .set(test_source::video(Size::new(320, 240), 30))
        .expect("failed to set video");
    broadcast
        .audio()
        .set(test_source::audio(440.0, 48_000, 1))
        .expect("failed to set audio");

    let subscriber = Live::builder(endpoint().await).spawn();
    let sub = subscriber
        .subscribe(publisher.endpoint().addr(), "av-stream")
        .await
        .expect("failed to subscribe");
    let remote = sub.broadcast();

    // Wait for the publisher to advertise audio; the two tracks are registered
    // by independent tasks, so the first catalog may carry only one of them.
    let rendition = tokio::time::timeout(TIMEOUT, async {
        loop {
            let catalog = remote.catalog();
            if let Some(name) = catalog.first_audio() {
                return (name.to_string(), catalog.audio()[name].clone());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("timed out waiting for an audio rendition");

    let mut audio = moq_media::audio::decode::Consumer::new(
        remote.consumer(),
        &rendition.1,
        rendition.0,
        moq_media::audio::decode::Config::new(),
    )
    .await
    .expect("failed to open the audio decoder");

    let mut samples = 0usize;
    while samples == 0 {
        let frame = tokio::time::timeout(TIMEOUT, audio.read())
            .await
            .expect("timed out waiting for audio")
            .expect("audio decode failed")
            .expect("audio track ended before any samples");
        samples += frame.data.len();
    }

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Publishes two renditions and drives the subscriber's adaptation with
/// generated transport signals: heavy loss should move it down the ladder.
#[tokio::test]
#[traced_test]
async fn adaptive_rendition_switching() {
    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publisher
        .publish("adaptive-stream")
        .expect("failed to publish");
    broadcast
        .video()
        .set_renditions(
            test_source::video(Size::new(640, 480), 30),
            vec![
                VideoRendition::new("high").with_bitrate(2_000_000),
                VideoRendition::new("low")
                    .with_size(Size::new(320, 240))
                    .with_bitrate(200_000),
            ],
        )
        .expect("failed to set video");

    let subscriber = Live::builder(endpoint().await).spawn();
    let sub = subscriber
        .subscribe(publisher.endpoint().addr(), "adaptive-stream")
        .await
        .expect("failed to subscribe");
    let remote = sub.broadcast();

    tokio::time::timeout(TIMEOUT, async {
        while remote.catalog().video().len() < 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("timed out waiting for both renditions");

    let track = remote
        .video()
        .await
        .expect("failed to open the video track");
    let started = track.rendition();

    // Short holds, so a switch happens inside the test's own timeout rather
    // than after the four-second upgrade hold a real link wants.
    let config = AdaptiveConfig {
        downgrade_hold: Duration::from_millis(100),
        upgrade_hold: Duration::from_millis(200),
        check_interval: Duration::from_millis(50),
        ..AdaptiveConfig::default()
    };
    let (signals, receiver) = watch::channel(NetworkSignals {
        rtt: Duration::from_millis(20),
        loss_rate: 0.0,
        available_bps: 10_000_000,
        congestion_events: 0,
    });
    track.enable_adaptation_with(receiver, config);

    tokio::time::timeout(TIMEOUT, track.recv())
        .await
        .expect("timed out waiting for the first frame")
        .expect("video track closed");

    // A quarter of the packets lost is an emergency drop, not a gradual one.
    signals.send_replace(NetworkSignals {
        rtt: Duration::from_millis(200),
        loss_rate: 0.25,
        available_bps: 100_000,
        congestion_events: 1,
    });

    // The replacement encoder only starts once someone subscribes to it, so the
    // switch waits on an openh264 open plus a keyframe. That is fast alone and
    // not fast under a full parallel test suite, hence the same generous bound
    // the rest of the file uses.
    let switched = tokio::time::timeout(TIMEOUT, async {
        loop {
            if track.rendition() != started {
                return track.rendition();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("timed out waiting for a rendition downgrade");

    assert_eq!(switched, "low", "expected the downgrade to land on `low`");

    publisher.shutdown().await;
    subscriber.shutdown().await;
}
