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
