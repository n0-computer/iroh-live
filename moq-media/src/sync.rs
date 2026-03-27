//! Shared playout clock for A/V synchronization.
//!
//! Ported from `moq/js` at commit `53fe78d8`, file `js/watch/src/sync.ts`.
//! The data model and algorithm are kept identical to the JS source.
//!
//! ## How it works
//!
//! All internal arithmetic uses milliseconds as `i64`, matching the
//! JS `Time.Milli` branded type.
//!
//! - **`reference`**: The earliest `(wall_now_ms - frame_pts_ms)` ever
//!   observed. Captures the wall-clock offset for media time zero.
//!   Only moves earlier (smaller), never later — each frame that
//!   arrives faster than any previous one tightens the reference.
//!
//! - **`jitter`**: Minimum network jitter buffer (default 100 ms).
//!
//! - **`audio` / `video`**: Per-codec latency from the catalog's
//!   `jitter` field. Our Rust catalog does not carry this field yet,
//!   so both default to `None` (treated as 0). Setters are provided
//!   for forward compatibility.
//!
//! - **`latency`**: `max(audio, video) + jitter` — the total playout
//!   buffer target.
//!
//! A frame with timestamp `T` renders at wall time
//! `reference + T + latency`.
//!
//! ## Integration
//!
//! - **Video receive path**: call [`Sync::received`] when a packet
//!   arrives from the ordered consumer (before decode). Only the video
//!   path updates the reference — audio does not.
//!
//! - **Video render path**: call [`Sync::wait`] after decoding, before
//!   emitting the frame. Replaces the PTS-cadence `FramePacer`.
//!
//! - **Audio**: does *not* call `received` or `wait`. Audio uses its
//!   own ring-buffer/sink for pacing. The shared `latency` value is
//!   available for callers that want to align the audio buffer size.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

// ── Public API ──────────────────────────────────────────────────────────

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

#[derive(Debug)]
struct SyncInner {
    /// Wall-clock epoch set at construction. `base.elapsed()` gives us
    /// a monotonic millisecond counter equivalent to `performance.now()`
    /// in the JS source.
    base: Instant,

    state: Mutex<SyncState>,

    /// Wakes threads blocked in [`Sync::wait`] when `reference` or
    /// `latency` changes. Serves the same role as the JS
    /// `PromiseWithResolvers` that races against `setTimeout`.
    notify: Condvar,
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

    /// Per-codec latency from the audio catalog entry.
    /// Our Rust catalog does not yet carry this field, so it stays
    /// `None` (treated as 0) until the catalog is extended.
    audio_ms: Option<i64>,

    /// Per-codec latency from the video catalog entry. Same gap as
    /// `audio_ms`.
    video_ms: Option<i64>,

    /// Total latency: `max(audio, video) + jitter`. Recomputed eagerly
    /// by every setter (the JS source uses a reactive `Effect`; here
    /// we compute inline since setters are infrequent).
    latency_ms: i64,

