//! Encoder and decoder pipeline orchestration.
//!
//! Each pipeline runs on a dedicated OS thread and bridges between
//! sync codec APIs and async transport via channels. Pipelines are
//! created by the publish and subscribe modules — most callers do
//! not use this module directly.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::{format::MediaPacket, playout::PlaybackPolicy, transport::PacketSource};

mod audio_decode;
mod audio_encode;
mod video_decode;
mod video_encode;

pub use audio_decode::AudioDecoderPipeline;
pub use audio_encode::AudioEncoderPipeline;
pub use video_decode::{VideoDecoderFrames, VideoDecoderHandle, VideoDecoderPipeline};
pub use video_encode::{PreEncodedVideoPipeline, VideoEncoderPipeline};

/// Internal parameters for decoder pipelines.
///
/// This bundles the shared state that decode threads need to coordinate audio,
/// video, and stats. Callers configure playback through
/// [`crate::playout::PlaybackPolicy`] on [`crate::subscribe::RemoteBroadcast`];
/// they do not construct this directly.
///
/// The exception is code that builds pipelines manually, such as local tools
/// and examples. For normal subscribe-side playback, prefer
/// [`crate::subscribe::RemoteBroadcast`] and let it create this context.
#[derive(Debug, Clone)]
pub struct DecodeOpts {
    /// Stats collectors for metrics and timeline.
    pub stats: crate::stats::DecodeStats,
    /// Shared estimate of audio playback position.
    pub audio_position: AudioPosition,
    /// One-shot signal set by the video path after the first frame is released.
    pub video_started: Arc<AtomicBool>,
    /// Playback policy chosen by the owning remote broadcast.
    pub playback_policy: PlaybackPolicy,
    /// Shared freshness threshold in milliseconds.
    ///
    /// This lets an existing subscription adjust stale-media handling without
    /// rebuilding all tracks. It is intentionally internal; the public API
    /// exposes this as `RemoteBroadcast::set_max_stale_duration`.
    pub max_stale_duration_ms: Arc<AtomicU64>,
    /// Skip generation counter shared between video and audio decode loops.
    /// Video increments this on skip recovery; audio watches it and flushes
    /// its decoder to resync when it changes.
    pub skip_generation: Arc<AtomicU64>,
}

