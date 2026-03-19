use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::format::VideoFrame;

/// Controls playout buffer behavior.
#[derive(Debug, Clone)]
pub enum PlayoutMode {
    /// Real-time playback with frame skipping for A/V sync.
    ///
    /// Frames are released at PTS-correct intervals offset by `buffer`
    /// from decode time. This absorbs bursty output from hardware
    /// decoders (DPB flushes). Late video frames may be skipped to
    /// maintain A/V sync with the audio master.
    ///
    /// `max_latency` is propagated to hang's `TrackConsumer` to skip
    /// stale groups at the transport level.
    Live {
        /// Display offset from decode time. Smooths DPB burst output.
        buffer: Duration,
        /// Transport-level ceiling. Hang skips groups older than this.
        max_latency: Duration,
    },

    /// Reliable playback: every frame is played in order, never skipped.
    ///
    /// No latency target — frames are released as soon as they are
    /// decoded. Hang's group-skip threshold is set very high to avoid
    /// dropping any groups. Good for recordings, demos, and debugging.
    Reliable,
}

impl Default for PlayoutMode {
    fn default() -> Self {
        Self::Live {
            buffer: Duration::ZERO,
            max_latency: Duration::from_millis(150),
        }
    }
}

impl PlayoutMode {
    /// Returns the effective max_latency for hang's `TrackConsumer`.
    pub fn hang_max_latency(&self) -> Duration {
        match self {
            Self::Live { max_latency, .. } => *max_latency,
            // Reliable: very high so hang never skips groups.
            Self::Reliable => Duration::from_secs(3600),
        }
    }
}

/// Shared playout clock for A/V synchronization and latency control.
///
/// Uses the hang-style independent delay approach: each track (audio
/// and video) independently maps its PTS to wall-clock playout times
/// via a shared base reference. No cross-track sync actions are needed
/// because both tracks share the same time anchor and buffer offset.
///
/// All tracks in a broadcast share the same clock instance. The clock
/// maps media timestamps to wall-clock playout times based on the
/// configured [`PlayoutMode`] and measures inter-arrival jitter as a
/// diagnostic.
#[derive(Debug, Clone)]
pub struct PlayoutClock {
    inner: Arc<Mutex<ClockInner>>,
}

#[derive(Debug)]
struct ClockInner {
    mode: PlayoutMode,

    /// Observed inter-arrival jitter (EMA-smoothed, RFC 3550 style).
    /// Diagnostic only — does not drive playout timing.
    smoothed_jitter: Duration,

    /// Wall clock ↔ media timestamp mapping, established on first frame.
    base_wall: Option<Instant>,
    base_pts: Option<Duration>,

    /// Previous arrival for jitter calculation.
    prev_arrival: Option<Instant>,
    prev_pts: Option<Duration>,

