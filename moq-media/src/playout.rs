//! Video playout primitives and playback policy.
//!
//! We separate playback policy into two independent concerns:
//!
//! - [`SyncMode`] controls how we align decoded audio and video at playout
//!   time.
//! - [`FreshnessPolicy`] controls how much stale media we are willing to keep
//!   after a stall before we skip forward.
//!
//! This separation is intentional. Users usually tune these concerns for
//! different reasons:
//!
//! - You tune [`SyncMode::AudioMaster`] when you want to trade video smoothness
//!   against A/V alignment. A larger `video_hold_budget` gives video more time
//!   to wait for audio and smooth decoder bursts, at the cost of added video
//!   delay.
//! - You tune [`FreshnessPolicy::max_stale_duration`] when you want to trade
//!   continuity against recovery speed after congestion or loss. A larger value
//!   tolerates longer stalls before we skip forward; a smaller value returns to
//!   the live edge faster.
//!
//! Internally, playout is intentionally narrow:
//!
//! 1. [`PlayoutBuffer`] stores a bounded queue of decoded video frames.
//! 2. [`VideoSyncController`] decides whether the front frame should be held,
//!    rendered now, or dropped based on estimated audio playback position.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::format::VideoFrame;

/// A/V synchronization behavior at playout time.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncMode {
    /// Audio drives playout and video follows it.
    ///
    /// This is the default for interactive playback. Audio keeps continuity
    /// first. Video may wait up to `video_hold_budget`, or drop late frames, to
    /// stay close to the estimated speaker position.
    AudioMaster {
        /// Maximum time we intentionally hold a decoded video frame before
        /// rendering it.
        ///
        /// Increase this when decoder bursts or moderate jitter cause visible
        /// jank after recovery. Decrease it when you prefer lower delay and can
        /// tolerate more video drops.
        video_hold_budget: Duration,
    },

    /// Frames render as soon as they decode.
    ///
    /// This mode is useful for diagnostics and local tests where you want to
    /// see raw decode output without active A/V synchronization.
    Unmanaged,
}

impl Default for SyncMode {
    fn default() -> Self {
        Self::audio_master()
    }
}

impl SyncMode {
    /// Creates [`SyncMode::AudioMaster`] with a 150 ms video hold budget.
    pub fn audio_master() -> Self {
        Self::AudioMaster {
            video_hold_budget: Duration::from_millis(150),
        }
    }

    /// Creates [`SyncMode::AudioMaster`] with an explicit hold budget.
    pub fn audio_master_with_hold(video_hold_budget: Duration) -> Self {
        Self::AudioMaster { video_hold_budget }
    }

    /// Creates [`SyncMode::Unmanaged`].
    pub fn unmanaged() -> Self {
        Self::Unmanaged
    }

    /// Returns the configured video hold budget.
    pub fn video_hold_budget(&self) -> Duration {
        match self {
            Self::AudioMaster { video_hold_budget } => *video_hold_budget,
            Self::Unmanaged => Duration::ZERO,
        }
    }

    pub(crate) fn is_audio_master(&self) -> bool {
        matches!(self, Self::AudioMaster { .. })
    }
}

/// Freshness policy for ordered media delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreshnessPolicy {
    /// Maximum duration of stale media we are willing to keep before we skip
    /// forward to newer data.
    ///
    /// This value serves two roles:
    ///
    /// - it is the `max_latency` passed to Hang's ordered consumer, so older
    ///   groups may be dropped before decode;
    /// - it is the late-packet threshold for the video decode loop, which may
    ///   skip forward to the next keyframe once packets are clearly behind the
    ///   current audio playout position.
    ///
    /// Use a larger value when you want to preserve more continuity through
    /// congestion. Use a smaller value when you want faster recovery back to
    /// the live edge.
    pub max_stale_duration: Duration,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self {
            max_stale_duration: Duration::from_millis(500),
        }
    }
}

impl FreshnessPolicy {
    /// Creates a freshness policy with the given maximum stale duration.
    pub fn new(max_stale_duration: Duration) -> Self {
        Self { max_stale_duration }
    }
}