impl Default for DecodeOpts {
    fn default() -> Self {
        Self {
            stats: crate::stats::DecodeStats::default(),
            audio_position: AudioPosition::default(),
            video_started: Arc::new(AtomicBool::new(false)),
            playback_policy: PlaybackPolicy::unmanaged(),
            max_stale_duration_ms: Arc::new(AtomicU64::new(500)),
            skip_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Shared estimate of audio playout position at the speaker.
///
/// The audio decode loop writes this after each push into the output sink. The
/// video decode loop reads it and treats it as the timing master.
#[derive(Debug, Clone, Default)]
pub struct AudioPosition {
    /// PTS of the last pushed audio chunk's first sample, in microseconds.
    last_chunk_start_us: Arc<AtomicI64>,
    /// Duration of the last pushed audio chunk, in microseconds.
    last_chunk_duration_us: Arc<AtomicI64>,
    /// Audio buffered between push and speaker output, in microseconds.
    buffered_us: Arc<AtomicI64>,
}

impl AudioPosition {
    /// Records the PTS span of audio just pushed to the sink and the current
    /// output queue depth.
    pub(crate) fn record(&self, start_pts: Duration, chunk_duration: Duration, buffered: Duration) {
        self.last_chunk_start_us
            .store(start_pts.as_micros() as i64, Ordering::Release);
        self.last_chunk_duration_us
            .store(chunk_duration.as_micros() as i64, Ordering::Release);
        self.buffered_us
            .store(buffered.as_micros() as i64, Ordering::Release);
    }

    /// Clears stale timing when a pipeline is rebuilt.
    pub(crate) fn reset(&self) {
        self.last_chunk_start_us.store(0, Ordering::Release);
        self.last_chunk_duration_us.store(0, Ordering::Release);
        self.buffered_us.store(0, Ordering::Release);
    }

    /// Estimates the PTS currently reaching the speaker.
    pub(crate) fn playback_pts(&self) -> Option<Duration> {
        let start_us = self.last_chunk_start_us.load(Ordering::Acquire);
        if start_us < 0 {
            return None;
        }
        let dur_us = self.last_chunk_duration_us.load(Ordering::Acquire);
        if start_us == 0 && dur_us == 0 {
            return None;
        }
        let buf_us = self.buffered_us.load(Ordering::Acquire);
        let end_us = start_us.saturating_add(dur_us);
        let speaker_us = (end_us - buf_us).max(0);
        Some(Duration::from_micros(speaker_us as u64))
    }

    /// Returns the current output queue depth if we have a valid estimate.
    pub(crate) fn buffered_duration(&self) -> Option<Duration> {
        self.playback_pts()?;
        Some(Duration::from_micros(
            self.buffered_us.load(Ordering::Acquire).max(0) as u64,
        ))
    }
}

/// Forwards packets from an async [`PacketSource`] into an mpsc channel.
pub(crate) async fn forward_packets(
    mut source: impl PacketSource,
    sender: mpsc::Sender<MediaPacket>,
) {
    loop {
        match source.read().await {
            Ok(Some(packet)) => {
                if sender.send(packet).await.is_err() {
                    debug!("forward_packets: decoder channel closed");
                    break;
                }
            }
            Ok(None) => {
                debug!("forward_packets: source ended");
                break;
            }
            Err(err) => {
                error!("forward_packets: failed to read from source: {err:#}");
                break;
            }
        }
    }
}

/// Front-frame lead where video decode starts trimming future backlog instead
/// of merely pausing intake behind a held frame.
const VIDEO_DECODE_AHEAD_PAUSE_THRESHOLD: Duration = Duration::from_millis(250);

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rusty_codecs::config::VideoConfig;

    use super::{
        video_decode::{
            FrontFrameAction, VideoLoopState, maybe_pause_video_decode, take_front_frame_action,
            take_ready_front_frame,
        },
        *,
    };
    use crate::{
        codec::{H264Encoder, H264VideoDecoder, test_util::make_rgba_frame},
        format::{DecodeConfig, VideoFrame, VideoPreset},
        playout::{PlaybackPolicy, PlayoutBuffer, VideoSyncConfig, VideoSyncController},
        traits::{VideoEncoder, VideoEncoderFactory},
        transport::media_pipe,
        util::encoded_frames_to_media_packets,
    };

    fn encode_h264_packets(
        w: u32,
        h: u32,
        n: usize,
        preset: VideoPreset,
    ) -> (VideoConfig, Vec<MediaPacket>) {
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let mut packets = Vec::new();
        for i in 0..n {
            let frame = make_rgba_frame(w, h, (i * 25) as u8, 128, 64);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        let config = enc.config();
        (config, encoded_frames_to_media_packets(packets))
    }

    #[tokio::test]
    async fn video_decoder_pipeline_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let (config, packets) = encode_h264_packets(w, h, 10, VideoPreset::P180);
        assert!(!packets.is_empty());

        let decode_config = DecodeConfig::default();
        let (sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test".into(),
            source,
            &config,
            &decode_config,
            Default::default(),
        )
        .unwrap();

        // Feed packets from a blocking thread (send_blocking can't be called in async context)
        tokio::task::spawn_blocking(move || {
            for pkt in packets {
                sink.send_blocking(pkt).unwrap();
            }
            // drop sink to signal EOF
        });

        let frames = pipeline.frames;
        let mut count = 0;
        while let Some(frame) = frames.recv().await {
            let img = frame.rgba_image();
            assert_eq!(img.width(), w);
            assert_eq!(img.height(), h);
            count += 1;
        }
        assert!(count >= 5, "expected >= 5 decoded frames, got {count}");
    }

    #[tokio::test]
    async fn video_decoder_pipeline_shutdown_on_drop() {
        let (_config, _packets) = encode_h264_packets(320, 180, 1, VideoPreset::P180);
        let config = _config;
        let decode_config = DecodeConfig::default();
        let (_sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test".into(),
            source,
            &config,
            &decode_config,
            Default::default(),
        )
        .unwrap();

        // Drop pipeline — should not hang or panic
        drop(pipeline);
    }

    #[tokio::test]
    async fn video_decoder_pipeline_viewport() {
        let (config, packets) = encode_h264_packets(640, 360, 5, VideoPreset::P360);

        let decode_config = DecodeConfig::default();
        let (sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test".into(),
            source,
            &config,
            &decode_config,
            Default::default(),
        )
        .unwrap();

        pipeline.handle.set_viewport(320, 180);

        tokio::task::spawn_blocking(move || {
            for pkt in packets {
                sink.send_blocking(pkt).unwrap();
            }
        });

        let frames = pipeline.frames;
        while let Some(frame) = frames.recv().await {
            let img = frame.rgba_image();
            assert!(img.width() <= 320, "width {} > 320", img.width());
            assert!(img.height() <= 180, "height {} > 180", img.height());
        }
    }

    #[tokio::test]
    async fn video_decoder_pipeline_with_synced_playout() {
        let w = 320u32;
        let h = 180u32;
        let (config, packets) = encode_h264_packets(w, h, 10, VideoPreset::P180);
        assert!(!packets.is_empty());

        let decode_config = DecodeConfig::default();
        let (sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test-synced".into(),
            source,
            &config,
            &decode_config,
            DecodeOpts {
                playback_policy: PlaybackPolicy::audio_master_with_hold(Duration::from_millis(50)),
                ..Default::default()
            },
        )
        .unwrap();

        tokio::task::spawn_blocking(move || {
            for pkt in packets {
                sink.send_blocking(pkt).unwrap();
            }
        });

        let frames = pipeline.frames;
        let mut count = 0;
        while let Some(frame) = frames.recv().await {
            let img = frame.rgba_image();
            assert_eq!(img.width(), w);
            assert_eq!(img.height(), h);
            count += 1;
        }
        assert!(count >= 5, "expected >= 5 decoded frames, got {count}");
    }

    #[test]
    fn take_front_frame_action_can_drop_multiple_late_frames_without_stalling() {
        let stats = crate::stats::DecodeStats::default();
        let audio_position = AudioPosition::default();
        audio_position.record(
            Duration::from_millis(200),
            Duration::from_millis(20),
            Duration::ZERO,
        );
        let mut sync = Some(VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        ));
        // Prime the controller past its special "first frame always renders"
        // behavior so the test exercises real catchup/drop logic.
        let _ = sync.as_mut().unwrap().decide(
            Duration::from_millis(50),
            Some(Duration::from_millis(50)),
            Instant::now(),
        );
        let mut playout = PlayoutBuffer::new(30);
        let stale = [
            Duration::from_millis(0),
            Duration::from_millis(20),
            Duration::from_millis(40),
        ];
        for pts in stale {
            let _ = playout.push(VideoFrame::new_rgba(vec![0; 4].into(), 1, 1, pts));
        }
        let _ = playout.push(VideoFrame::new_rgba(
            vec![0; 4].into(),
            1,
            1,
            Duration::from_millis(210),
        ));

        let mut dropped = 0;
        loop {
            let len_before = playout.len();
            match take_front_frame_action(&mut playout, &mut sync, &audio_position, &stats) {
                FrontFrameAction::Idle => {
                    if playout.len() < len_before {
                        dropped += 1;
                    } else {
                        panic!("expected stale frames to drop or render");
                    }
                }
                FrontFrameAction::Render(frame) => {
                    assert_eq!(frame.timestamp, Duration::from_millis(210));
                    break;
                }
                FrontFrameAction::Hold => {
                    panic!("expected stale frames to drop before holding")
                }
            }
        }

        assert_eq!(dropped, 3);
    }

    #[test]
    fn maybe_pause_video_decode_trims_future_backlog() {
        let stats = crate::stats::DecodeStats::default();
        let audio_position = AudioPosition::default();
        audio_position.record(
            Duration::from_millis(100),
            Duration::from_millis(20),
            Duration::ZERO,
        );

        let mut sync = Some(VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        ));
        let _ = sync.as_mut().unwrap().decide(
            Duration::from_millis(100),
            Some(Duration::from_millis(100)),
            Instant::now(),
        );
        let state = VideoLoopState::new(Duration::from_millis(33));

        let mut playout = PlayoutBuffer::new(30);
        for pts in [
            Duration::from_millis(500),
            Duration::from_millis(533),
            Duration::from_millis(566),
        ] {
            let _ = playout.push(VideoFrame::new_rgba(vec![0; 4].into(), 1, 1, pts));
        }

        let wait = maybe_pause_video_decode(&mut playout, &sync, &audio_position, &stats, &state);

        assert!(wait.is_some(), "expected decode pause for far-future video");
        assert_eq!(playout.len(), 1, "future backlog should be trimmed");
        assert_eq!(
            playout.front().expect("front frame").timestamp,
            Duration::from_millis(500)
        );
    }

