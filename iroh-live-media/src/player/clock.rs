//! A player's playout clock, which keeps its audio and video aligned.
//!
//! Ported from `moq/js` at commit `53fe78d8`, `js/watch/src/sync.ts`. The
//! arithmetic matches the JS source, in `i64` milliseconds, so the two compare
//! without rounding. `moq-video` and `moq-audio` have no counterpart.
//!
//! The reference is the earliest `wall_now - frame_pts` seen. It only moves
//! earlier, so it estimates the wall time at media time zero. A frame stamped
//! `T` is due at `reference + T + latency`, where the latency is the jitter
//! allowance plus the audio queued at the speaker.
//!
//! The video path calls [`PlayoutClock::received`] as each frame decodes and
//! [`PlayoutClock::wait_async`] before handing it to the renderer. Only video
//! moves the reference. The audio path reports its buffer depth through an
//! [`AudioLatency`] guard, and video is held back by it. Without that, a video
//! frame would render while its audio is still queued behind 50 ms of sound.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How long a frame still has to wait before it is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delay {
    /// The frame is due now.
    Now,
    /// The frame is due after this long.
    After(Duration),
    /// The clock is closed. Stop the pipeline.
    Closed,
}

/// The clock that keeps a player's audio and video in step.
///
/// Clones share state. Each player has one, shared by its video and audio
/// paths.
#[derive(Clone, Debug)]
pub(crate) struct PlayoutClock {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// The construction time. `base.elapsed()` stands in for `performance.now()`.
    base: Instant,

    state: Mutex<State>,

    /// Wakes [`PlayoutClock::wait_async`] when the clock changes or closes.
    changed: tokio::sync::Notify,
}

/// The mutable state, with durations in signed `i64` milliseconds.
#[derive(Debug)]
struct State {
    /// The earliest `now_ms - pts_ms` seen on the current timeline, if any.
    reference: Option<i64>,

    /// The jitter allowance in ms.
    jitter_ms: i64,

    /// The audio queued ahead of the speaker in ms, as last reported.
    audio_ms: Option<i64>,

    /// Set by [`PlayoutClock::close`] to make every wait return at once.
    closed: bool,
}

impl State {
    /// Returns the total latency, `audio + jitter`, in ms.
    fn latency_ms(&self) -> i64 {
        self.audio_ms.unwrap_or(0) + self.jitter_ms
    }
}

impl PlayoutClock {
    /// Creates a playout clock with the jitter allowance `jitter`.
    pub(crate) fn new(jitter: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                base: Instant::now(),
                state: Mutex::new(State {
                    reference: None,
                    jitter_ms: jitter.as_millis() as i64,
                    audio_ms: None,
                    closed: false,
                }),
                changed: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Records the arrival of a frame stamped `timestamp`.
    ///
    /// Moves the reference earlier if the frame arrived ahead of every earlier
    /// one. Only the video path calls this.
    pub(crate) fn received(&self, timestamp: Duration) {
        let now_ms = self.now_ms();
        let timestamp_ms = timestamp.as_millis() as i64;
        let ref_val = now_ms - timestamp_ms;

        let mut state = self.inner.state.lock().expect("poisoned");
        if state.reference.is_some_and(|current| ref_val >= current) {
            return;
        }

        state.reference = Some(ref_val);
        self.inner.changed.notify_waiters();
    }

    /// Starts the reference over, for a broadcast on a new route.
    ///
    /// A publisher behind the new route may have restarted its timestamps.
    /// Against the old reference, every later frame would be overdue and
    /// nothing would be paced.
    pub(crate) fn restart(&self) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.reference = None;
        self.inner.changed.notify_waiters();
    }

    /// Waits until the frame stamped `timestamp` is due.
    ///
    /// Recomputes the delay whenever the clock changes during the wait.
    /// Returns `true` when the frame should render, and `false` once the clock
    /// is closed.
    pub(crate) async fn wait_async(&self, timestamp: Duration) -> bool {
        loop {
            // Register before reading the delay, so a `close` or a reference
            // update between the two is not missed.
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            match self.delay(timestamp) {
                Delay::Closed => return false,
                Delay::Now => return true,
                Delay::After(sleep) => {
                    // A clock change or a close wakes the wait early, so a
                    // shutdown does not wait out the playout latency.
                    tokio::select! {
                        _ = tokio::time::sleep(sleep) => return true,
                        _ = changed => continue,
                    }
                }
            }
        }
    }