/// Public playback policy for a subscribed broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Cross-track synchronization policy.
    pub sync: SyncMode,
    /// Staleness policy for ordered delivery and decode recovery.
    pub freshness: FreshnessPolicy,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self::audio_master()
    }
}

impl PlaybackPolicy {
    /// Creates the default interactive policy: audio-master sync with a 150 ms
    /// video hold budget and a 500 ms freshness threshold.
    pub fn audio_master() -> Self {
        Self {
            sync: SyncMode::audio_master(),
            freshness: FreshnessPolicy::default(),
        }
    }

    /// Creates an audio-master policy with an explicit hold budget.
    pub fn audio_master_with_hold(video_hold_budget: Duration) -> Self {
        Self {
            sync: SyncMode::audio_master_with_hold(video_hold_budget),
            freshness: FreshnessPolicy::default(),
        }
    }

    /// Creates an unmanaged policy with the default freshness threshold.
    pub fn unmanaged() -> Self {
        Self {
            sync: SyncMode::unmanaged(),
            freshness: FreshnessPolicy::default(),
        }
    }

    /// Returns a copy with a different freshness policy.
    pub fn with_freshness(mut self, freshness: FreshnessPolicy) -> Self {
        self.freshness = freshness;
        self
    }

    /// Returns a copy with a different sync mode.
    pub fn with_sync(mut self, sync: SyncMode) -> Self {
        self.sync = sync;
        self
    }

    pub(crate) fn is_audio_master(&self) -> bool {
        self.sync.is_audio_master()
    }
}

// ── PlayoutBuffer ─────────────────────────────────────────────────

/// Post-decoder video frame queue.
///
/// The queue is intentionally small and bounded. When it is full, we drop the
/// newest decoded frame instead of resetting to the newest frame. Ordered
/// delivery already gives us monotonic video input, so keeping the current
/// front of the queue stable is more valuable than repeatedly throwing away
/// the whole backlog.
pub(crate) struct PlayoutBuffer {
    buffer: VecDeque<VideoFrame>,
    max_frames: usize,
}

impl PlayoutBuffer {
    pub(crate) fn new(max_frames: usize) -> Self {
        Self {
            buffer: VecDeque::new(),
            max_frames,
        }
    }

    /// Inserts a decoded frame.
    ///
    /// Returns `true` when the queue was already full and the new frame was
    /// dropped. Under normal operation decode backpressure keeps us below this
    /// bound; hitting it usually means the decoder emitted a burst before the
    /// release loop could run.
    pub(crate) fn push(&mut self, frame: VideoFrame) -> bool {
        if self.buffer.len() >= self.max_frames {
            tracing::debug!(
                max_frames = self.max_frames,
                pts_ms = frame.timestamp.as_millis(),
                "playout buffer: dropping newest frame because the queue is full"
            );
            return true;
        }

        self.buffer.push_back(frame);
        false
    }

    pub(crate) fn front(&self) -> Option<&VideoFrame> {
        self.buffer.front()
    }

    pub(crate) fn pop_front(&mut self) -> Option<VideoFrame> {
        self.buffer.pop_front()
    }

    pub(crate) fn drop_front(&mut self) -> Option<VideoFrame> {
        self.buffer.pop_front()
    }

    pub(crate) fn len(&self) -> usize {
        self.buffer.len()
    }

    pub(crate) fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Drops all queued frames except the current front frame.
    ///
    /// This is used when video is already far ahead of audio. Keeping a deep
    /// queue of future frames does not improve sync, but it can retain scarce
    /// decoder-backed resources such as VAAPI output surfaces.
    pub(crate) fn trim_to_front(&mut self) -> usize {
        let dropped = self.buffer.len().saturating_sub(1);
        self.buffer.truncate(1);
        dropped
    }
}

// ── recv_timeout ──────────────────────────────────────────────────

/// Receives a value from a tokio mpsc channel with a timeout.
pub(crate) fn recv_timeout<T>(
    rx: &mut tokio::sync::mpsc::Receiver<T>,
    timeout: Duration,
) -> RecvResult<T> {
    use tokio::sync::mpsc::error::TryRecvError;

    let deadline = Instant::now() + timeout;
    loop {
        match rx.try_recv() {
            Ok(val) => return RecvResult::Value(val),
            Err(TryRecvError::Disconnected) => return RecvResult::Disconnected,
            Err(TryRecvError::Empty) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return RecvResult::Timeout;
                }
                std::thread::sleep(remaining.min(Duration::from_millis(2)) / 2);
            }
        }
    }
}

