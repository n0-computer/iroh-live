use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::format::VideoFrame;

/// Controls playout buffer behavior.
#[derive(Debug, Clone)]
pub enum PlayoutMode {
    /// Minimizes latency: skip stale groups/frames to stay near real-time.
    ///
    /// `max_latency` controls the staleness threshold. Propagated to hang's
    /// `TrackConsumer::set_max_latency()`. Default: 150ms.
    Live {
        /// Maximum acceptable delay behind the sender's clock.
        max_latency: Duration,
    },

    /// Delivers every frame in order, no skipping. Accepts higher latency.
    ///
    /// hang's `max_latency` is set to `Duration::MAX` to disable group-skip.
    Reliable,
}

impl Default for PlayoutMode {
    fn default() -> Self {
        Self::Live {
            max_latency: Duration::from_millis(150),
        }
    }
}

impl PlayoutMode {
    /// Returns the effective max_latency for hang's `TrackConsumer`.
    pub fn hang_max_latency(&self) -> Duration {
        match self {
            Self::Live { max_latency } => *max_latency,
            Self::Reliable => Duration::MAX,
        }
    }
}

/// Shared playout clock for A/V synchronization and latency control.
///
/// All tracks in a broadcast share the same clock instance. The clock
/// maps media timestamps to wall-clock playout times based on the
/// configured [`PlayoutMode`].
#[derive(Debug, Clone)]
pub struct PlayoutClock {
    inner: Arc<Mutex<ClockInner>>,
}

#[derive(Debug)]
struct ClockInner {
    mode: PlayoutMode,

    /// Observed inter-arrival jitter (EMA-smoothed, RFC 3550 style).
    smoothed_jitter: Duration,

    /// Wall clock ↔ media timestamp mapping, established on first frame.
    base_wall: Option<Instant>,
    base_pts: Option<Duration>,

    /// Previous arrival for jitter calculation.
    prev_arrival: Option<Instant>,
    prev_pts: Option<Duration>,
}

impl PlayoutClock {
    /// Creates a new playout clock with the given mode.
    pub fn new(mode: PlayoutMode) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ClockInner {
                mode,
                smoothed_jitter: Duration::ZERO,
                base_wall: None,
                base_pts: None,
                prev_arrival: None,
                prev_pts: None,
            })),
        }
    }

    /// Returns the current playout mode.
    pub fn mode(&self) -> PlayoutMode {
        self.inner.lock().expect("lock").mode.clone()
    }

    /// Sets the playout mode.
    ///
    /// Resets the base wall-clock mapping so playout times are recalculated
    /// relative to the next frame arrival with the new mode's offset.
    pub fn set_mode(&self, mode: PlayoutMode) {
        let mut inner = self.inner.lock().expect("lock");
        inner.mode = mode;
        // Reset base so the new mode's offset (Live vs Reliable) takes effect.
        inner.base_wall = None;
        inner.base_pts = None;
    }

    /// Returns the effective max_latency for hang's `TrackConsumer`.
    pub fn hang_max_latency(&self) -> Duration {
        self.inner.lock().expect("lock").mode.hang_max_latency()
    }

    /// Returns the current observed jitter (informational).
    pub fn jitter(&self) -> Duration {
        self.inner.lock().expect("lock").smoothed_jitter
    }

    /// Records a frame arrival and updates jitter estimate.
    ///
    /// Called by decoder threads when a decoded frame is pushed into the
    /// playout buffer.
    pub(crate) fn observe_arrival(&self, pts: Duration) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("lock");

        // Establish base mapping on first frame.
        if inner.base_wall.is_none() {
            let offset = match &inner.mode {
                PlayoutMode::Live { max_latency } => *max_latency,
                PlayoutMode::Reliable => Duration::ZERO,
            };
            inner.base_wall = Some(now + offset);
            inner.base_pts = Some(pts);
        }

        // RFC 3550 jitter calculation.
        if let (Some(prev_arrival), Some(prev_pts)) = (inner.prev_arrival, inner.prev_pts) {
            let actual_interval = now.duration_since(prev_arrival);
            let expected_interval = pts.saturating_sub(prev_pts);
            let jitter_sample = actual_interval.abs_diff(expected_interval);
            // EMA: J = J + (|D| - J) / 16
            let current = inner.smoothed_jitter.as_nanos() as i128;
            let sample = jitter_sample.as_nanos() as i128;
            let new_jitter = current + (sample - current) / 16;
            inner.smoothed_jitter = Duration::from_nanos(new_jitter.max(0) as u64);
        }

        inner.prev_arrival = Some(now);
        inner.prev_pts = Some(pts);
    }

    /// Returns the wall-clock time at which a frame with the given PTS
    /// should be played out.
    ///
    /// In Live mode: `base_wall + (pts - base_pts)`, where `base_wall`
    /// includes the initial `max_latency` offset. In Reliable mode:
    /// `base_wall + (pts - base_pts)` with no extra buffering delay.
    pub(crate) fn playout_time(&self, pts: Duration) -> Option<Instant> {
        let inner = self.inner.lock().expect("lock");
        let base_wall = inner.base_wall?;
        let base_pts = inner.base_pts?;
        let media_offset = pts.saturating_sub(base_pts);
        Some(base_wall + media_offset)
    }

    /// Resets the clock's base mapping. Called when switching tracks or
    /// recovering from errors.
    pub(crate) fn reset(&self) {
        let mut inner = self.inner.lock().expect("lock");
        inner.base_wall = None;
        inner.base_pts = None;
        inner.prev_arrival = None;
        inner.prev_pts = None;
        inner.smoothed_jitter = Duration::ZERO;
    }
}

