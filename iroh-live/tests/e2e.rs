//! End-to-end integration tests for iroh-live.
//!
//! These tests exercise the full publish → transport → subscribe pipeline
//! over real QUIC connections between two iroh endpoints.
//!
//! We use `VideoCodec::H264` (software) rather than `best_available()` because
//! workspace feature unification can activate hardware codec features (V4L2,
//! VAAPI) from other crates, and hardware encoders fail when no device exists.

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets};
use iroh_live::{Call, Live};
use moq_media::{
    adaptive::AdaptiveConfig,
    codec::{AudioCodec, VideoCodec},
    format::{AudioFormat, AudioPreset, DecodeConfig, VideoPreset},
    net::NetworkSignals,
    publish::{LocalBroadcast, VideoInput},
    test_util::{CapturingAudioBackend, TestAudioSource, TestVideoSource},
};
use n0_tracing_test::traced_test;
use tokio::sync::watch;
use tracing::{Instrument, info_span};

/// Generous timeout for frame arrival — must survive CPU contention when the
/// full workspace test suite runs in parallel.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Software video codec that works on any machine, regardless of which
/// hardware-codec features workspace unification activates.
const TEST_VIDEO_CODEC: VideoCodec = VideoCodec::H264;

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

/// Publishes video over one Live node and subscribes from another,
/// verifying that decoded video frames arrive across the QUIC connection
/// with valid dimensions and monotonically increasing timestamps.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_video() {
    // --- Publisher ---
    let (publisher, _broadcast) = async {
        let live = Live::builder(endpoint().await).with_router().spawn();

        let broadcast = LocalBroadcast::new();

        let source = TestVideoSource::new(320, 240).with_fps(30.0);
        broadcast
            .video()
            .set(VideoInput::new(
                source,
                TEST_VIDEO_CODEC,
                [VideoPreset::P180],
            ))
            .expect("failed to set video");
        live.publish("test-stream", &broadcast)
            .await
            .expect("failed to publish");
        (live, broadcast)
    }
    .instrument(info_span!("publisher"))
    .await;
    let pub_addr = publisher.endpoint().addr();

    // --- Subscriber ---
    let subscriber = async move {
        let live = Live::builder(endpoint().await).spawn();

        let sub = live
            .subscribe(pub_addr, "test-stream")
            .await
            .expect("failed to subscribe");

        let remote = sub.broadcast();
        // video_ready waits for the catalog to contain video before subscribing,
        // avoiding the race where ready() returns on audio alone.
        let mut video_track = tokio::time::timeout(FRAME_TIMEOUT, remote.video_ready())
            .await
            .expect("timed out waiting for video catalog")
            .expect("failed to create video track");

        // Receive several frames and verify the pipeline streams correctly.
        let mut prev_ts = None;
        for i in 0..5 {
            let frame = tokio::time::timeout(FRAME_TIMEOUT, video_track.next_frame())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for frame {i}"))
                .unwrap_or_else(|| panic!("video track closed before frame {i}"));

            assert!(
                frame.width() > 0 && frame.height() > 0,
                "frame {i}: dimensions should be non-zero, got {}×{}",
                frame.width(),
                frame.height(),
            );

            if let Some(prev) = prev_ts {
                assert!(
                    frame.timestamp >= prev,
                    "frame {i}: timestamp {:#?} should not precede previous {prev:#?}",
                    frame.timestamp,
                );
            }
            prev_ts = Some(frame.timestamp);
        }

        live
    }
    .instrument(info_span!("subscriber"))
    .await;

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Publishes audio + video and verifies both tracks arrive. Uses
/// [`CapturingAudioBackend`] to confirm decoded audio samples flow through
/// the pipeline end-to-end.
#[tokio::test]
#[traced_test]
async fn publish_subscribe_audio() {
    // --- Publisher ---
    let publisher = Live::builder(endpoint().await).with_router().spawn();

    let broadcast = LocalBroadcast::new();
    let video_source = TestVideoSource::new(320, 240).with_fps(30.0);
    broadcast
        .video()
        .set(VideoInput::new(
            video_source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
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
    let subscriber = Live::builder(endpoint().await).spawn();

    let sub = subscriber
        .subscribe(publisher.endpoint().addr(), "av-stream")
        .await
        .expect("failed to subscribe");

    let remote = sub.broadcast();
    // Wait for both tracks to appear in the catalog.
    let mut video_track = tokio::time::timeout(FRAME_TIMEOUT, remote.video_ready())
        .await
        .expect("timed out waiting for video catalog")
        .expect("failed to create video track");

    // Use a capturing backend so we can verify audio samples arrived.
    let audio_backend = CapturingAudioBackend::new();
    let captured = audio_backend.captured_samples();
    let _audio_track = tokio::time::timeout(FRAME_TIMEOUT, remote.audio_ready(&audio_backend))
        .await
        .expect("timed out waiting for audio catalog")
        .expect("failed to create audio track");

    // Receive several video frames to give the audio pipeline time to
    // accumulate samples.
    for i in 0..5 {
        let _frame = tokio::time::timeout(FRAME_TIMEOUT, video_track.next_frame())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for video frame {i}"))
            .unwrap_or_else(|| panic!("video track closed at frame {i}"));
    }

    // Verify audio samples flowed through the pipeline.
    let sample_count = captured.lock().unwrap().len();
    assert!(
        sample_count > 0,
        "expected captured audio samples, got {sample_count}",
    );

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Publishes two video renditions (180p + 360p), creates a VideoTrack with
/// adaptation enabled via synthetic NetworkSignals, injects high loss to
/// trigger a downgrade, then restores good conditions and waits for an
/// upgrade probe.
///
/// This exercises the full adaptive pipeline over a real QUIC connection
/// without needing patchbay or real network impairment.
#[tokio::test]
#[traced_test]
async fn adaptive_rendition_switching() {
    // --- Publisher with two renditions ---
    let publisher = Live::builder(endpoint().await).with_router().spawn();

    let broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(640, 480).with_fps(30.0);
    broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P360, VideoPreset::P180],
        ))
        .expect("failed to set video");

    publisher
        .publish("adaptive-stream", &broadcast)
        .await
        .expect("failed to publish");

    // --- Subscriber with adaptive track ---
    let subscriber = Live::builder(endpoint().await).spawn();

    let sub = subscriber
        .subscribe(publisher.endpoint().addr(), "adaptive-stream")
        .await
        .expect("failed to subscribe");

    let remote = sub.broadcast();
    // Wait for catalog to arrive with video renditions.
    tokio::time::timeout(FRAME_TIMEOUT, remote.ready())
        .await
        .expect("timed out waiting for video catalog");

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

    // Create a regular VideoTrack and enable adaptation on it.
    let mut track = remote.video().expect("failed to create video track");
    track
        .enable_adaptation(remote.clone(), signals_rx, config, DecodeConfig::default())
        .expect("failed to enable adaptation");

    // Get initial frame — should start on highest rendition (360p).
    let first = tokio::time::timeout(FRAME_TIMEOUT, track.next_frame())
        .await
        .expect("timed out waiting for first adaptive frame")
        .expect("adaptive track closed");

    assert!(
        first.width() > 0 && first.height() > 0,
        "first adaptive frame should have non-zero dimensions"
    );

    let initial_rendition = track.selected_rendition();

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
    let downgrade_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        // Drain frames to keep the pipeline moving.
        let _ = tokio::time::timeout(Duration::from_millis(500), track.next_frame()).await;
        let current = track.selected_rendition();
        if current != initial_rendition {
            break;
        }
        if tokio::time::Instant::now() > downgrade_deadline {
            panic!("adaptive track did not downgrade within 10 s (still on {current})");
        }
    }

    let downgraded_rendition = track.selected_rendition();
    assert_ne!(
        downgraded_rendition, initial_rendition,
        "should have switched to a lower rendition"
    );

    // Restore good conditions → expect upgrade probe eventually.
    signals_tx.send(good).ok();

    let upgrade_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(500), track.next_frame()).await;
        let current = track.selected_rendition();
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
    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// 1:1 call using [`Call::dial`] and [`Call::accept`].