/// Result of a [`recv_timeout`] call.
pub(crate) enum RecvResult<T> {
    Value(T),
    Timeout,
    Disconnected,
}

// ── VideoSyncController ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncState {
    CatchUp,
    Locked,
}

impl SyncState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CatchUp => "catchup",
            Self::Locked => "locked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDecision {
    Hold(Duration),
    RenderNow,
    DropLate,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VideoSyncConfig {
    pub video_hold_target: Duration,
    pub sync_window: Duration,
    pub late_drop_threshold: Duration,
    pub stall_timeout: Duration,
}

impl VideoSyncConfig {
    pub(crate) fn from_policy(policy: &PlaybackPolicy, framerate: f64) -> Option<Self> {
        let SyncMode::AudioMaster { video_hold_budget } = &policy.sync else {
            return None;
        };
        let frame_interval = Duration::from_secs_f64(1.0 / framerate.max(1.0));
        let sync_window = Duration::from_micros(
            frame_interval
                .max(Duration::from_millis(50))
                .as_micros()
                .min(u64::MAX as u128) as u64,
        );
        Some(Self {
            video_hold_target: *video_hold_budget,
            // Sync tolerance must reflect frame cadence. Low-fps streams cannot
            // physically present video inside a narrow sub-frame window, so the
            // lock target must be at least "good enough" (~50ms) and never
            // smaller than one frame interval.
            sync_window,
            late_drop_threshold: (*video_hold_budget).max(sync_window * 2),
            stall_timeout: Duration::from_millis(300),
        })
    }

    pub(crate) fn decode_pause_queue_len(self) -> usize {
        // Once video is being intentionally held, a small queue is enough to
        // bridge decoder burstiness. Letting the queue grow past this point
        // only creates future backlog that will later be trimmed or overflowed.
        let frames = (self.video_hold_target.as_secs_f64() / self.sync_window.as_secs_f64())
            .ceil()
            .max(2.0) as usize;
        frames + 1
    }
}

#[derive(Debug)]
pub(crate) struct VideoSyncController {
    /// Current diagnostic mode of the controller.
    state: SyncState,
    /// The very first frame is always rendered immediately to avoid startup
    /// deadlock before we have a usable playout estimate.
    first_frame_sent: bool,
    /// Last time we saw a video packet arrive from transport.
    last_video_arrival: Option<Instant>,
    /// Last skip generation observed from the decode loop.
    last_skip_generation: u64,
    config: VideoSyncConfig,
}

impl VideoSyncController {
    pub(crate) fn new(config: VideoSyncConfig) -> Self {
        Self {
            state: SyncState::CatchUp,
            first_frame_sent: false,
            last_video_arrival: None,
            last_skip_generation: 0,
            config,
        }
    }

    pub(crate) fn state(&self) -> SyncState {
        self.state
    }

    pub(crate) fn decode_pause_queue_len(&self) -> usize {
        self.config.decode_pause_queue_len()
    }

    pub(crate) fn note_arrival(&mut self, now: Instant) {
        self.last_video_arrival = Some(now);
    }

    pub(crate) fn force_catchup(&mut self) {
        self.state = SyncState::CatchUp;
    }

    pub(crate) fn on_skip_generation(&mut self, generation: u64) {
        if generation != self.last_skip_generation {
            self.last_skip_generation = generation;
            self.force_catchup();
        }
    }

    /// Falls back to catchup if transport has gone quiet for too long.
    fn enforce_stall_timeout(&mut self, now: Instant) {
        if self.state == SyncState::Locked
            && let Some(last) = self.last_video_arrival
            && now.duration_since(last) > self.config.stall_timeout
        {
            self.force_catchup();
        }
    }