    /// Total accumulated drift from re-anchoring (diagnostic).
    /// Positive = we shifted forward (frames were late).
    total_reanchor_drift: Duration,
    /// Number of times the clock has been re-anchored.
    reanchor_count: u32,
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
                total_reanchor_drift: Duration::ZERO,
                reanchor_count: 0,
            })),
        }
    }

    /// Returns the current playout mode.
    pub fn mode(&self) -> PlayoutMode {
        self.inner.lock().expect("poisoned").mode.clone()
    }

    /// Sets the playout mode.
    ///
    /// If a base mapping exists, shifts `base_wall` by the difference in
    /// buffer offset so frames already in the playout buffer get correct
    /// new playout times without a gap. If no base exists yet, just stores
    /// the new mode.
    pub fn set_mode(&self, mode: PlayoutMode) {
        let mut inner = self.inner.lock().expect("poisoned");
        let old_buf = buffer_duration(&inner.mode);
        let new_buf = buffer_duration(&mode);
        if let Some(ref mut base_wall) = inner.base_wall {
            // Shift the anchor: increasing buffer → later playout, decreasing → earlier.
            let delta = new_buf.as_nanos() as i128 - old_buf.as_nanos() as i128;
            if delta >= 0 {
                *base_wall += Duration::from_nanos(delta as u64);
            } else {
                *base_wall -= Duration::from_nanos((-delta) as u64);
            }
        }
        tracing::debug!(
            old_buffer_ms = old_buf.as_millis(),
            new_buffer_ms = new_buf.as_millis(),
            has_base = inner.base_wall.is_some(),
            "playout clock: mode changed"
        );
        inner.mode = mode;
    }

    /// Returns the effective max_latency for hang's `TrackConsumer`.
    pub fn hang_max_latency(&self) -> Duration {
        self.inner.lock().expect("poisoned").mode.hang_max_latency()
    }

    /// Returns the current observed jitter (diagnostic).
    pub fn jitter(&self) -> Duration {
        self.inner.lock().expect("poisoned").smoothed_jitter
    }

    /// Returns the configured buffer duration (Live mode) or zero (Reliable).
    pub fn buffer(&self) -> Duration {
        let inner = self.inner.lock().expect("poisoned");
        buffer_duration(&inner.mode)
    }

    /// Sets the buffer duration for Live mode.
    ///
    /// No-op if the clock is in Reliable mode. If a base mapping exists,
    /// shifts it so frames already in the buffer get correct playout times.
    pub fn set_buffer(&self, new_buffer: Duration) {
        let mut inner = self.inner.lock().expect("poisoned");
        let old = buffer_duration(&inner.mode);
        if old == new_buffer {
            return;
        }
        if !matches!(inner.mode, PlayoutMode::Live { .. }) {
            return;
        }
        // Shift the anchor to account for the buffer change.
        if let Some(ref mut base_wall) = inner.base_wall {
            let delta = new_buffer.as_nanos() as i128 - old.as_nanos() as i128;
            if delta >= 0 {
                *base_wall += Duration::from_nanos(delta as u64);
            } else {
                *base_wall -= Duration::from_nanos((-delta) as u64);
            }
        }
        tracing::debug!(
            old_buffer_ms = old.as_millis(),
            new_buffer_ms = new_buffer.as_millis(),
            "playout clock: buffer updated"
        );
        if let PlayoutMode::Live { ref mut buffer, .. } = inner.mode {
            *buffer = new_buffer;
        }
    }

    /// Returns the total accumulated drift from re-anchoring and the count.
    pub fn reanchor_stats(&self) -> (Duration, u32) {
        let inner = self.inner.lock().expect("poisoned");
        (inner.total_reanchor_drift, inner.reanchor_count)
    }

    /// Records a frame arrival and updates the playout timeline.
    ///
    /// On the first call, establishes the PTS→wall-clock mapping with
    /// the full buffer offset (absorbs initial decoder DPB burst). On
    /// subsequent calls, re-anchors if the frame would play in the past
    /// (buffer underrun). Re-anchors use a **small jitter offset** (two
    /// frame intervals at 30fps ≈ 66ms) instead of the full buffer.
    /// This absorbs normal arrival jitter without the accumulation
    /// problem: even in the worst case, the decode-loop's 500ms skip
    /// threshold catches runaway accumulation (500ms / 66ms ≈ 7
    /// re-anchors). Using zero offset caused 90+ re-anchors per
    /// session, each producing a visible stutter.
    pub(crate) fn observe_arrival(&self, pts: Duration) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("poisoned");
        let offset = buffer_duration(&inner.mode);

        // Small jitter buffer for re-anchors: 2 frame intervals at 30fps.
        // Large enough to smooth normal arrival jitter, small enough that
        // accumulation triggers the skip mechanism before it gets bad.
        const REANCHOR_JITTER_BUFFER: Duration = Duration::from_millis(66);

        if inner.base_wall.is_none() {
            // First frame: establish base mapping with full buffer offset
            // to absorb initial decoder DPB burst output.
            inner.base_wall = Some(now + offset);
            inner.base_pts = Some(pts);
            tracing::debug!(
                buffer_ms = offset.as_millis(),
                pts_ms = pts.as_millis(),
                "playout clock: anchored (first frame)"
            );
        } else {
            // Check if this frame would play in the past (buffer underrun).
            let base_wall = inner.base_wall.unwrap();
            let base_pts = inner.base_pts.unwrap();
            let media_offset = pts.saturating_sub(base_pts);
            let playout = base_wall + media_offset;
            if now >= playout {
                let late_by = now - playout;
                // Re-anchor with small jitter buffer, not the full DPB
                // burst buffer. The stream is behind but we need some
                // smoothing to absorb normal network/decoder jitter.
                inner.base_wall = Some(now + REANCHOR_JITTER_BUFFER);
                inner.base_pts = Some(pts);
                inner.total_reanchor_drift += late_by;
                inner.reanchor_count += 1;
                tracing::debug!(
                    late_by_ms = late_by.as_millis(),
                    jitter_buf_ms = REANCHOR_JITTER_BUFFER.as_millis(),
                    total_drift_ms = inner.total_reanchor_drift.as_millis(),
                    reanchor_count = inner.reanchor_count,
                    pts_ms = pts.as_millis(),
                    "playout clock: re-anchored after buffer underrun"
                );
            }
        }

        // RFC 3550 jitter calculation (diagnostic only).
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
    pub(crate) fn playout_time(&self, pts: Duration) -> Option<Instant> {
        let inner = self.inner.lock().expect("poisoned");
        let base_wall = inner.base_wall?;
        let base_pts = inner.base_pts?;
        let media_offset = pts.saturating_sub(base_pts);
        Some(base_wall + media_offset)
    }

    /// Resets the clock's base mapping. Called when switching tracks or
    /// recovering from errors.
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("poisoned");
        tracing::debug!(
            total_drift_ms = inner.total_reanchor_drift.as_millis(),
            reanchor_count = inner.reanchor_count,
            "playout clock: reset"
        );
        inner.base_wall = None;
        inner.base_pts = None;
        inner.prev_arrival = None;
        inner.prev_pts = None;
        inner.smoothed_jitter = Duration::ZERO;
        inner.total_reanchor_drift = Duration::ZERO;
        inner.reanchor_count = 0;
    }
}

