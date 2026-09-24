//! A player's playout clock, which keeps its audio and video aligned.
//!
//! Ported from `moq/js` at commit `53fe78d8`, `js/watch/src/sync.ts`, and the
//! arithmetic is kept identical to the JS source: milliseconds as `i64`, so
//! there is no rounding to reason about when comparing the two.
//!
//! Neither `moq-video` nor `moq-audio` has a counterpart, which is why this is
//! here. Two independent decode paths would otherwise drift apart, because
//! nothing else knows what the other one is holding. Each player owns one, so
//! two players of one broadcast never hold each other's frames back.
//!
//! ## The model
//!
//! - **`reference`** is the earliest `wall_now - frame_pts` ever seen. It only
//!   ever moves earlier: a frame that arrives faster than every previous one
//!   tightens it, and nothing loosens it. That is what makes it an estimate of
//!   wall time at media time zero rather than a running average.
//! - **`jitter`** is the network jitter allowance, 100 ms by default.
//! - **`audio`** is how much audio is queued at the speaker, reported by the
//!   audio path on every decoded frame through its [`AudioLatency`] guard.
//! - **`latency`** is `audio + jitter`.
//!
//! A frame stamped `T` is due at `reference + T + latency`.
//!
//! ## How the two paths use it
//!
//! The video path calls [`PlayoutClock::received`] as each frame is decoded
//! and [`PlayoutClock::wait_async`] before handing it to the renderer. Only video moves the
//! reference; audio is paced by its own device.
//!
//! The audio path reports its buffer depth, which is the only latency either
//! side can actually measure, and video holds frames back by it. That coupling
//! is the whole point: without it a video frame renders as soon as it is
//! decoded while its audio is still queued behind 50 ms of sound.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

// --- Public API ------------------------------------------------------

/// How long a frame still has to wait before it is due.
///
/// Returned by [`PlayoutClock::delay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delay {
    /// The frame is due now.
    Now,
    /// The frame is due after this long.
    After(Duration),
    /// The clock was closed; tear the pipeline down.
    Closed,
}

/// Shared playout clock for A/V synchronization.
///
/// Cheaply cloneable (wraps an `Arc`). One per player, shared between its
/// video and audio decode paths.
///
/// Ported from `moq/js` commit `53fe78d8`, `js/watch/src/sync.ts`.
#[derive(Clone, Debug)]
pub(crate) struct PlayoutClock {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Wall-clock epoch set at construction. `base.elapsed()` gives us
    /// a monotonic millisecond counter equivalent to `performance.now()`
    /// in the JS source.
    base: Instant,

    state: Mutex<State>,

    /// Wakes a [`PlayoutClock::wait_async`] when the reference, the latency, or the
    /// closed flag moves. Serves the same role as the JS
    /// `PromiseWithResolvers` racing against a `setTimeout`.
    changed: tokio::sync::Notify,
}

/// Mutable state behind the lock. All durations stored as `i64`
/// milliseconds to match the JS arithmetic exactly (signed, no
/// saturation, no precision loss from `Duration` rounding).
#[derive(Debug)]
struct State {
    /// Earliest `(now_ms - pts_ms)` observed on the current timeline. `None`
    /// until the first call to [`PlayoutClock::received`].
    reference: Option<i64>,

    /// Network jitter buffer in ms (default 100).
    jitter_ms: i64,

    /// How much audio is queued ahead of the speaker, in ms, as the audio
    /// path last reported it. Video is held back by this so the two land
    /// together.
    audio_ms: Option<i64>,

    /// Set by [`PlayoutClock::close`], which makes every wait return immediately.
    closed: bool,
}

impl State {
    /// Returns the total latency, `audio + jitter`, in ms.
    fn latency_ms(&self) -> i64 {
        self.audio_ms.unwrap_or(0) + self.jitter_ms
    }
}

impl PlayoutClock {
    /// Creates a new playout clock with a custom jitter buffer.
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

    // --- Reference updates (video receive path) ----------------------

    /// Records the arrival of a frame with the given PTS timestamp.
    ///
    /// Computes `ref = now_ms - pts_ms` and stores it as the new
    /// reference if it is strictly smaller (earlier) than the current
    /// one. Only the video receive path calls this.
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