    /// Decides whether the current front frame should be held, rendered, or
    /// dropped relative to the estimated audio playout position.
    pub(crate) fn decide(
        &mut self,
        frame_pts: Duration,
        audio_pts: Option<Duration>,
        now: Instant,
    ) -> FrameDecision {
        self.enforce_stall_timeout(now);

        if !self.first_frame_sent {
            self.first_frame_sent = true;
            return FrameDecision::RenderNow;
        }

        let Some(audio_pts) = audio_pts else {
            self.force_catchup();
            return FrameDecision::RenderNow;
        };

        let delta = frame_pts.as_micros() as i128 - audio_pts.as_micros() as i128;
        let sync_window = self.config.sync_window.as_micros() as i128;
        let late_drop = self.config.late_drop_threshold.as_micros() as i128;

        if delta < -late_drop {
            self.state = SyncState::CatchUp;
            return FrameDecision::DropLate;
        }

        if delta > sync_window {
            let steady_early = self.config.video_hold_target.as_micros() as i128;
            self.state = if delta <= steady_early {
                SyncState::Locked
            } else {
                SyncState::CatchUp
            };
            return FrameDecision::Hold(Duration::from_micros((delta - sync_window) as u64));
        }

        self.state = if delta >= -sync_window {
            SyncState::Locked
        } else {
            SyncState::CatchUp
        };
        FrameDecision::RenderNow
    }

    /// Returns the earliest time at which `frame_pts` could be rendered
    /// without violating the current sync window.
    pub(crate) fn wait_hint(&self, frame_pts: Duration, audio_pts: Option<Duration>) -> Duration {
        let Some(audio_pts) = audio_pts else {
            return Duration::ZERO;
        };
        if !self.first_frame_sent {
            return Duration::ZERO;
        }
        let delta = frame_pts.as_micros() as i128 - audio_pts.as_micros() as i128;
        let sync_window = self.config.sync_window.as_micros() as i128;
        if delta <= sync_window {
            Duration::ZERO
        } else {
            Duration::from_micros((delta - sync_window) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_frame(timestamp: Duration) -> VideoFrame {
        VideoFrame::new_rgba(vec![0u8; 4].into(), 1, 1, timestamp)
    }

    #[test]
    fn playout_buffer_push_pop() {
        let mut buf = PlayoutBuffer::new(30);
        for i in 0..3 {
            assert!(!buf.push(make_test_frame(Duration::from_millis(i * 33))));
        }
        assert_eq!(buf.len(), 3);
        let f0 = buf.pop_front();
        assert!(f0.is_some());
    }

    #[test]
    fn playout_buffer_full_drops_newest_frame() {
        let mut buf = PlayoutBuffer::new(3);
        for i in 0..3 {
            assert!(!buf.push(make_test_frame(Duration::from_millis(i))));
        }
        assert!(buf.push(make_test_frame(Duration::from_millis(99))));
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.front().expect("front frame").timestamp, Duration::ZERO);
    }

    #[test]
    fn playout_buffer_trim_to_front_drops_future_backlog() {
        let mut buf = PlayoutBuffer::new(30);
        for i in 0..4 {
            let _ = buf.push(make_test_frame(Duration::from_millis(i * 33)));
        }

        let dropped = buf.trim_to_front();

        assert_eq!(dropped, 3);
        assert_eq!(buf.len(), 1);
        assert_eq!(
            buf.front().expect("front frame").timestamp,
            Duration::from_millis(0)
        );
    }

    #[test]
    fn playback_policy_keeps_sync_and_freshness_separate() {
        let policy = PlaybackPolicy::audio_master_with_hold(Duration::from_millis(80))
            .with_freshness(FreshnessPolicy::new(Duration::from_millis(200)));

        assert_eq!(policy.sync.video_hold_budget(), Duration::from_millis(80));
        assert_eq!(
            policy.freshness.max_stale_duration,
            Duration::from_millis(200)
        );
    }

    #[test]
    fn recv_timeout_on_empty_channel() {
        let (_tx, mut rx) = tokio::sync::mpsc::channel::<u32>(1);
        let start = Instant::now();
        let result = recv_timeout(&mut rx, Duration::from_millis(10));
        let elapsed = start.elapsed();
        assert!(matches!(result, RecvResult::Timeout));
        assert!(elapsed >= Duration::from_millis(9));
        assert!(elapsed < Duration::from_millis(50));
    }

    #[test]
    fn recv_timeout_on_disconnected_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u32>(1);
        drop(tx);
        let result = recv_timeout(&mut rx, Duration::from_millis(100));
        assert!(matches!(result, RecvResult::Disconnected));
    }

    #[test]
    fn recv_timeout_with_value() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u32>(1);
        tx.try_send(42).unwrap();
        let result = recv_timeout(&mut rx, Duration::from_millis(100));
        assert!(matches!(result, RecvResult::Value(42)));
    }