    #[test]
    fn maybe_pause_video_decode_is_disabled_for_unmanaged_playout() {
        let stats = crate::stats::DecodeStats::default();
        let audio_position = AudioPosition::default();
        audio_position.record(
            Duration::from_millis(100),
            Duration::from_millis(20),
            Duration::ZERO,
        );
        let mut playout = PlayoutBuffer::new(30);
        let _ = playout.push(VideoFrame::new_rgba(
            vec![0; 4].into(),
            1,
            1,
            Duration::from_millis(500),
        ));
        let state = VideoLoopState::new(Duration::from_millis(33));

        let wait = maybe_pause_video_decode(&mut playout, &None, &audio_position, &stats, &state);

        assert!(wait.is_none(), "unmanaged playout must not pause decode");
        assert_eq!(playout.len(), 1);
    }

    #[test]
    fn maybe_pause_video_decode_does_not_pause_single_early_frame() {
        let stats = crate::stats::DecodeStats::default();
        let audio_position = AudioPosition::default();
        audio_position.record(
            Duration::from_millis(100),
            Duration::from_millis(20),
            Duration::ZERO,
        );

        let mut sync = Some(VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        ));
        let _ = sync.as_mut().unwrap().decide(
            Duration::from_millis(100),
            Some(Duration::from_millis(100)),
            Instant::now(),
        );
        let state = VideoLoopState::new(Duration::from_millis(33));