/// Returns the buffer/display offset for the given mode.
fn buffer_duration(mode: &PlayoutMode) -> Duration {
    match mode {
        PlayoutMode::Live { buffer, .. } => *buffer,
        // Reliable: no buffering offset, play as soon as decoded.
        PlayoutMode::Reliable => Duration::ZERO,
    }
}

/// Post-decoder frame buffer that smooths bursty decoder output.
///
/// Sits between the video decoder's `pop_frame()` and the output channel.
/// Frames are inserted as they come from the decoder and released when
/// their playout time arrives according to the shared [`PlayoutClock`].
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
    ///
    /// Frames are released based on the clock's PTS-to-wall-clock mapping.
    /// Each track independently gates on its playout time (hang-style
    /// independent delay). No cross-track comparison is needed.
    pub(crate) fn pop_ready(&mut self) -> Option<VideoFrame> {
        let front = self.buffer.front()?;
        let playout = self.clock.playout_time(front.timestamp)?;
        if Instant::now() < playout {
            return None;
        }
        self.buffer.pop_front()
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

    /// Returns the number of frames currently in the buffer.
    pub(crate) fn buf_len(&self) -> usize {
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
        let pts_interval = Duration::from_millis(33);
        for i in 0..32 {
            let pts = pts_interval * i;
            clock.observe_arrival(pts);
        }
        let jitter = clock.jitter();
        assert!(
            jitter < Duration::from_millis(100),
            "jitter should be small, got {jitter:?}"
        );
    }

    #[test]
    fn playout_clock_base_mapping_live() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            buffer: Duration::from_millis(80),
            max_latency: Duration::from_millis(150),
        });
        let pts0 = Duration::from_millis(0);
        let pts1 = Duration::from_millis(33);

        clock.observe_arrival(pts0);
        let t0 = clock.playout_time(pts0).expect("should have base");
        let t1 = clock.playout_time(pts1).expect("should have base");

        let diff = t1.duration_since(t0);
        assert!(
            (diff.as_millis() as i64 - 33).unsigned_abs() < 2,
            "playout times should be 33ms apart, got {diff:?}"
        );

        // Playout should be offset by ~buffer (80ms) from now.
        let now = Instant::now();
        let diff_from_now = if t0 > now {
            t0.duration_since(now)
        } else {
            now.duration_since(t0)
        };
        assert!(
            diff_from_now < Duration::from_millis(90),
            "playout should be near buffer offset, got {diff_from_now:?}"
        );
    }

    #[test]
    fn playout_clock_reliable_mode() {
        let clock = PlayoutClock::new(PlayoutMode::Reliable);
        let pts0 = Duration::from_millis(0);
        clock.observe_arrival(pts0);
        let t0 = clock.playout_time(pts0).expect("should have base");
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
        for i in 0..3 {
            buf.push(make_test_frame(Duration::from_millis(i * 33)));
        }
        assert_eq!(buf.len(), 3);
        let f0 = buf.pop_ready();
        assert!(f0.is_some(), "frame 0 should be ready");
    }

    #[test]
    fn playout_buffer_overflow_drops_oldest() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            buffer: Duration::from_secs(10),
            max_latency: Duration::from_secs(10),
        });
        let mut buf = PlayoutBuffer::new(clock);
        for i in 0..40 {
            buf.push(make_test_frame(Duration::from_millis(i * 33)));
        }
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
            buffer: Duration::from_millis(80),
            max_latency: Duration::from_millis(200),
        };
        assert_eq!(live.hang_max_latency(), Duration::from_millis(200));

        let reliable = PlayoutMode::Reliable;
        assert_eq!(reliable.hang_max_latency(), Duration::from_secs(3600));
    }

    #[test]
    fn playout_clock_set_mode_shifts_base() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            buffer: Duration::from_millis(80),
            max_latency: Duration::from_millis(150),
        });
        clock.observe_arrival(Duration::ZERO);
        let t_before = clock
            .playout_time(Duration::ZERO)
            .expect("should have base");

        // Switch to Reliable (buffer=0): playout times should shift earlier by 80ms.
        clock.set_mode(PlayoutMode::Reliable);
        let t_after = clock
            .playout_time(Duration::ZERO)
            .expect("base should still exist after set_mode");

        // t_after should be ~80ms earlier than t_before.
        let shift = t_before.duration_since(t_after);
        assert!(
            (shift.as_millis() as i64 - 80).unsigned_abs() < 5,
            "expected ~80ms shift, got {shift:?}"
        );

        // Switch to Live with larger buffer: should shift later.
        clock.set_mode(PlayoutMode::Live {
            buffer: Duration::from_millis(200),
            max_latency: Duration::from_millis(300),
        });
        let t_large = clock
            .playout_time(Duration::ZERO)
            .expect("base should still exist");
        let shift2 = t_large.duration_since(t_after);
        assert!(
            (shift2.as_millis() as i64 - 200).unsigned_abs() < 5,
            "expected ~200ms shift from reliable, got {shift2:?}"
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

    #[test]
    fn set_buffer_noop_in_reliable_mode() {
        let clock = PlayoutClock::new(PlayoutMode::Reliable);
        clock.observe_arrival(Duration::ZERO);
        let t_before = clock.playout_time(Duration::ZERO).unwrap();
        // set_buffer should be a no-op in Reliable mode.
        clock.set_buffer(Duration::from_millis(100));
        let t_after = clock.playout_time(Duration::ZERO).unwrap();
        assert_eq!(
            t_before, t_after,
            "set_buffer should be no-op in Reliable mode"
        );
        assert_eq!(clock.buffer(), Duration::ZERO);
    }

    #[test]
    fn set_buffer_shifts_anchor_in_live_mode() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            buffer: Duration::from_millis(50),
            max_latency: Duration::from_millis(150),
        });
        clock.observe_arrival(Duration::ZERO);
        let t_before = clock.playout_time(Duration::ZERO).unwrap();

        // Increase buffer by 100ms — playout should shift later.
        clock.set_buffer(Duration::from_millis(150));
        let t_after = clock.playout_time(Duration::ZERO).unwrap();
        let shift = t_after.duration_since(t_before);
        assert!(
            (shift.as_millis() as i64 - 100).unsigned_abs() < 5,
            "expected ~100ms shift, got {shift:?}"
        );
        assert_eq!(clock.buffer(), Duration::from_millis(150));
    }

    #[test]
    fn set_buffer_same_value_is_noop() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            buffer: Duration::from_millis(80),
            max_latency: Duration::from_millis(150),
        });
        clock.observe_arrival(Duration::ZERO);
        let t_before = clock.playout_time(Duration::ZERO).unwrap();
        clock.set_buffer(Duration::from_millis(80));
        let t_after = clock.playout_time(Duration::ZERO).unwrap();
        assert_eq!(t_before, t_after, "same buffer value should be no-op");
    }

    #[test]
    fn observe_arrival_reanchors_on_late_frame() {
        let clock = PlayoutClock::new(PlayoutMode::Live {
            buffer: Duration::from_millis(50),
            max_latency: Duration::from_millis(200),
        });
        // First frame anchors the clock.
        clock.observe_arrival(Duration::ZERO);
        let (drift_before, count_before) = clock.reanchor_stats();
        assert_eq!(count_before, 0);
        assert_eq!(drift_before, Duration::ZERO);

        // Sleep long enough that the next frame's playout time is in the past.
        std::thread::sleep(Duration::from_millis(100));

        // Second frame with PTS=33ms — playout time would be base_wall + 33ms,
        // which is ~50ms in the past. Should trigger re-anchor.
        clock.observe_arrival(Duration::from_millis(33));
        let (drift_after, count_after) = clock.reanchor_stats();
        assert_eq!(count_after, 1, "should have re-anchored once");
        assert!(drift_after > Duration::ZERO, "drift should be positive");
    }
}