    #[test]
    fn controller_enters_catchup_when_audio_appears() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        assert_eq!(
            c.decide(Duration::ZERO, None, Instant::now()),
            FrameDecision::RenderNow
        );
        assert_eq!(c.state(), SyncState::CatchUp);
        assert_eq!(
            c.decide(
                Duration::from_millis(33),
                Some(Duration::ZERO),
                Instant::now()
            ),
            FrameDecision::RenderNow
        );
        assert_eq!(c.state(), SyncState::Locked);
    }

    #[test]
    fn controller_drops_late_frames_in_catchup() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        let _ = c.decide(Duration::ZERO, Some(Duration::ZERO), Instant::now());
        assert_eq!(
            c.decide(
                Duration::ZERO,
                Some(Duration::from_millis(200)),
                Instant::now()
            ),
            FrameDecision::DropLate
        );
        assert_eq!(c.state(), SyncState::CatchUp);
    }

    #[test]
    fn controller_never_zero_holds_late_frame_in_catchup() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        let _ = c.decide(Duration::ZERO, Some(Duration::ZERO), Instant::now());
        assert_eq!(
            c.decide(
                Duration::from_millis(3100),
                Some(Duration::from_millis(3235)),
                Instant::now(),
            ),
            FrameDecision::RenderNow
        );
    }

    #[test]
    fn controller_locks_when_frame_is_inside_sync_window() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        let _ = c.decide(Duration::ZERO, Some(Duration::ZERO), Instant::now());
        let _ = c.decide(
            Duration::from_millis(66),
            Some(Duration::from_millis(66)),
            Instant::now(),
        );
        assert_eq!(c.state(), SyncState::Locked);
    }

    #[test]
    fn controller_holds_early_frames_until_audio_catches_up() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        let _ = c.decide(Duration::ZERO, Some(Duration::ZERO), Instant::now());
        assert_eq!(
            c.decide(
                Duration::from_millis(300),
                Some(Duration::from_millis(100)),
                Instant::now(),
            ),
            FrameDecision::Hold(Duration::from_millis(150))
        );
        assert_eq!(c.state(), SyncState::CatchUp);
    }

    #[test]
    fn controller_renders_slightly_late_frames_for_continuity() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        let _ = c.decide(Duration::ZERO, Some(Duration::ZERO), Instant::now());
        assert_eq!(
            c.decide(
                Duration::from_millis(170),
                Some(Duration::from_millis(200)),
                Instant::now(),
            ),
            FrameDecision::RenderNow
        );
        assert_eq!(c.state(), SyncState::Locked);
    }

    #[test]
    fn sync_window_scales_with_frame_interval() {
        let low_fps = VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 15.0).unwrap();
        let high_fps = VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 60.0).unwrap();

        assert_eq!(low_fps.sync_window, Duration::from_micros(66_666));
        assert_eq!(high_fps.sync_window, Duration::from_millis(50));
        assert_eq!(low_fps.late_drop_threshold, Duration::from_millis(150));
        assert_eq!(high_fps.late_drop_threshold, Duration::from_millis(150));
    }

    #[test]
    fn controller_forces_catchup_on_skip_generation_change() {
        let mut c = VideoSyncController::new(
            VideoSyncConfig::from_policy(&PlaybackPolicy::audio_master(), 30.0).unwrap(),
        );
        let _ = c.decide(Duration::ZERO, Some(Duration::ZERO), Instant::now());
        let _ = c.decide(
            Duration::from_millis(33),
            Some(Duration::from_millis(33)),
            Instant::now(),
        );
        assert_eq!(c.state(), SyncState::Locked);
        c.on_skip_generation(1);
        assert_eq!(c.state(), SyncState::CatchUp);
    }
}
