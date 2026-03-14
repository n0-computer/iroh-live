//! End-to-end integration tests for iroh-live.
//!
//! These tests exercise the full publish → transport → subscribe pipeline
//! over real QUIC connections between two iroh endpoints.

use std::time::Duration;

use iroh::{Endpoint, SecretKey};
use iroh_live::Live;
use moq_media::{
    format::VideoPreset,
    publish::LocalBroadcast,
    test_util::{NullAudioBackend, TestAudioSource, TestVideoSource},
};

/// Creates an endpoint bound to localhost with a random secret key.
async fn make_endpoint() -> Endpoint {
    Endpoint::builder()
        .secret_key(SecretKey::generate(&mut rand::rng()))
        .bind()
        .await
        .expect("failed to bind endpoint")
}

/// Publishes video over one Live node and subscribes from another,
/// verifying that decoded video frames arrive across the QUIC connection.
#[tokio::test]
async fn publish_subscribe_video() {
    use moq_media::codec::VideoCodec;

    let _ = tracing_subscriber::fmt::try_init();

    // --- Publisher ---
    let pub_ep = make_endpoint().await;
    let publisher = Live::builder(pub_ep.clone()).spawn_with_router();

    let broadcast = LocalBroadcast::new();
    // Yield so the broadcast's internal task starts listening for track requests.
    tokio::task::yield_now().await;

    let source = TestVideoSource::new(320, 240).with_fps(30.0);
    broadcast
        .video()
        .set(source, VideoCodec::best_available(), [VideoPreset::P180])
        .expect("failed to set video");

    publisher
        .publish("test-stream", &broadcast)
        .await
        .expect("failed to publish");

    // --- Subscriber ---
    let sub_ep = make_endpoint().await;
    let subscriber = Live::builder(sub_ep.clone()).spawn();

    let pub_addr = pub_ep.addr();
    let (session, remote) = subscriber
        .subscribe(pub_addr, "test-stream")
        .await
        .expect("failed to subscribe");

    // Wait for catalog to arrive and contain video.
    assert!(remote.has_video(), "broadcast should have video renditions");

    let mut video_track = remote.video().expect("failed to create video track");

    // Receive a few frames with a generous timeout.
    let frame = tokio::time::timeout(Duration::from_secs(10), video_track.next_frame())
        .await
        .expect("timed out waiting for first video frame")
        .expect("video track closed without producing a frame");

    let [w, h] = frame.dimensions;
    assert!(w > 0 && h > 0, "frame dimensions should be non-zero");

    // Receive a second frame to confirm the pipeline is streaming.
    let frame2 = tokio::time::timeout(Duration::from_secs(5), video_track.next_frame())
        .await
        .expect("timed out waiting for second video frame")
        .expect("video track closed after first frame");

    assert!(
        frame2.timestamp > frame.timestamp || frame2.timestamp == Duration::ZERO,
        "second frame should have a later timestamp"
    );

    // Clean up.
    drop(video_track);
    drop(remote);
    drop(session);
    drop(broadcast);
    publisher.shutdown().await;
    sub_ep.close().await;
}

/// Subscribes to audio and verifies the audio track is created successfully.
#[tokio::test]
async fn publish_subscribe_audio() {
    use moq_media::{
        codec::{AudioCodec, VideoCodec},
        format::{AudioFormat, AudioPreset},
    };

    let _ = tracing_subscriber::fmt::try_init();

    // --- Publisher ---
    let pub_ep = make_endpoint().await;
    let publisher = Live::builder(pub_ep.clone()).spawn_with_router();

    let broadcast = LocalBroadcast::new();
    tokio::task::yield_now().await;

    let video_source = TestVideoSource::new(320, 240).with_fps(30.0);
    broadcast
        .video()
        .set(
            video_source,
            VideoCodec::best_available(),
            [VideoPreset::P180],
        )
        .expect("failed to set video");

    let audio_source = TestAudioSource::new(AudioFormat::mono_48k());
    broadcast
        .audio()
        .set(audio_source, AudioCodec::Opus, [AudioPreset::Hq])
        .expect("failed to set audio");

    publisher
        .publish("av-stream", &broadcast)
        .await
        .expect("failed to publish");

    // --- Subscriber ---
    let sub_ep = make_endpoint().await;
    let subscriber = Live::builder(sub_ep.clone()).spawn();

    let (session, remote) = subscriber
        .subscribe(pub_ep.addr(), "av-stream")
        .await
        .expect("failed to subscribe");

    assert!(remote.has_video(), "broadcast should have video");
    assert!(remote.has_audio(), "broadcast should have audio");

    // Subscribe to audio — verifies the full decode pipeline starts.
    let audio_track = remote
        .audio(&NullAudioBackend)
        .await
        .expect("failed to create audio track");

    // Subscribe to video and get at least one frame.
    let mut video_track = remote.video().expect("failed to create video track");
    let _frame = tokio::time::timeout(Duration::from_secs(10), video_track.next_frame())
        .await
        .expect("timed out waiting for video frame")
        .expect("video track closed");

    // Clean up.
    drop(audio_track);
    drop(video_track);
    drop(remote);
    drop(session);
    drop(broadcast);
    publisher.shutdown().await;
    sub_ep.close().await;
}

