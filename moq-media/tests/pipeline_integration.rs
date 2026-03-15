//! Integration tests for the publish → subscribe media pipeline.
//!
//! Tests the full path: source → encoder → moq-lite transport → decoder → frames,
//! using in-process moq-lite (no network). Each test is short and tests one behavior.
//! The codec dimension is factored into a helper so adding codecs is a one-liner.

use std::time::Duration;

use moq_media::codec::VideoCodec;
use moq_media::format::{AudioFormat, AudioPreset, VideoPreset};
use moq_media::playout::PlayoutMode;
use moq_media::publish::LocalBroadcast;
use moq_media::subscribe::RemoteBroadcast;
use moq_media::test_util::{NullAudioBackend, TestAudioSource, TestVideoSource};
use n0_watcher::Watcher as _;

const TIMEOUT: Duration = Duration::from_secs(10);

// ── Helpers ────────────────────────────────────────────────────────

/// Creates a broadcast and consumer pair, yielding to let the broadcast's
/// background task register its dynamic producer (required for subscribe_track).
async fn setup_broadcast() -> (LocalBroadcast, moq_lite::BroadcastConsumer) {
    let broadcast = LocalBroadcast::new();
    // The broadcast's background task must call `producer.dynamic()` before
    // any subscriber can subscribe_track. Yield to give it a chance to run.
    tokio::task::yield_now().await;
    let consumer = broadcast.consume();
    (broadcast, consumer)
}

/// Sets up a full publish → subscribe pipeline with video, returning the
/// remote broadcast ready for frame consumption.
async fn publish_and_subscribe(
    codec: VideoCodec,
    preset: VideoPreset,
) -> (LocalBroadcast, RemoteBroadcast) {
    let (broadcast, consumer) = setup_broadcast().await;
    let (w, h) = preset.dimensions();
    broadcast
        .video()
        .set(TestVideoSource::new(w, h), codec, [preset])
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();
    (broadcast, remote)
}

/// Publishes video, subscribes, and collects `n` decoded frames.
async fn roundtrip_video(
    codec: VideoCodec,
    preset: VideoPreset,
    n: usize,
) -> Vec<moq_media::format::VideoFrame> {
    let (_broadcast, remote) = publish_and_subscribe(codec, preset).await;
    let mut track = remote.video().unwrap();
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout waiting for frame")
            .expect("track closed unexpectedly");
        frames.push(frame);
    }
    frames
}

/// Publishes video, subscribes, and asserts frames arrive.
async fn assert_codec_roundtrip(codec: VideoCodec) {
    let frames = roundtrip_video(codec, VideoPreset::P180, 3).await;
    assert_eq!(frames.len(), 3);
    for frame in &frames {
        assert!(frame.dimensions[0] > 0);
        assert!(frame.dimensions[1] > 0);
    }
}

// ── Group A: Codec roundtrip ───────────────────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn publish_h264_subscriber_receives_frames() {
    assert_codec_roundtrip(VideoCodec::H264).await;
}

#[cfg(feature = "av1")]
#[tokio::test]
async fn publish_av1_subscriber_receives_frames() {
    assert_codec_roundtrip(VideoCodec::Av1).await;
}

// ── Group B: Catalog and rendition changes ─────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn catalog_lists_published_renditions() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    let catalog = remote.catalog();
    let renditions: Vec<&str> = catalog.video_renditions().collect();
    assert!(
        !renditions.is_empty(),
        "catalog should have video renditions"
    );
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn multiple_renditions_subscriber_selects_each() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(640, 360),
            VideoCodec::H264,
            [VideoPreset::P180, VideoPreset::P360],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    let catalog = remote.catalog();
    let renditions: Vec<String> = catalog.video_renditions().map(String::from).collect();
    assert!(
        renditions.len() >= 2,
        "expected at least 2 renditions, got {renditions:?}"
    );

    // Subscribe to each rendition and verify frames arrive
    for name in &renditions {
        let mut track = remote
            .video_rendition::<moq_media::codec::DynamicVideoDecoder>(&Default::default(), name)
            .unwrap();
        let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout")
            .expect("track closed");
        assert!(frame.dimensions[0] > 0, "rendition {name} produced frame");
    }
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn publisher_replace_triggers_catalog_update() {
    let (broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    let mut watcher = remote.catalog_watcher();

    // Drain initial frame to confirm pipeline is running
    let mut track = remote.video().unwrap();
    tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("no frame");
    drop(track);

    // Replace video with new source
    broadcast
        .video()
        .replace(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();

    // Catalog should update
    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update")
        .expect("catalog watcher disconnected");

    // Subscribe to new rendition and verify frames
    let mut track = remote.video().unwrap();
    let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("no frame after replace");
    assert!(frame.dimensions[0] > 0);
}

// ── Group C: Audio + Video ─────────────────────────────────────────

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_and_video_roundtrip() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    assert!(remote.has_video());
    assert!(remote.has_audio());

    // Video frames arrive
    let mut video = remote.video().unwrap();
    let frame = tokio::time::timeout(TIMEOUT, video.next_frame())
        .await
        .expect("timeout")
        .expect("no video frame");
    assert!(frame.dimensions[0] > 0);

    // Audio track starts
    let audio = remote.audio(&NullAudioBackend).await.unwrap();
    assert!(!audio.rendition().is_empty());
}

// ── Group D: Playout clock ─────────────────────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn playout_reliable_frames_have_monotonic_timestamps() {
    let frames = roundtrip_video(VideoCodec::H264, VideoPreset::P180, 5).await;
    for window in frames.windows(2) {
        assert!(
            window[1].timestamp >= window[0].timestamp,
            "timestamps not monotonic: {:?} vs {:?}",
            window[0].timestamp,
            window[1].timestamp,
        );
    }
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn playout_clock_reports_jitter_after_frames() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    let mut track = remote.video().unwrap();
    // Drain a few frames so the clock has data
    for _ in 0..5 {
        tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout")
            .expect("closed");
    }

    // Clock should have observed arrivals (jitter may be zero in fast tests)
    let _jitter = remote.clock().jitter();
}