    /// Set by [`Sync::close`]. Causes [`Sync::wait`] to return `false`
    /// immediately. No JS equivalent — JS relies on effect cleanup and
    /// GC.
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
                notify: Condvar::new(),
            }),
        }
    }

    // ── Reference updates (video receive path) ─────────────────────

    /// Records the arrival of a frame with the given PTS timestamp.
    ///
    /// Computes `ref = now_ms - pts_ms` and stores it as the new
    /// reference if it is strictly smaller (earlier) than the current
    /// one. Only the video receive path calls this.
    pub fn received(&self, timestamp: Duration) {
        let now_ms = self.now_ms();
        let timestamp_ms = timestamp.as_millis() as i64;
        let ref_val = now_ms - timestamp_ms;

        let mut state = self.inner.state.lock().unwrap();

        if state.reference.is_some_and(|current| ref_val >= current) {
            return;
        }

        state.reference = Some(ref_val);
        self.inner.notify.notify_all();
    }

    // ── Playout gating (video render path) ─────────────────────────

    /// Blocks until it is time to render the frame with the given PTS.
    ///
    /// Computes `sleep = (reference - (now - pts)) + latency`. If
    /// positive, sleeps for that duration. If the reference or latency
    /// changes while sleeping, the sleep is interrupted and the
    /// duration is recalculated.
    ///
    /// Returns `true` when the frame should be rendered, `false` if
    /// the sync was closed (caller should tear down the pipeline).
    ///
    /// Runs on the video decode OS thread. The JS equivalent is async,
    /// using `setTimeout` raced against a notification promise; here
    /// `Condvar::wait_timeout` serves the same role.
    ///
    /// If no reference has been set yet (no `received()` call), returns
    /// `true` immediately so the pipeline doesn't stall on startup.
    /// The JS source throws in this case, but our synchronous decode
    /// loop can reach `wait()` before the first `received()` in edge
    /// cases that the async JS pipeline avoids by construction.
    pub fn wait(&self, timestamp: Duration) -> bool {
        let timestamp_ms = timestamp.as_millis() as i64;

        let mut state = self.inner.state.lock().unwrap();

        // No reference yet — render immediately rather than stalling.
        if state.reference.is_none() {
            return true;
        }

        loop {
            if state.closed {
                return false;
            }

            let Some(current_ref) = state.reference else {
                return false;
            };

            let now_ms = self.now_ms();
            let ref_val = now_ms - timestamp_ms;
            let sleep_ms = (current_ref - ref_val) + state.latency_ms;

            if sleep_ms <= 0 {
                return true;
            }

            let sleep = Duration::from_millis(sleep_ms as u64);

            // wait_timeout: notified → recalculate, timed out → render.
            let (new_state, timeout_result) = self.inner.notify.wait_timeout(state, sleep).unwrap();
            state = new_state;

            if timeout_result.timed_out() {
                return true;
            }
        }
    }

    // ── Latency configuration ──────────────────────────────────────

    /// Returns the current total latency: `max(audio, video) + jitter`.
    pub fn latency(&self) -> Duration {
        let state = self.inner.state.lock().unwrap();
        Duration::from_millis(state.latency_ms.max(0) as u64)
    }

    /// Sets the network jitter buffer. Wakes any blocked `wait()` call
    /// so it can recalculate with the new latency.
    pub fn set_jitter(&self, jitter: Duration) {
        let mut state = self.inner.state.lock().unwrap();
        state.jitter_ms = jitter.as_millis() as i64;
        Self::recompute_latency(&mut state);
        self.inner.notify.notify_all();
    }

    /// Sets the audio codec latency (from the catalog `jitter` field).
    ///
    /// Our Rust catalog does not yet carry per-codec jitter values, so
    /// this setter is unused in practice. Provided for forward
    /// compatibility with the JS catalog schema.
    pub fn set_audio_latency(&self, latency: Option<Duration>) {
        let mut state = self.inner.state.lock().unwrap();
        state.audio_ms = latency.map(|d| d.as_millis() as i64);
        Self::recompute_latency(&mut state);
        self.inner.notify.notify_all();
    }

    /// Sets the video codec latency (from the catalog `jitter` field).
    ///
    /// Same forward-compatibility note as [`set_audio_latency`](Self::set_audio_latency).
    pub fn set_video_latency(&self, latency: Option<Duration>) {
        let mut state = self.inner.state.lock().unwrap();
        state.video_ms = latency.map(|d| d.as_millis() as i64);
        Self::recompute_latency(&mut state);
        self.inner.notify.notify_all();
    }

    /// Wakes all threads blocked in [`wait`](Self::wait) and causes
    /// future `wait` calls to return `false` immediately. No JS
    /// equivalent — Rust needs explicit cleanup since OS threads are
    /// not cancelled automatically.
    pub fn close(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.closed = true;
        self.inner.notify.notify_all();
    }

    // ── Internal helpers ───────────────────────────────────────────

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
            let state = sync.inner.state.lock().unwrap();
            assert!(state.reference.is_some());
            let first_ref = state.reference.unwrap();
            assert!(first_ref > 0, "reference should be positive for pts=0");
            drop(state);
        }

        // A later frame arriving at a worse offset should not update
        // the reference (it stays at the earlier/smaller value).
        thread::sleep(Duration::from_millis(10));
        let ref_before = sync.inner.state.lock().unwrap().reference;
        sync.received(Duration::from_millis(0));
        let ref_after = sync.inner.state.lock().unwrap().reference;
        assert_eq!(ref_before, ref_after, "reference should not increase");
    }

    #[test]
    fn wait_returns_immediately_when_no_reference() {
        let sync = Sync::new();
        assert!(sync.wait(Duration::from_millis(0)));
    }

    #[test]
    fn wait_returns_false_when_closed() {
        let sync = Sync::new();
        sync.received(Duration::from_millis(0));
        sync.close();
        assert!(!sync.wait(Duration::from_millis(0)));
    }

    #[test]
    fn latency_computation() {
        let sync = Sync::with_jitter(Duration::from_millis(50));
        assert_eq!(sync.latency(), Duration::from_millis(50));

        sync.set_video_latency(Some(Duration::from_millis(30)));
        assert_eq!(sync.latency(), Duration::from_millis(80));

        sync.set_audio_latency(Some(Duration::from_millis(60)));
        assert_eq!(sync.latency(), Duration::from_millis(110));

        sync.set_audio_latency(None);
        assert_eq!(sync.latency(), Duration::from_millis(80));
    }

    #[test]
    fn wait_blocks_approximately_latency_duration() {
        let sync = Sync::with_jitter(Duration::from_millis(50));
        sync.received(Duration::from_millis(0));

        // Right after received(), reference ≈ now, so
        // sleep ≈ (ref - now + 0) + latency ≈ latency.
        let start = Instant::now();
        assert!(sync.wait(Duration::from_millis(0)));
        let elapsed = start.elapsed();

        // Lower bound: should sleep at least most of the 50ms jitter.
        // Upper bound: generous for shared CI VMs (macOS runners can
        // stall threads for tens of ms under load).
        assert!(
            elapsed >= Duration::from_millis(20),
            "expected ~50ms sleep, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "expected ~50ms sleep, got {elapsed:?}"
        );
    }

    /// Verifies that updating the reference wakes a thread blocked in
    /// `wait()`, causing it to return well before the jitter timeout.
    ///
    /// The test uses a large jitter (2s) so the "without wake" case is
    /// unmistakable, and a generous startup sleep (200ms) to avoid the
    /// race where `notify_all` fires before the spawned thread enters
    /// `Condvar::wait_timeout`. On a shared CI VM the thread might not
    /// be scheduled promptly, so we assert < 1s rather than < 100ms —
    /// still far below the 2s jitter, proving the wake happened.
    ///
    /// In production, missed wakes are harmless: `received()` is called
    /// on every packet (~30 fps), so the Condvar is re-notified within
    /// one frame interval. The `wait_timeout` also caps the maximum
    /// delay to `latency_ms`.
    #[test]
    fn wait_wakes_on_reference_update() {
        let sync = Sync::with_jitter(Duration::from_millis(2000));
        sync.received(Duration::from_millis(0));

        let sync2 = sync.clone();
        let handle = thread::spawn(move || {
            let start = Instant::now();
            sync2.wait(Duration::from_millis(0));
            start.elapsed()
        });

        // Give the spawned thread ample time to enter wait_timeout.
        thread::sleep(Duration::from_millis(200));
        // Push the reference so far back that sleep_ms ≤ 0, causing
        // the woken thread to return immediately.
        sync.received(Duration::from_millis(999_999));

        let elapsed = handle.join().unwrap();
        assert!(
            elapsed < Duration::from_secs(1),
            "expected early wake (well under 2s jitter), got {elapsed:?}"
        );
    }
}