        let mut playout = PlayoutBuffer::new(30);
        let _ = playout.push(VideoFrame::new_rgba(
            vec![0; 4].into(),
            1,
            1,
            Duration::from_millis(500),
        ));

        let wait = maybe_pause_video_decode(&mut playout, &sync, &audio_position, &stats, &state);

        assert!(wait.is_none(), "single-frame queue should keep decoding");
        assert_eq!(playout.len(), 1);
    }

    #[test]
    fn maybe_pause_video_decode_pauses_small_steady_hold_before_overflow() {
        let stats = crate::stats::DecodeStats::default();
        let audio_position = AudioPosition::default();
        audio_position.record(
            Duration::from_millis(1_000),
            Duration::from_millis(20),
            Duration::ZERO,
        );

        let mut sync = Some(VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        ));
        let _ = sync.as_mut().unwrap().decide(
            Duration::from_millis(1_000),
            Some(Duration::from_millis(1_000)),
            Instant::now(),
        );
        let state = VideoLoopState::new(Duration::from_millis(33));

        let mut playout = PlayoutBuffer::new(30);
        for pts in [
            Duration::from_millis(1_100),
            Duration::from_millis(1_133),
            Duration::from_millis(1_166),
            Duration::from_millis(1_200),
        ] {
            let _ = playout.push(VideoFrame::new_rgba(vec![0; 4].into(), 1, 1, pts));
        }

        let wait = maybe_pause_video_decode(&mut playout, &sync, &audio_position, &stats, &state);

        assert!(
            wait.is_some(),
            "steady held video should backpressure decode"
        );
        assert_eq!(
            playout.len(),
            4,
            "steady-state pause should not trim backlog"
        );
        assert_eq!(
            playout.front().expect("front frame").timestamp,
            Duration::from_millis(1_100)
        );
    }

    #[test]
    fn take_ready_front_frame_renders_at_most_one_ready_frame_per_pass() {
        let stats = crate::stats::DecodeStats::default();
        let audio_position = AudioPosition::default();
        audio_position.record(
            Duration::from_millis(1_000),
            Duration::from_millis(20),
            Duration::ZERO,
        );

        let mut sync = Some(VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        ));
        let _ = sync.as_mut().unwrap().decide(
            Duration::from_millis(1_000),
            Some(Duration::from_millis(1_000)),
            Instant::now(),
        );
        let state = VideoLoopState::new(Duration::from_millis(33));

        let mut playout = PlayoutBuffer::new(30);
        for pts in [Duration::from_millis(940), Duration::from_millis(970)] {
            let _ = playout.push(VideoFrame::new_rgba(vec![0; 4].into(), 1, 1, pts));
        }

        let action =
            take_ready_front_frame(&mut playout, &mut sync, &audio_position, &stats, &state);

        let FrontFrameAction::Render(frame) = action else {
            panic!("expected a ready frame to render");
        };
        assert_eq!(frame.timestamp, Duration::from_millis(940));
        assert_eq!(playout.len(), 1, "one ready frame should remain queued");
        assert_eq!(
            playout.front().expect("front frame").timestamp,
            Duration::from_millis(970)
        );
    }
}