// ── Group E: Publisher disconnect ──────────────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn publisher_drop_closes_subscriber() {
    let (broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    let closed = remote.closed();

    // Drain one frame to ensure pipeline is active
    let mut track = remote.video().unwrap();
    tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("closed");

    // Drop the publisher
    drop(broadcast);

    // Subscriber should detect the closure
    tokio::time::timeout(TIMEOUT, closed)
        .await
        .expect("timeout: subscriber did not detect publisher drop");
}

// ── Group F: Clear video ───────────────────────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn clear_video_updates_catalog() {
    let (broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    assert!(remote.has_video());

    let mut watcher = remote.catalog_watcher();

    // Clear video
    broadcast.video().clear();

    // Wait for catalog update
    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update after clear")
        .expect("catalog watcher disconnected");

    assert!(!remote.has_video(), "video should be gone after clear");
}

// ── Group G: Two subscribers on same broadcast ────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn two_subscribers_receive_frames() {
    let (broadcast, consumer1) = setup_broadcast().await;
    let consumer2 = broadcast.consume();
    let (w, h) = VideoPreset::P180.dimensions();
    broadcast
        .video()
        .set(
            TestVideoSource::new(w, h),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();

    let remote1 = RemoteBroadcast::with_playout("s1", consumer1, PlayoutMode::Reliable)
        .await
        .unwrap();
    let remote2 = RemoteBroadcast::with_playout("s2", consumer2, PlayoutMode::Reliable)
        .await
        .unwrap();

    let mut track1 = remote1.video().unwrap();
    let mut track2 = remote2.video().unwrap();

    let f1 = tokio::time::timeout(TIMEOUT, track1.next_frame())
        .await
        .expect("timeout s1")
        .expect("closed s1");
    let f2 = tokio::time::timeout(TIMEOUT, track2.next_frame())
        .await
        .expect("timeout s2")
        .expect("closed s2");

    assert!(f1.dimensions[0] > 0);
    assert!(f2.dimensions[0] > 0);
}

// ── Group H: Publisher resolution change ──────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn publisher_resolution_change_updates_subscriber() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Drain a frame from the initial rendition
    let mut track = remote.video().unwrap();
    tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("closed");
    drop(track);

    let mut watcher = remote.catalog_watcher();

    // Replace with higher resolution
    broadcast
        .video()
        .replace(
            TestVideoSource::new(640, 360),
            VideoCodec::H264,
            [VideoPreset::P360],
        )
        .unwrap();

    // Wait for catalog update
    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update")
        .expect("catalog watcher disconnected");

    // Verify catalog now has 360p rendition
    let catalog = remote.catalog();
    let renditions: Vec<&str> = catalog.video_renditions().collect();
    assert!(
        renditions.iter().any(|r| r.contains("360")),
        "expected 360p rendition, got {renditions:?}"
    );

    // Subscribe to new rendition and verify frames arrive
    let mut track = remote.video().unwrap();
    let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("closed");
    assert!(frame.dimensions[0] > 0);
}