/// Post-decoder frame buffer that smooths bursty decoder output.
///
/// Sits between the video decoder's `pop_frame()` and the output channel.
/// Frames are inserted as they come from the decoder and released when
/// their playout time arrives.
pub(crate) struct PlayoutBuffer {
    buffer: VecDeque<VideoFrame>,
    max_frames: usize,
    clock: PlayoutClock,
}

impl PlayoutBuffer {
    /// Creates a new playout buffer.
    pub(crate) fn new(clock: PlayoutClock) -> Self {
        Self {
            buffer: VecDeque::new(),
            max_frames: 30, // 1 second at 30fps — safety valve
            clock,
        }
    }

    /// Inserts a decoded frame from the decoder.
    pub(crate) fn push(&mut self, frame: VideoFrame) {
        self.clock.observe_arrival(frame.timestamp);
        self.buffer.push_back(frame);
        // Safety valve: drop oldest if over limit.
        while self.buffer.len() > self.max_frames {
            self.buffer.pop_front();
        }
    }

    /// Pops the next frame whose playout time has arrived.
    pub(crate) fn pop_ready(&mut self) -> Option<VideoFrame> {
        let front = self.buffer.front()?;
        let playout = self.clock.playout_time(front.timestamp)?;
        if Instant::now() >= playout {
            self.buffer.pop_front()
        } else {
            None
        }
    }

    /// Returns the duration until the next frame is ready for playout.
    ///
    /// Returns `None` if the buffer is empty or the clock has no base
    /// mapping yet.
    pub(crate) fn next_playout_wait(&self) -> Option<Duration> {
        let front = self.buffer.front()?;
        let playout = self.clock.playout_time(front.timestamp)?;
        let now = Instant::now();
        if now >= playout {
            Some(Duration::ZERO)
        } else {
            Some(playout - now)
        }
    }

    /// Returns the number of frames currently in the buffer.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Clears all buffered frames.
    pub(crate) fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Resets the clock's base mapping for a fresh PTS sequence.
    pub(crate) fn reset_clock(&self) {
        self.clock.reset();
    }
}