    /// Returns how long the frame stamped `timestamp` still has to wait.
    ///
    /// This is the arithmetic behind [`wait_async`](Self::wait_async), for a
    /// caller that drives its own timer.
    pub(crate) fn delay(&self, timestamp: Duration) -> Delay {
        let timestamp_ms = timestamp.as_millis() as i64;
        let state = self.inner.state.lock().expect("poisoned");

        if state.closed {
            return Delay::Closed;
        }
        // No reference yet: render now.
        let Some(current_ref) = state.reference else {
            return Delay::Now;
        };

        let sleep_ms = (current_ref - (self.now_ms() - timestamp_ms)) + state.latency_ms();
        match sleep_ms > 0 {
            true => Delay::After(Duration::from_millis(sleep_ms as u64)),
            false => Delay::Now,
        }
    }

    /// Returns the current total latency: `audio + jitter`.
    pub(crate) fn latency(&self) -> Duration {
        let state = self.inner.state.lock().expect("poisoned");
        Duration::from_millis(state.latency_ms().max(0) as u64)
    }

    /// Sets the jitter allowance, and wakes waits to recompute their delay.
    pub(crate) fn set_jitter(&self, jitter: Duration) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.jitter_ms = jitter.as_millis() as i64;
        self.inner.changed.notify_waiters();
    }

    /// Sets how much audio is queued ahead of the speaker.
    ///
    /// Only an [`AudioLatency`] guard calls this, so the value cannot outlive
    /// the audio path that reported it.
    fn set_audio_buffered(&self, latency: Option<Duration>) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.audio_ms = latency.map(|d| d.as_millis() as i64);
        self.inner.changed.notify_waiters();
    }

    /// Registers an audio path's contribution to the latency.
    ///
    /// The audio path reports its buffer depth through the returned guard.
    /// Dropping the guard clears the contribution, so an audio track that
    /// stops no longer holds video back.
    pub(crate) fn register_audio(&self) -> AudioLatency {
        AudioLatency {
            clock: self.clone(),
        }
    }

    /// Closes the clock, so current and later waits return at once.
    ///
    /// The JS source has no counterpart because it relies on effect cleanup.
    pub(crate) fn close(&self) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.closed = true;
        self.inner.changed.notify_waiters();
    }

    /// Returns the milliseconds since construction, like `performance.now()`.
    fn now_ms(&self) -> i64 {
        self.inner.base.elapsed().as_millis() as i64
    }
}

/// An audio path's registration with a playout clock.
///
/// Created by [`PlayoutClock::register_audio`]. Dropping it clears the audio
/// path's report, so a stopped track no longer holds video back.
#[derive(Debug)]
pub(crate) struct AudioLatency {
    clock: PlayoutClock,
}

impl AudioLatency {
    /// Reports how much audio is queued ahead of the speaker now.
    pub(crate) fn set(&self, buffered: Duration) {
        self.clock.set_audio_buffered(Some(buffered));
    }
}

