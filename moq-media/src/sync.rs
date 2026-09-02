//! The shared playout clock that keeps audio and video aligned.
//!
//! Ported from `moq/js` at commit `53fe78d8`, `js/watch/src/sync.ts`, and the
//! arithmetic is kept identical to the JS source: milliseconds as `i64`, so
//! there is no rounding to reason about when comparing the two.
//!
//! Neither `moq-video` nor `moq-audio` has a counterpart, which is why this is
//! here. Two independent decode paths would otherwise drift apart, because
//! nothing else knows what the other one is holding.
//!
//! ## The model
//!
//! - **`reference`** is the earliest `wall_now - frame_pts` ever seen. It only
//!   ever moves earlier: a frame that arrives faster than every previous one
//!   tightens it, and nothing loosens it. That is what makes it an estimate of
//!   wall time at media time zero rather than a running average.
//! - **`jitter`** is the network jitter allowance, 100 ms by default.
//! - **`audio`** is how much audio is queued at the speaker, reported by the
//!   audio path on every decoded frame (see [`Sync::set_audio_buffered`]).
//! - **`video`** is the video path's own decode latency, if a caller sets one.
//! - **`latency`** is `max(audio, video) + jitter`.
//!
//! A frame stamped `T` is due at `reference + T + latency`.
//!
//! ## How the two paths use it
//!
//! The video path calls [`Sync::received`] as each frame is decoded and
//! [`Sync::wait_async`] before handing it to the renderer. Only video moves the
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
/// Returned by [`Sync::delay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delay {
    /// The frame is due now.
    Now,
    /// The frame is due after this long.
    After(Duration),
    /// The clock was closed; tear the pipeline down.
    Closed,
}

/// Shared playout clock for A/V synchronization.
///
/// Cheaply cloneable (wraps an `Arc`). Create one per
/// [`RemoteBroadcast`](crate::subscribe::RemoteBroadcast) and share it
/// between the video and audio decode pipelines.
///
/// Ported from `moq/js` commit `53fe78d8`, `js/watch/src/sync.ts`.
#[derive(Clone, Debug)]
pub struct Sync {
    inner: Arc<SyncInner>,
}

/// Closes the clock when the last [`Sync`] handle goes, so a
/// [`Sync::wait_async`] holding one of those handles cannot outlive the
/// pipeline it was pacing.
impl Drop for SyncInner {
    fn drop(&mut self) {
        self.state.get_mut().expect("poisoned").closed = true;
        self.changed.notify_waiters();
    }
}

#[derive(Debug)]
struct SyncInner {
    /// Wall-clock epoch set at construction. `base.elapsed()` gives us
    /// a monotonic millisecond counter equivalent to `performance.now()`
    /// in the JS source.
    base: Instant,

    state: Mutex<SyncState>,

    /// Wakes a [`Sync::wait_async`] when the reference, the latency, or the
    /// closed flag moves. Serves the same role as the JS
    /// `PromiseWithResolvers` racing against a `setTimeout`.
    changed: tokio::sync::Notify,
}

/// Mutable state behind the lock. All durations stored as `i64`
/// milliseconds to match the JS arithmetic exactly (signed, no
/// saturation, no precision loss from `Duration` rounding).
#[derive(Debug)]
struct SyncState {
    /// Earliest `(now_ms - pts_ms)` ever observed. `None` until the
    /// first call to [`Sync::received`].
    reference: Option<i64>,

    /// Network jitter buffer in ms (default 100).
    jitter_ms: i64,

    /// How much audio is queued ahead of the speaker, in ms, as the audio
    /// path last reported it. Video is held back by this so the two land
    /// together.
    audio_ms: Option<i64>,

    /// The video path's own decode latency, if a caller measured one. Nothing
    /// in this crate does, so it is unset in practice.
    video_ms: Option<i64>,

    /// Total latency: `max(audio, video) + jitter`. Recomputed eagerly
    /// by every setter (the JS source uses a reactive `Effect`; here
    /// we compute inline since setters are infrequent).
    latency_ms: i64,

    /// Set by [`Sync::close`], which makes every wait return immediately.
    closed: bool,
}

impl Sync {
    /// Creates a new playout clock with the default 100 ms jitter buffer.
    pub fn new() -> Self {
        Self::with_jitter(Duration::from_millis(100))
    }