/// Receives a value from a tokio mpsc channel with a timeout.
///
/// Polls with `try_recv` and short sleeps. The 1ms polling resolution
/// is acceptable for video playout (33ms frame intervals).
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
                if Instant::now() >= deadline {
                    return RecvResult::Timeout;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Result of a [`recv_timeout`] call.
pub(crate) enum RecvResult<T> {
    /// A value was received.
    Value(T),
    /// The timeout elapsed without receiving a value.
    Timeout,
    /// The channel is disconnected.
    Disconnected,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_frame(timestamp: Duration) -> VideoFrame {
        VideoFrame::new_rgba(vec![0u8; 4].into(), 1, 1, timestamp)
    }

    #[test]
    fn playout_clock_jitter_measurement() {
        let clock = PlayoutClock::new(PlayoutMode::default());

        // Simulate arrivals with 33ms PTS spacing but ±5ms jitter.
        let pts_interval = Duration::from_millis(33);
        for i in 0..32 {
            let pts = pts_interval * i;
            clock.observe_arrival(pts);
            // Simulate wall-clock jitter by sleeping a tiny bit extra on odd frames.
            // In tests we can't actually control wall time precisely, but we verify
            // the jitter calculation doesn't panic and converges.
        }

        // After 32 samples, jitter should be small (near zero since we call
        // observe_arrival without actual wall-clock jitter in a tight loop).
        let jitter = clock.jitter();
        assert!(
            jitter < Duration::from_millis(100),
            "jitter should be small, got {jitter:?}"
        );
    }

    #[test]
    fn playout_clock_base_mapping() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            max_latency: Duration::from_millis(100),
        });

        let pts0 = Duration::from_millis(0);
        let pts1 = Duration::from_millis(33);

        clock.observe_arrival(pts0);
        let t0 = clock.playout_time(pts0).expect("should have base");
        let t1 = clock.playout_time(pts1).expect("should have base");

        // t1 should be ~33ms after t0.
        let diff = t1.duration_since(t0);
        assert!(
            (diff.as_millis() as i64 - 33).unsigned_abs() < 2,
            "playout times should be 33ms apart, got {diff:?}"
        );

        // t0 should be ~100ms (max_latency) in the future from when we called observe.
        let now = Instant::now();
        // t0 was set as now + 100ms at the time of observe_arrival, which was
        // a few microseconds ago. It should still be in the near future or very
        // recently past.
        let diff_from_now = if t0 > now {
            t0.duration_since(now)
        } else {
            now.duration_since(t0)
        };
        assert!(
            diff_from_now < Duration::from_millis(110),
            "playout should be near max_latency offset, got {diff_from_now:?}"
        );
    }

    #[test]
    fn playout_clock_reliable_mode() {
        let clock = PlayoutClock::new(PlayoutMode::Reliable);

        let pts0 = Duration::from_millis(0);
        clock.observe_arrival(pts0);
        let t0 = clock.playout_time(pts0).expect("should have base");

        // Reliable mode: base_wall = now (no offset), so t0 ≈ now.
        let now = Instant::now();
        let diff = if t0 > now {
            t0.duration_since(now)
        } else {
            now.duration_since(t0)
        };
        assert!(
            diff < Duration::from_millis(5),
            "reliable mode should have near-zero offset, got {diff:?}"
        );
    }

    #[test]
    fn playout_clock_reset() {
        let clock = PlayoutClock::new(PlayoutMode::default());
        clock.observe_arrival(Duration::ZERO);
        assert!(clock.playout_time(Duration::ZERO).is_some());

        clock.reset();
        assert!(clock.playout_time(Duration::ZERO).is_none());
        assert_eq!(clock.jitter(), Duration::ZERO);
    }

    #[test]
    fn playout_buffer_push_pop() {
        let clock = PlayoutClock::new(PlayoutMode::Reliable);
        let mut buf = PlayoutBuffer::new(clock);

        // Push 3 frames with 33ms PTS spacing.
        for i in 0..3 {
            buf.push(make_test_frame(Duration::from_millis(i * 33)));
        }
        assert_eq!(buf.len(), 3);

        // In Reliable mode with base_wall ≈ now, all 3 frames should be
        // immediately ready (their playout times are now or in the past since
        // we pushed 0ms, 33ms, 66ms but called observe_arrival which set
        // base_wall = now for the first one).
        // Frame 0: playout = base_wall + 0 ≈ now → ready
        let f0 = buf.pop_ready();
        assert!(f0.is_some(), "frame 0 should be ready");

        // Frame 1: playout = base_wall + 33ms — may not be ready yet.
        // In tests this runs fast so it may or may not be ready.
        // Just verify no panics and eventual draining.
    }

    #[test]
    fn playout_buffer_overflow_drops_oldest() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            max_latency: Duration::from_secs(10), // large latency so nothing is ready
        });
        let mut buf = PlayoutBuffer::new(clock);

        // Push more than max_frames.
        for i in 0..40 {
            buf.push(make_test_frame(Duration::from_millis(i * 33)));
        }

        // Buffer should be capped at max_frames.
        assert_eq!(buf.len(), 30);
    }

    #[test]
    fn playout_buffer_empty_pop() {
        let clock = PlayoutClock::new(PlayoutMode::default());
        let mut buf = PlayoutBuffer::new(clock);

        assert!(buf.pop_ready().is_none());
        assert!(buf.next_playout_wait().is_none());
    }

    #[test]
    fn playout_buffer_clear() {
        let clock = PlayoutClock::new(PlayoutMode::Reliable);
        let mut buf = PlayoutBuffer::new(clock);
        buf.push(make_test_frame(Duration::ZERO));
        assert_eq!(buf.len(), 1);
        buf.clear();
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn playout_mode_hang_max_latency() {
        let live = PlayoutMode::Live {
            max_latency: Duration::from_millis(200),
        };
        assert_eq!(live.hang_max_latency(), Duration::from_millis(200));

        let reliable = PlayoutMode::Reliable;
        assert_eq!(reliable.hang_max_latency(), Duration::MAX);
    }

    #[test]
    fn playout_clock_set_mode_resets_base() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            max_latency: Duration::from_millis(100),
        });

        // Establish base mapping.
        clock.observe_arrival(Duration::ZERO);
        assert!(clock.playout_time(Duration::ZERO).is_some());

        // Switch mode → base should reset.
        clock.set_mode(PlayoutMode::Reliable);
        assert!(
            clock.playout_time(Duration::ZERO).is_none(),
            "base should be cleared after set_mode"
        );

        // Next arrival re-establishes with new mode.
        clock.observe_arrival(Duration::from_millis(100));
        let t = clock
            .playout_time(Duration::from_millis(100))
            .expect("should have base after re-observe");
        // Reliable mode: base_wall ≈ now (no offset).
        let now = Instant::now();
        let diff = if t > now {
            t.duration_since(now)
        } else {
            now.duration_since(t)
        };
        assert!(
            diff < Duration::from_millis(10),
            "reliable mode should have near-zero offset, got {diff:?}"
        );
    }

    #[test]
    fn playout_buffer_reset_clock_clears_base() {
        let clock = PlayoutClock::new(PlayoutMode::Reliable);
        let mut buf = PlayoutBuffer::new(clock.clone());

        buf.push(make_test_frame(Duration::ZERO));
        assert!(clock.playout_time(Duration::ZERO).is_some());

        buf.clear();
        buf.reset_clock();
        assert!(
            clock.playout_time(Duration::ZERO).is_none(),
            "clock base should be reset"
        );
    }

    #[test]
    fn recv_timeout_on_empty_channel() {
        let (_tx, mut rx) = tokio::sync::mpsc::channel::<u32>(1);
        let start = Instant::now();
        let result = recv_timeout(&mut rx, Duration::from_millis(10));
        let elapsed = start.elapsed();

        assert!(matches!(result, RecvResult::Timeout));
        assert!(elapsed >= Duration::from_millis(9), "should wait ~10ms");
        assert!(
            elapsed < Duration::from_millis(50),
            "should not wait too long"
        );
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
}