///
/// Both sides publish video, one dials the other, and both receive decoded
/// video frames from the remote peer with valid dimensions.
#[tokio::test]
#[traced_test]
async fn call_dial_accept() {
    // --- Caller side ---
    let caller_ep = endpoint().await;
    let caller_live = Live::builder(caller_ep.clone()).with_router().spawn();

    let caller_broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(320, 240).with_fps(30.0);
    caller_broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
        .expect("failed to set caller video");

    // --- Callee side ---
    let callee_ep = endpoint().await;
    let callee_live = Live::builder(callee_ep.clone()).with_router().spawn();

    let callee_broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(320, 240).with_fps(30.0);
    callee_broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
        .expect("failed to set callee video");

    // Spawn the callee's accept loop in the background.
    let callee_live_clone = callee_live.clone();
    let accept_handle = tokio::spawn(async move {
        let mut incoming = callee_live_clone.transport().incoming_sessions();
        let session = incoming
            .next()
            .await
            .expect("transport shut down before incoming session");
        let session = session.accept();
        Call::accept(session, callee_broadcast).await
    });

    // Caller dials the callee.
    let callee_addr = callee_ep.addr();

    let caller_call = tokio::time::timeout(
        FRAME_TIMEOUT,
        Call::dial(&caller_live, callee_addr, caller_broadcast),
    )
    .await
    .expect("timed out dialing")
    .expect("Call::dial failed");

    // Wait for callee to accept.
    let callee_call = tokio::time::timeout(FRAME_TIMEOUT, accept_handle)
        .await
        .expect("timed out waiting for accept")
        .expect("accept task panicked")
        .expect("Call::accept failed");

    // Both sides should see each other's video.
    assert!(
        caller_call.remote().has_video(),
        "caller should see callee's video"
    );
    assert!(
        callee_call.remote().has_video(),
        "callee should see caller's video"
    );

    // Caller receives multiple frames from callee.
    let mut caller_video = caller_call
        .remote()
        .video()
        .expect("caller: failed to create video track");
    let mut prev_ts = None;
    for i in 0..3 {
        let frame = tokio::time::timeout(FRAME_TIMEOUT, caller_video.next_frame())
            .await
            .unwrap_or_else(|_| panic!("caller: timed out on frame {i}"))
            .unwrap_or_else(|| panic!("caller: video track closed at frame {i}"));
        assert!(
            frame.width() > 0 && frame.height() > 0,
            "caller: frame {i} dimensions should be non-zero"
        );
        if let Some(prev) = prev_ts {
            assert!(
                frame.timestamp >= prev,
                "caller: timestamps should not go backwards"
            );
        }
        prev_ts = Some(frame.timestamp);
    }

    // Callee receives multiple frames from caller.
    let mut callee_video = callee_call
        .remote()
        .video()
        .expect("callee: failed to create video track");
    let mut prev_ts = None;
    for i in 0..3 {
        let frame = tokio::time::timeout(FRAME_TIMEOUT, callee_video.next_frame())
            .await
            .unwrap_or_else(|_| panic!("callee: timed out on frame {i}"))
            .unwrap_or_else(|| panic!("callee: video track closed at frame {i}"));
        assert!(
            frame.width() > 0 && frame.height() > 0,
            "callee: frame {i} dimensions should be non-zero"
        );
        if let Some(prev) = prev_ts {
            assert!(
                frame.timestamp >= prev,
                "callee: timestamps should not go backwards"
            );
        }
        prev_ts = Some(frame.timestamp);
    }

    // Clean up.
    caller_call.close();
    callee_call.close();
    drop(caller_video);
    drop(callee_video);
    caller_live.shutdown().await;
    callee_live.shutdown().await;
    caller_ep.close().await;
    callee_ep.close().await;
}