    /// Creates a new playout clock with a custom jitter buffer.
    pub fn with_jitter(jitter: Duration) -> Self {
        let jitter_ms = jitter.as_millis() as i64;
        Self {
            inner: Arc::new(SyncInner {
                base: Instant::now(),
                state: Mutex::new(SyncState {
                    reference: None,
                    jitter_ms,
                    audio_ms: None,
                    video_ms: None,
                    latency_ms: jitter_ms,
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
    pub fn received(&self, timestamp: Duration) {
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

    // --- Playout gating (video render path) --------------------------

    /// Waits until it is time to render the frame with the given PTS.
    ///
    /// Recomputes the delay whenever the clock moves under the wait, so a
    /// reference that tightened while we slept still holds the frame back.
    ///
    /// Returns `true` when the frame should be rendered, and `false` if the
    /// clock was closed.
    pub async fn wait_async(&self, timestamp: Duration) -> bool {
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
    pub fn delay(&self, timestamp: Duration) -> Delay {
        let timestamp_ms = timestamp.as_millis() as i64;
        let state = self.inner.state.lock().expect("poisoned");

        if state.closed {
            return Delay::Closed;
        }
        // No reference yet: render immediately rather than stalling.
        let Some(current_ref) = state.reference else {
            return Delay::Now;
        };

        let sleep_ms = (current_ref - (self.now_ms() - timestamp_ms)) + state.latency_ms;
        match sleep_ms > 0 {
            true => Delay::After(Duration::from_millis(sleep_ms as u64)),
            false => Delay::Now,
        }
    }

    // --- Latency configuration ---------------------------------------

    /// Returns the current total latency: `max(audio, video) + jitter`.
    pub fn latency(&self) -> Duration {
        let state = self.inner.state.lock().expect("poisoned");
        Duration::from_millis(state.latency_ms.max(0) as u64)
    }

    /// Sets the network jitter buffer. Wakes any blocked `wait()` call
    /// so it can recalculate with the new latency.
    pub fn set_jitter(&self, jitter: Duration) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.jitter_ms = jitter.as_millis() as i64;
        Self::recompute_latency(&mut state);
        self.inner.changed.notify_waiters();
    }

    /// Sets how much audio is queued ahead of the speaker.
    ///
    /// Called by the audio decode path on every frame it writes to its sink.
    /// This is the only latency either side can actually measure, and video is
    /// held back by it so the two land together.
    pub fn set_audio_buffered(&self, latency: Option<Duration>) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.audio_ms = latency.map(|d| d.as_millis() as i64);
        Self::recompute_latency(&mut state);
        self.inner.changed.notify_waiters();
    }

    /// Sets the video path's own decode latency.
    ///
    /// The counterpart of [`set_audio_buffered`](Self::set_audio_buffered) for
    /// a caller that can measure how long its decoder holds a frame. Nothing
    /// in this crate measures one, so the video term stays zero unless a caller
    /// supplies it.
    pub fn set_video_latency(&self, latency: Option<Duration>) {
        let mut state = self.inner.state.lock().expect("poisoned");
        state.video_ms = latency.map(|d| d.as_millis() as i64);
        Self::recompute_latency(&mut state);
        self.inner.changed.notify_waiters();
    }

    /// Closes the clock, so every wait returns at once and later ones return
    /// immediately.
    ///
    /// The JS source has no counterpart: it leans on effect cleanup, where a
    /// Rust pipeline has to be told to stop.
    pub fn close(&self) {
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

    /// Recomputes `latency = max(audio, video) + jitter`.
    fn recompute_latency(state: &mut SyncState) {
        let video = state.video_ms.unwrap_or(0);
        let audio = state.audio_ms.unwrap_or(0);
        state.latency_ms = video.max(audio) + state.jitter_ms;
    }
}

impl Default for Sync {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn received_tracks_minimum_reference() {
        let sync = Sync::new();

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
        let sync = Sync::new();
        assert!(sync.wait_async(Duration::from_millis(0)).await);
    }

    #[tokio::test]
    async fn wait_returns_false_when_closed() {
        let sync = Sync::new();
        sync.received(Duration::from_millis(0));
        sync.close();
        assert!(!sync.wait_async(Duration::from_millis(0)).await);
    }

    #[test]
    fn latency_computation() {
        let sync = Sync::with_jitter(Duration::from_millis(50));
        assert_eq!(sync.latency(), Duration::from_millis(50));

        sync.set_video_latency(Some(Duration::from_millis(30)));
        assert_eq!(sync.latency(), Duration::from_millis(80));

        sync.set_audio_buffered(Some(Duration::from_millis(60)));
        assert_eq!(sync.latency(), Duration::from_millis(110));

        sync.set_audio_buffered(None);
        assert_eq!(sync.latency(), Duration::from_millis(80));
    }

    #[tokio::test]
    async fn wait_holds_a_frame_for_the_latency() {
        let sync = Sync::with_jitter(Duration::from_millis(50));
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
        let sync = Sync::with_jitter(Duration::from_millis(2000));
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
        let sync = Sync::with_jitter(Duration::from_millis(2000));
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