impl Drop for AudioLatency {
    fn drop(&mut self) {
        self.clock.set_audio_buffered(None);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn received_tracks_minimum_reference() {
        let sync = PlayoutClock::new(Duration::from_millis(100));

        // Wait a moment so base.elapsed() > 0.
        thread::sleep(Duration::from_millis(5));

        sync.received(Duration::from_millis(0));
        {
            let state = sync.inner.state.lock().expect("poisoned");
            assert!(state.reference.is_some());
            let first_ref = state.reference.unwrap();
            assert!(first_ref > 0, "reference should be positive for pts=0");
            drop(state);
        }

        // A later frame at a worse offset leaves the reference alone.
        thread::sleep(Duration::from_millis(10));
        let ref_before = sync.inner.state.lock().expect("poisoned").reference;
        sync.received(Duration::from_millis(0));
        let ref_after = sync.inner.state.lock().expect("poisoned").reference;
        assert_eq!(ref_before, ref_after, "reference should not increase");
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_no_reference() {
        let sync = PlayoutClock::new(Duration::from_millis(100));
        assert!(sync.wait_async(Duration::from_millis(0)).await);
    }

    #[tokio::test]
    async fn wait_returns_false_when_closed() {
        let sync = PlayoutClock::new(Duration::from_millis(100));
        sync.received(Duration::from_millis(0));
        sync.close();
        assert!(!sync.wait_async(Duration::from_millis(0)).await);
    }

    #[test]
    fn latency_computation() {
        let sync = PlayoutClock::new(Duration::from_millis(50));
        assert_eq!(sync.latency(), Duration::from_millis(50));

        let audio = sync.register_audio();
        audio.set(Duration::from_millis(60));
        assert_eq!(sync.latency(), Duration::from_millis(110));

        drop(audio);
        assert_eq!(sync.latency(), Duration::from_millis(50));
    }

    /// A new route starts the reference over for timestamps back at zero.
    #[test]
    fn a_new_route_starts_the_timeline_over() {
        let sync = PlayoutClock::new(Duration::from_millis(50));
        sync.received(Duration::from_secs(2));
        sync.restart();
        sync.received(Duration::ZERO);
        assert!(
            matches!(sync.delay(Duration::ZERO), Delay::After(_)),
            "the new timeline's first frame is not held for the jitter allowance"
        );
    }

    /// A dropped audio registration no longer holds video back.
    #[test]
    fn a_dropped_audio_registration_stops_holding_video_back() {
        let sync = PlayoutClock::new(Duration::from_millis(50));
        {
            let audio = sync.register_audio();
            audio.set(Duration::from_millis(400));
            assert_eq!(sync.latency(), Duration::from_millis(450));
            // Dropped at scope end, as on every exit of an audio task.
        }
        assert_eq!(sync.latency(), Duration::from_millis(50));
    }

    #[tokio::test]
    async fn wait_holds_a_frame_for_the_latency() {
        let sync = PlayoutClock::new(Duration::from_millis(50));
        sync.received(Duration::from_millis(0));

        // Right after `received`, the reference is about now, so the wait is
        // about the latency.
        let start = Instant::now();
        assert!(sync.wait_async(Duration::from_millis(0)).await);
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(20),
            "expected about 50ms, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "expected about 50ms, got {elapsed:?}"
        );
    }

    /// A reference update interrupts a wait for a frame that became due early.
    ///
    /// The 2 s jitter makes a missed wake obvious. The bound is 1 s so that a
    /// loaded machine that schedules the waiter late does not fail the test.
    #[tokio::test]
    async fn wait_wakes_on_reference_update() {
        let sync = PlayoutClock::new(Duration::from_millis(2000));
        sync.received(Duration::from_millis(0));

        let waiter = sync.clone();
        let handle = tokio::spawn(async move {
            let start = Instant::now();
            waiter.wait_async(Duration::from_millis(0)).await;
            start.elapsed()
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        // Push the reference back far enough that the frame is due now.
        sync.received(Duration::from_millis(999_999));

        let elapsed = handle.await.unwrap();
        assert!(
            elapsed < Duration::from_secs(1),
            "expected an early wake, well under the 2s jitter, got {elapsed:?}"
        );
    }

    /// Closing interrupts a wait, so shutdown skips the playout latency.
    #[tokio::test]
    async fn wait_wakes_on_close() {
        let sync = PlayoutClock::new(Duration::from_millis(2000));
        sync.received(Duration::from_millis(0));

        let waiter = sync.clone();
        let handle = tokio::spawn(async move { waiter.wait_async(Duration::from_millis(0)).await });

        tokio::time::sleep(Duration::from_millis(200)).await;
        sync.close();

        assert!(
            !tokio::time::timeout(Duration::from_secs(1), handle)
                .await
                .expect("close should wake the waiter")
                .unwrap(),
            "a closed clock reports that the frame should not render",
        );
    }
}