    /// Starts the reference over, as the broadcast came back on a new route.
    ///
    /// A publisher behind the new route may have restarted, and its clock with
    /// it: held against the old reference, every later frame would be overdue
    /// and nothing paced.
    pub(crate) fn restart(&self) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.reference = None;
        self.inner.changed.notify_waiters();
    }

    // --- Playout gating (video render path) --------------------------

    /// Waits until it is time to render the frame with the given PTS.
    ///
    /// Recomputes the delay whenever the clock moves under the wait, so a
    /// reference that tightened while we slept still holds the frame back.
    ///
    /// Returns `true` when the frame should be rendered, and `false` if the
    /// clock was closed.
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
                    // Whichever comes first: the frame is due, or the clock
                    // moved under us. A shutdown lands on the second, so it does
                    // not have to wait out the playout latency.
                    tokio::select! {
                        _ = tokio::time::sleep(sleep) => return true,
                        _ = changed => continue,
                    }
                }
            }
        }
    }

    /// How long the frame with the given PTS still has to wait.
    ///
    /// The arithmetic behind [`wait_async`](Self::wait_async), exposed so a caller
    /// can drive its own timer.
    pub(crate) fn delay(&self, timestamp: Duration) -> Delay {
        let timestamp_ms = timestamp.as_millis() as i64;
        let state = self.inner.state.lock().expect("poisoned");

        if state.closed {
            return Delay::Closed;
        }
        // No reference yet: render immediately rather than stalling.
        let Some(current_ref) = state.reference else {
            return Delay::Now;
        };

        let sleep_ms = (current_ref - (self.now_ms() - timestamp_ms)) + state.latency_ms();
        match sleep_ms > 0 {
            true => Delay::After(Duration::from_millis(sleep_ms as u64)),
            false => Delay::Now,
        }
    }

    // --- Latency configuration ---------------------------------------

    /// Returns the current total latency: `audio + jitter`.
    pub(crate) fn latency(&self) -> Duration {
        let state = self.inner.state.lock().expect("poisoned");
        Duration::from_millis(state.latency_ms().max(0) as u64)
    }

    /// Sets the network jitter buffer. Wakes any blocked `wait()` call
    /// so it can recalculate with the new latency.
    pub(crate) fn set_jitter(&self, jitter: Duration) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.jitter_ms = jitter.as_millis() as i64;
        self.inner.changed.notify_waiters();
    }

    /// Sets how much audio is queued ahead of the speaker.
    ///
    /// Written through an [`AudioLatency`] guard, so the value cannot outlive
    /// the audio path that reported it.
    fn set_audio_buffered(&self, latency: Option<Duration>) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.audio_ms = latency.map(|d| d.as_millis() as i64);
        self.inner.changed.notify_waiters();
    }

    /// Registers an audio path's contribution to the latency.
    ///
    /// The audio path reports how much it has buffered through the returned
    /// guard, and dropping the guard clears the contribution. That is the one
    /// way the audio term can be set, so an audio track that stops, whether it
    /// ended, failed or was dropped, stops holding video back with it.
    pub(crate) fn register_audio(&self) -> AudioLatency {
        AudioLatency {
            clock: self.clone(),
        }
    }

    /// Closes the clock, so every wait returns at once and later ones return
    /// immediately.
    ///
    /// The JS source has no counterpart: it leans on effect cleanup, where a
    /// Rust pipeline has to be told to stop.
    pub(crate) fn close(&self) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.closed = true;
        self.inner.changed.notify_waiters();
    }

    // --- Internal helpers --------------------------------------------

    /// Milliseconds elapsed since construction, equivalent to the JS
    /// `performance.now()` call.
    fn now_ms(&self) -> i64 {
        self.inner.base.elapsed().as_millis() as i64
    }
}

/// An audio path's registration with a playout clock.
///
/// From [`PlayoutClock::register_audio`]. Reports how much audio is queued ahead of the
/// speaker, which is the only latency either side can actually measure, and
/// video is held back by it so the two land together. Dropping it clears the
/// report on every exit of the audio path, so a stopped track stops holding
/// video back.
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

        // First frame: reference is set.
        sync.received(Duration::from_millis(0));
        {
            let state = sync.inner.state.lock().expect("poisoned");
            assert!(state.reference.is_some());
            let first_ref = state.reference.unwrap();
            assert!(first_ref > 0, "reference should be positive for pts=0");
            drop(state);
        }

        // A later frame arriving at a worse offset should not update
        // the reference (it stays at the earlier/smaller value).
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

    /// S8: a publisher that restarts starts its timestamps at zero again; the
    /// new route it comes back on starts the reference over.
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

    /// Regression (R12): the audio path set its buffer depth on every frame and
    /// never cleared it, so a stopped audio track held video back by its last
    /// reading for the rest of the playback.
    #[test]
    fn a_dropped_audio_registration_stops_holding_video_back() {
        let sync = PlayoutClock::new(Duration::from_millis(50));
        {
            let audio = sync.register_audio();
            audio.set(Duration::from_millis(400));
            assert_eq!(sync.latency(), Duration::from_millis(450));
            // Leaves the scope the way an audio task leaves its loop: on an
            // error, an end of track, or an abort, all of which drop it.
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

    /// A reference update has to interrupt a wait, or a frame that became due
    /// early still waits out the old estimate.
    ///
    /// The jitter is 2s so the un-woken case is unmistakable, and the assertion
    /// is under 1s rather than under 100ms because a loaded machine may not
    /// schedule the waiter promptly. Either way it is far below 2s.
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

    /// Closing has to interrupt a wait too: a shutdown should not sit through
    /// the playout latency before the decode task notices.
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