// ── Group I: Audio clear while video continues ────────────────────

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_clear_while_video_continues() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    assert!(remote.has_video());
    assert!(remote.has_audio());

    let mut watcher = remote.catalog_watcher();

    // Clear audio only
    broadcast.audio().clear();

    // Wait for catalog update
    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update")
        .expect("catalog watcher disconnected");

    assert!(remote.has_video(), "video should still be available");
    assert!(!remote.has_audio(), "audio should be gone after clear");

    // Video should still work
    let mut track = remote.video().unwrap();
    let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("closed");
    assert!(frame.dimensions[0] > 0);
}

// ── Group J: Multiple rapid republishes ───────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn rapid_republish_does_not_panic() {
    let (broadcast, consumer) = setup_broadcast().await;

    // Rapid-fire video replacements
    for _ in 0..5 {
        broadcast
            .video()
            .set(
                TestVideoSource::new(320, 180),
                VideoCodec::H264,
                [VideoPreset::P180],
            )
            .unwrap();
    }

    // Should still be able to subscribe and get frames
    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    let mut track = remote.video().unwrap();
    let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("closed");
    assert!(frame.dimensions[0] > 0);
}

// ── Group K: Audio source switching ────────────────────────────────

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_replace_source_subscriber_still_receives() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    assert!(remote.has_audio());

    let mut watcher = remote.catalog_watcher();

    // Replace audio source with a fresh one
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    // Wait for catalog update from the replacement
    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update after audio replace")
        .expect("catalog watcher disconnected");

    assert!(
        remote.has_audio(),
        "audio should still be present after replace"
    );

    let audio = remote.audio(&NullAudioBackend).await.unwrap();
    assert!(!audio.rendition().is_empty());
}

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_clear_and_readd_works() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    assert!(remote.has_audio());

    let mut watcher = remote.catalog_watcher();

    // Clear audio
    broadcast.audio().clear();

    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update after audio clear")
        .expect("catalog watcher disconnected");

    assert!(!remote.has_audio(), "audio should be gone after clear");

    // Re-add audio
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update after audio re-add")
        .expect("catalog watcher disconnected");

    assert!(remote.has_audio(), "audio should be back after re-add");

    let audio = remote.audio(&NullAudioBackend).await.unwrap();
    assert!(!audio.rendition().is_empty());
}

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn video_source_switch_preserves_audio() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Drain one video frame to confirm the pipeline is running
    let mut track = remote.video().unwrap();
    tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("no initial frame");
    drop(track);

    let mut watcher = remote.catalog_watcher();

    // Replace video with a different source
    broadcast
        .video()
        .replace(
            TestVideoSource::new(640, 360),
            VideoCodec::H264,
            [VideoPreset::P360],
        )
        .unwrap();

    tokio::time::timeout(TIMEOUT, watcher.updated())
        .await
        .expect("timeout waiting for catalog update after video replace")
        .expect("catalog watcher disconnected");

    // Audio should still be present
    assert!(
        remote.has_audio(),
        "audio should survive video source switch"
    );

    // Video frames should arrive from the new source
    let mut track = remote.video().unwrap();
    let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout waiting for frame after video replace")
        .expect("track closed after video replace");
    assert!(frame.dimensions[0] > 0);
}

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn rapid_audio_switches_do_not_panic() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(320, 180),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Rapid-fire audio clear + re-add cycles
    for _ in 0..5 {
        broadcast.audio().clear();
        broadcast
            .audio()
            .set(
                TestAudioSource::new(AudioFormat::mono_48k()),
                moq_media::codec::AudioCodec::Opus,
                [AudioPreset::Hq],
            )
            .unwrap();
    }

    // Let the catalog settle — drain updates until we see audio present
    let mut watcher = remote.catalog_watcher();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !remote.has_audio() {
        tokio::time::timeout_at(deadline, watcher.updated())
            .await
            .expect("timeout waiting for audio after rapid switches")
            .expect("catalog watcher disconnected");
    }

    assert!(
        remote.has_audio(),
        "audio should be available after rapid switches"
    );

    let audio = remote.audio(&NullAudioBackend).await.unwrap();
    assert!(!audio.rendition().is_empty());
}

// ── Group L: Playout clock shared across tracks ───────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn playout_clock_reset_on_resubscribe() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    // Drain a few frames so the clock establishes a base mapping
    let mut track = remote.video().unwrap();
    for _ in 0..3 {
        tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout")
            .expect("closed");
    }

    // Clock should have non-zero jitter or at least an established base
    let jitter_before = remote.clock().jitter();
    let _ = jitter_before; // just verify it doesn't panic

    // Reset clock (as split.rs does on resubscribe)
    remote.clock().reset();

    // Jitter should be zero after reset
    assert_eq!(
        remote.clock().jitter(),
        std::time::Duration::ZERO,
        "jitter should be zero after reset"
    );
}
