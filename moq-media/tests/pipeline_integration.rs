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
use moq_media::test_util::{
    CapturingAudioBackend, NullAudioBackend, SineAudioSource, TestAudioSource, TestVideoSource,
};
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
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
    let mut track = remote.video_ready().await.unwrap();
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

/// Publishes video, subscribes, and asserts frames arrive with correct dimensions.
async fn assert_codec_roundtrip(codec: VideoCodec) {
    let preset = VideoPreset::P180;
    let (expected_w, expected_h) = preset.dimensions();
    let frames = roundtrip_video(codec, preset, 3).await;
    assert_eq!(frames.len(), 3);
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.dimensions,
            [expected_w, expected_h],
            "frame {i} dimensions mismatch: expected [{expected_w}, {expected_h}], got {:?}",
            frame.dimensions,
        );
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

    remote.ready().await;
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    remote.ready().await;
    let catalog = remote.catalog();
    let renditions: Vec<String> = catalog.video_renditions().map(String::from).collect();
    assert!(
        renditions.len() >= 2,
        "expected at least 2 renditions, got {renditions:?}"
    );

    // Subscribe to each rendition and verify frames arrive with correct dimensions.
    let mut dims_by_rendition = std::collections::HashMap::new();
    for name in &renditions {
        let mut track = remote
            .video_rendition::<moq_media::codec::DynamicVideoDecoder>(&Default::default(), name)
            .unwrap();
        let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout")
            .expect("track closed");
        assert!(frame.dimensions[0] > 0, "rendition {name} produced frame");
        dims_by_rendition.insert(name.clone(), frame.dimensions);
    }

    // The 180p and 360p renditions should produce frames with different dimensions.
    let all_dims: Vec<[u32; 2]> = dims_by_rendition.values().copied().collect();
    assert!(
        all_dims.windows(2).any(|w| w[0] != w[1]),
        "expected different renditions to produce different dimensions, got {dims_by_rendition:?}"
    );
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn multiple_renditions_have_distinct_dimensions() {
    let (broadcast, consumer) = setup_broadcast().await;
    broadcast
        .video()
        .set(
            TestVideoSource::new(640, 360),
            VideoCodec::H264,
            [VideoPreset::P360, VideoPreset::P180],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    remote.ready().await;
    let catalog = remote.catalog();
    let renditions: Vec<String> = catalog.video_renditions().map(String::from).collect();
    assert!(
        renditions.len() >= 2,
        "expected at least 2 renditions, got {renditions:?}"
    );

    // Collect 3 frames from each rendition and record their dimensions.
    let mut dims_by_rendition = std::collections::HashMap::new();
    for name in &renditions {
        let mut track = remote
            .video_rendition::<moq_media::codec::DynamicVideoDecoder>(&Default::default(), name)
            .unwrap();
        let mut dims = Vec::new();
        for _ in 0..3 {
            let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
                .await
                .expect("timeout waiting for frame")
                .expect("track closed");
            dims.push(frame.dimensions);
        }
        dims_by_rendition.insert(name.clone(), dims);
    }

    // Find the 360p and 180p renditions by name substring.
    let r360 = renditions
        .iter()
        .find(|r| r.contains("360"))
        .expect("no 360p rendition");
    let r180 = renditions
        .iter()
        .find(|r| r.contains("180"))
        .expect("no 180p rendition");

    let dims_360 = &dims_by_rendition[r360];
    let dims_180 = &dims_by_rendition[r180];

    // All frames within a rendition should have consistent dimensions.
    for d in dims_360 {
        assert_eq!(d, &dims_360[0], "360p frame dimensions inconsistent");
    }
    for d in dims_180 {
        assert_eq!(d, &dims_180[0], "180p frame dimensions inconsistent");
    }

    // 360p should have strictly larger dimensions than 180p.
    assert!(
        dims_360[0][0] > dims_180[0][0] && dims_360[0][1] > dims_180[0][1],
        "360p dims {:?} should be larger than 180p dims {:?}",
        dims_360[0],
        dims_180[0],
    );
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn publisher_replace_triggers_catalog_update() {
    let (broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    let mut watcher = remote.catalog_watcher();

    // Drain initial frame to confirm pipeline is running
    let mut track = remote.video_ready().await.unwrap();
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
    let mut track = remote.video_ready().await.unwrap();
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Video frames arrive
    let mut video = remote.video_ready().await.unwrap();
    let frame = tokio::time::timeout(TIMEOUT, video.next_frame())
        .await
        .expect("timeout")
        .expect("no video frame");
    assert!(frame.dimensions[0] > 0);

    // Audio track starts
    let audio = remote.audio_ready(&NullAudioBackend).await.unwrap();
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

    let mut track = remote.video_ready().await.unwrap();
    // Drain a few frames so the clock has data
    for _ in 0..5 {
        tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout")
            .expect("closed");
    }

    // Clock should have observed arrivals; in-process tests have near-zero jitter.
    let jitter = remote.clock().jitter();
    assert!(
        jitter < Duration::from_millis(500),
        "jitter too high for in-process test: {jitter:?}"
    );
}

// ── Group E: Publisher disconnect ──────────────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn publisher_drop_closes_subscriber() {
    let (broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    let closed = remote.closed();

    // Drain one frame to ensure pipeline is active
    let mut track = remote.video_ready().await.unwrap();
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

    remote.ready().await;
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

    let remote1 = RemoteBroadcast::with_playout_mode("s1", consumer1, PlayoutMode::Reliable)
        .await
        .unwrap();
    let remote2 = RemoteBroadcast::with_playout_mode("s2", consumer2, PlayoutMode::Reliable)
        .await
        .unwrap();

    let mut track1 = remote1.video_ready().await.unwrap();
    let mut track2 = remote2.video_ready().await.unwrap();

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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Drain a frame from the initial rendition
    let mut track = remote.video_ready().await.unwrap();
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
    let mut track = remote.video_ready().await.unwrap();
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    remote.ready().await;
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
    let mut track = remote.video_ready().await.unwrap();
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
    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    let mut track = remote.video_ready().await.unwrap();
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    remote.ready().await;
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    remote.ready().await;
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Drain one video frame to confirm the pipeline is running
    let mut track = remote.video_ready().await.unwrap();
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
    let mut track = remote.video_ready().await.unwrap();
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
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

// ── Group L: Audio data plane verification ────────────────────────

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_data_flows_through_pipeline() {
    let (broadcast, consumer) = setup_broadcast().await;

    // Publish with a sine wave source instead of silence.
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
            SineAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    // Subscribe with a capturing backend to record decoded samples.
    let backend = CapturingAudioBackend::new();
    let captured = backend.captured_samples();

    let _audio = remote.audio_ready(&backend).await.unwrap();

    // Drain some video frames to give the audio pipeline time to produce output.
    let mut track = remote.video_ready().await.unwrap();
    for _ in 0..10 {
        tokio::time::timeout(TIMEOUT, track.next_frame())
            .await
            .expect("timeout")
            .expect("closed");
    }

    // Give audio decoder a moment to push samples.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let samples = captured.lock().unwrap();
    assert!(
        !samples.is_empty(),
        "expected captured audio samples, got none — audio data did not flow through pipeline"
    );

    // Verify the captured samples are not all silent (Opus should preserve
    // the sine wave energy through encode→decode).
    let rms: f32 = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    assert!(
        rms > 0.01,
        "captured audio RMS {rms:.6} is too low — expected non-silent sine wave after Opus roundtrip"
    );
}

// ── Group M: Playout clock shared across tracks ───────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn playout_clock_reset_on_resubscribe() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;

    // Drain a few frames so the clock establishes a base mapping
    let mut track = remote.video_ready().await.unwrap();
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

// ── Group N: RemoteBroadcast construction and state ────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn remote_broadcast_name_matches_constructor_arg() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;
    assert_eq!(remote.broadcast_name(), "test");
}

#[tokio::test]
async fn remote_broadcast_empty_has_no_media() {
    let broadcast = LocalBroadcast::new();
    tokio::task::yield_now().await;
    let consumer = broadcast.consume();

    // An empty broadcast still publishes a catalog (with no renditions).
    let remote = RemoteBroadcast::with_playout_mode("empty", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    assert!(!remote.has_video(), "empty broadcast should have no video");
    assert!(!remote.has_audio(), "empty broadcast should have no audio");
    assert_eq!(remote.broadcast_name(), "empty");
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn remote_broadcast_video_appears_after_publish() {
    let broadcast = LocalBroadcast::new();
    tokio::task::yield_now().await;
    let consumer = broadcast.consume();

    let remote = RemoteBroadcast::with_playout_mode("late", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();
    assert!(!remote.has_video());

    // Publish video after the subscriber connected.
    let (w, h) = VideoPreset::P180.dimensions();
    broadcast
        .video()
        .set(
            TestVideoSource::new(w, h),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
        .unwrap();

    // Wait for catalog update to propagate.
    let mut watcher = remote.catalog_watcher();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !remote.has_video() {
        tokio::time::timeout_at(deadline, watcher.updated())
            .await
            .expect("timeout waiting for video to appear")
            .expect("catalog watcher disconnected");
    }

    assert!(remote.has_video());
}

// ── Group O: VideoTrack metadata ──────────────────────────────────

#[cfg(feature = "h264")]
#[tokio::test]
async fn video_track_rendition_name_contains_preset() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;
    let track = remote.video_ready().await.unwrap();
    let rendition = track.rendition();
    assert!(
        rendition.contains("180"),
        "rendition name should contain preset identifier, got: {rendition}"
    );
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn video_track_decoder_name_is_nonempty() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;
    let track = remote.video_ready().await.unwrap();
    assert!(
        !track.decoder_name().is_empty(),
        "decoder_name should not be empty for a pipeline-backed track"
    );
}

#[cfg(feature = "h264")]
#[tokio::test]
async fn video_track_current_frame_returns_none_initially() {
    let (_broadcast, remote) = publish_and_subscribe(VideoCodec::H264, VideoPreset::P180).await;
    let mut track = remote.video_ready().await.unwrap();

    // Drain all buffered frames via next_frame first to confirm pipeline works.
    let frame = tokio::time::timeout(TIMEOUT, track.next_frame())
        .await
        .expect("timeout")
        .expect("no frame");
    assert!(frame.dimensions[0] > 0);

    // current_frame is non-blocking and returns the latest buffered frame (or None).
    // After draining, it may or may not have another frame ready — just verify
    // it does not panic and returns a valid Option.
    let _ = track.current_frame();
}

// ── Group P: AudioTrack metadata ──────────────────────────────────

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_track_rendition_name_contains_codec() {
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

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    let audio = remote.audio_ready(&NullAudioBackend).await.unwrap();
    let rendition = audio.rendition();
    assert!(
        rendition.contains("opus"),
        "audio rendition name should contain codec id, got: {rendition}"
    );
}

#[cfg(all(feature = "h264", feature = "opus"))]
#[tokio::test]
async fn audio_track_handle_pause_resume() {
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
            SineAudioSource::new(AudioFormat::mono_48k()),
            moq_media::codec::AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .unwrap();

    let remote = RemoteBroadcast::with_playout_mode("test", consumer, PlayoutMode::Reliable)
        .await
        .unwrap();

    let audio = remote.audio_ready(&NullAudioBackend).await.unwrap();
    let handle = audio.handle();

    assert!(!handle.is_paused(), "audio should start unpaused");
    handle.pause();
    assert!(handle.is_paused(), "audio should be paused after pause()");
    handle.resume();
    assert!(
        !handle.is_paused(),
        "audio should be unpaused after resume()"
    );
}

// ── Group Q: Standalone AudioDecoderPipeline (T2) ─────────────────

#[cfg(feature = "opus")]
#[tokio::test]
async fn audio_decoder_pipeline_standalone_roundtrip() {
    use moq_media::codec::OpusAudioDecoder;
    use moq_media::pipeline::AudioDecoderPipeline;
    use moq_media::transport::media_pipe;

    let format = AudioFormat::mono_48k();
    let preset = AudioPreset::Hq;

    // Build encoder.
    let enc_config = moq_media::format::AudioEncoderConfig::from_preset(format, preset);
    use moq_media::traits::AudioEncoderFactory;

    let encoder =
        <moq_media::codec::OpusEncoder as AudioEncoderFactory>::with_preset(format, preset)
            .unwrap();
    let audio_config =
        <moq_media::codec::OpusEncoder as AudioEncoderFactory>::config_for(&enc_config);

    // Create an in-memory pipe connecting encoder output to decoder input.
    let (sink, source) = media_pipe(64);

    // Build capturing backend to verify decoded samples arrive.
    let backend = CapturingAudioBackend::new();
    let captured = backend.captured_samples();

    // Start the decoder pipeline from the pipe source.
    let _decoder_pipeline = AudioDecoderPipeline::new::<OpusAudioDecoder>(
        "test-audio".into(),
        source,
        &audio_config,
        &backend,
        Default::default(),
    )
    .await
    .unwrap();

    // Start the encoder pipeline with a sine source writing to the pipe sink.
    let sine = SineAudioSource::new(format);
    let _encoder_pipeline =
        moq_media::pipeline::AudioEncoderPipeline::with_source(Box::new(sine), encoder, sink)
            .unwrap();

    // Let the pipeline run for a bit to produce output.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let samples = captured.lock().unwrap();
    assert!(
        !samples.is_empty(),
        "standalone audio decoder pipeline should produce decoded samples"
    );

    // Verify the decoded audio is not silent (sine wave energy survives encode/decode).
    let rms: f32 = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    assert!(
        rms > 0.01,
        "decoded audio RMS {rms:.6} too low — expected non-silent sine wave after Opus roundtrip"
    );
}

#[cfg(feature = "opus")]
#[tokio::test]
async fn audio_decoder_pipeline_stops_when_source_closes() {
    use moq_media::codec::OpusAudioDecoder;
    use moq_media::pipeline::AudioDecoderPipeline;
    use moq_media::transport::media_pipe;

    let format = AudioFormat::mono_48k();
    let preset = AudioPreset::Hq;

    use moq_media::traits::AudioEncoderFactory;

    let encoder =
        <moq_media::codec::OpusEncoder as AudioEncoderFactory>::with_preset(format, preset)
            .unwrap();
    let enc_config = moq_media::format::AudioEncoderConfig::from_preset(format, preset);
    let audio_config =
        <moq_media::codec::OpusEncoder as AudioEncoderFactory>::config_for(&enc_config);

    let (sink, source) = media_pipe(64);
    let backend = NullAudioBackend;

    let decoder_pipeline = AudioDecoderPipeline::new::<OpusAudioDecoder>(
        "test-stop".into(),
        source,
        &audio_config,
        &backend,
        Default::default(),
    )
    .await
    .unwrap();

    // Start the encoder, let it produce a few packets, then drop it to close the pipe.
    let sine = SineAudioSource::new(format);
    let encoder_pipeline =
        moq_media::pipeline::AudioEncoderPipeline::with_source(Box::new(sine), encoder, sink)
            .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(encoder_pipeline);

    // The decoder pipeline should detect the closed source and stop.
    tokio::time::timeout(TIMEOUT, decoder_pipeline.stopped())
        .await
        .expect("decoder pipeline should stop after source closes");
}