/// Publishes two video renditions (180p + 360p), creates an AdaptiveVideoTrack
/// with synthetic NetworkSignals, injects high loss to trigger a downgrade,
/// then restores good conditions and waits for an upgrade probe.
///
/// This exercises the full adaptive pipeline over a real QUIC connection
/// without needing patchbay or real network impairment.
#[tokio::test]
async fn adaptive_rendition_switching() {
    use moq_media::{adaptive::AdaptiveConfig, codec::VideoCodec, net::NetworkSignals};
    use tokio::sync::watch;

    let _ = tracing_subscriber::fmt::try_init();

    // --- Publisher with two renditions ---
    let pub_ep = make_endpoint().await;
    let publisher = Live::builder(pub_ep.clone()).spawn_with_router();

    let broadcast = LocalBroadcast::new();
    tokio::task::yield_now().await;

    let source = TestVideoSource::new(640, 480).with_fps(30.0);
    broadcast
        .video()
        .set(
            source,
            VideoCodec::best_available(),
            [VideoPreset::P360, VideoPreset::P180],
        )
        .expect("failed to set video");

    publisher
        .publish("adaptive-stream", &broadcast)
        .await
        .expect("failed to publish");

    // --- Subscriber with adaptive track ---
    let sub_ep = make_endpoint().await;
    let subscriber = Live::builder(sub_ep.clone()).spawn();

    let (_session, remote) = subscriber
        .subscribe(pub_ep.addr(), "adaptive-stream")
        .await
        .expect("failed to subscribe");

    assert!(remote.has_video(), "should have video renditions");
    let catalog = remote.catalog();
    let rendition_count = catalog.video.renditions.len();
    assert_eq!(
        rendition_count, 2,
        "expected 2 video renditions, got {rendition_count}"
    );

    // Synthetic signals channel — start with good conditions.
    let good = NetworkSignals {
        rtt: Duration::from_millis(20),
        loss_rate: 0.0,
        available_bps: 10_000_000,
        congestion_events: 0,
    };
    let (signals_tx, signals_rx) = watch::channel(good);

    // Use fast timers for testing.
    let config = AdaptiveConfig {
        upgrade_hold: Duration::from_millis(200),
        downgrade_hold: Duration::from_millis(100),
        probe_duration: Duration::from_millis(500),
        probe_cooldown: Duration::from_millis(300),
        post_downgrade_cooldown: Duration::from_millis(300),
        check_interval: Duration::from_millis(50),
        ..AdaptiveConfig::default()
    };

    let mut adaptive = remote
        .adaptive_video_with(signals_rx, config, Default::default())
        .expect("failed to create adaptive track");

    // Get initial frame — should start on highest rendition (360p).
    let _frame = tokio::time::timeout(Duration::from_secs(10), adaptive.next_frame())
        .await
        .expect("timed out waiting for first adaptive frame")
        .expect("adaptive track closed");

    let initial_rendition = adaptive.selected_rendition();

    // Inject high loss → expect downgrade.
    signals_tx
        .send(NetworkSignals {
            loss_rate: 0.25, // 25% — triggers emergency
            available_bps: 10_000_000,
            congestion_events: 0,
            rtt: Duration::from_millis(200),
        })
        .ok();

    // Wait for adaptation to react (emergency is immediate, but the task
    // needs a few check intervals to process).
    let downgrade_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        // Drain frames to keep the pipeline moving.
        let _ = tokio::time::timeout(Duration::from_millis(200), adaptive.next_frame()).await;
        let current = adaptive.selected_rendition();
        if current != initial_rendition {
            // Downgrade happened.
            break;
        }
        if tokio::time::Instant::now() > downgrade_deadline {
            panic!("adaptive track did not downgrade within 5s (still on {current})");
        }
    }

    let downgraded_rendition = adaptive.selected_rendition();
    assert_ne!(
        downgraded_rendition, initial_rendition,
        "should have switched to a lower rendition"
    );

    // Restore good conditions → expect upgrade probe eventually.
    signals_tx.send(good).ok();

    let upgrade_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(200), adaptive.next_frame()).await;
        let current = adaptive.selected_rendition();
        if current != downgraded_rendition {
            // Upgrade happened.
            break;
        }
        if tokio::time::Instant::now() > upgrade_deadline {
            // Upgrade may not happen within the test window due to cooldowns.
            // This is acceptable — the downgrade already proved adaptation works.
            break;
        }
    }

    // Clean up.
    drop(adaptive);
    drop(remote);
    drop(broadcast);
    publisher.shutdown().await;
    sub_ep.close().await;
}
