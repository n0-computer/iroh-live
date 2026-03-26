//! Typed metrics infrastructure for debug overlays.
//!
//! Each observable value is a [`Metric`] (EMA-smoothed current value +
//! ring buffer history for sparklines) or a [`Label`] (atomic string).
//! These are grouped into typed structs ([`NetStats`], [`EncodeStats`],
//! [`RenderStats`], [`TimingStats`]) that the pipelines write to and
//! the overlay reads from. No string keys, no registration.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// ── Metric ──────────────────────────────────────────────────────────

/// Static metadata for display and thresholds.
#[derive(Debug, Clone, Copy)]
pub struct MetricMeta {
    pub label: &'static str,
    pub unit: &'static str,
    pub alpha: f64,
    pub history_cap: usize,
    /// Color thresholds. `None` = always white.
    pub thresholds: Option<Thresholds>,
}

/// Color thresholds for a metric value.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// Below this = good (green). Between good and warn = yellow.
    pub good: f64,
    /// Above this = bad (red).
    pub warn: f64,
    /// If true, higher is better (e.g. FPS). Inverts the comparison.
    pub inverted: bool,
}

impl MetricMeta {
    pub const fn smooth(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            alpha: 0.1,
            history_cap: 300,
            thresholds: None,
        }
    }
    pub const fn responsive(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            alpha: 0.3,
            history_cap: 300,
            thresholds: None,
        }
    }
    pub const fn with_thresholds(mut self, good: f64, warn: f64, inverted: bool) -> Self {
        self.thresholds = Some(Thresholds {
            good,
            warn,
            inverted,
        });
        self
    }
}

/// A single observable numeric metric with EMA smoothing and history.
#[derive(Clone)]
pub struct Metric {
    inner: Arc<MetricInner>,
}

struct MetricInner {
    current: AtomicU64,
    sample_count: AtomicU64,
    meta: MetricMeta,
    history: Mutex<VecDeque<(Instant, f64)>>,
}

impl std::fmt::Debug for Metric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metric")
            .field("current", &self.current())
            .field("label", &self.meta().label)
            .finish()
    }
}

impl Metric {
    pub fn new(meta: MetricMeta) -> Self {
        Self {
            inner: Arc::new(MetricInner {
                current: AtomicU64::new(0f64.to_bits()),
                sample_count: AtomicU64::new(0),
                history: Mutex::new(VecDeque::with_capacity(meta.history_cap)),
                meta,
            }),
        }
    }

    /// Records a sample. Briefly locks history ring buffer.
    ///
    /// The current-value EMA update is not fully atomic across concurrent
    /// writers. In practice each Metric has a single writer (one pipeline
    /// thread), so this is benign.
    pub fn record(&self, value: f64) {
        let count = self.inner.sample_count.fetch_add(1, Ordering::Relaxed);
        let smoothed = if count == 0 {
            value
        } else {
            let prev = f64::from_bits(self.inner.current.load(Ordering::Relaxed));
            let a = self.inner.meta.alpha;
            a * value + (1.0 - a) * prev
        };
        self.inner
            .current
            .store(smoothed.to_bits(), Ordering::Relaxed);

        let mut hist = self.inner.history.lock().expect("metric history lock");
        if hist.len() >= self.inner.meta.history_cap {
            hist.pop_front();
        }
        hist.push_back((Instant::now(), value));
    }

    /// Returns the EMA-smoothed current value.
    pub fn current(&self) -> f64 {
        f64::from_bits(self.inner.current.load(Ordering::Relaxed))
    }

    /// Copies history into `out`, clearing it first. Reuses the Vec allocation.
    pub fn history_into(&self, out: &mut Vec<(Instant, f64)>) {
        out.clear();
        let hist = self.inner.history.lock().expect("history lock");
        out.extend(hist.iter().copied());
    }

    /// Returns a copy of the history ring buffer.
    pub fn history(&self) -> Vec<(Instant, f64)> {
        let mut v = Vec::new();
        self.history_into(&mut v);
        v
    }

    pub fn meta(&self) -> &MetricMeta {
        &self.inner.meta
    }

    pub fn has_samples(&self) -> bool {
        self.inner.sample_count.load(Ordering::Relaxed) > 0
    }

    /// Records a [`Duration`] as milliseconds.
    pub fn record_ms(&self, d: Duration) {
        self.record(d.as_secs_f64() * 1000.0);
    }

    /// Records an FPS sample from the gap since the previous event. Ignores
    /// gaps shorter than 5ms to avoid division noise.
    pub fn record_fps_gap(&self, gap: Duration) {
        if gap >= Duration::from_millis(5) {
            self.record(1.0 / gap.as_secs_f64());
        }
    }
}

/// Tracks wall-clock drift from PTS cadence to produce a lag metric.
///
/// On first call, records the base wall+PTS. On subsequent calls, returns
/// `wall_elapsed - pts_elapsed` in milliseconds (positive = behind live).
#[derive(Debug, Default)]
pub struct LagTracker {
    base_wall: Option<Instant>,
    base_pts: Option<Duration>,
}

impl LagTracker {
    pub fn new() -> Self {
        Self {
            base_wall: None,
            base_pts: None,
        }
    }

    /// Records one observation and returns the signed lag in milliseconds.
    ///
    /// Positive values mean wall clock is behind PTS (rendering late).
    /// Negative values mean wall clock is ahead of PTS (rendering early).
    pub fn record(&mut self, wall: Instant, pts: Duration) -> f64 {
        let base_w = *self.base_wall.get_or_insert(wall);
        let base_p = *self.base_pts.get_or_insert(pts);
        let wall_elapsed = wall.duration_since(base_w);
        let pts_elapsed = pts.saturating_sub(base_p);
        wall_elapsed.as_secs_f64() * 1000.0 - pts_elapsed.as_secs_f64() * 1000.0
    }

    /// Alias for [`record`](Self::record).
    pub fn record_ms(&mut self, wall: Instant, pts: Duration) -> f64 {
        self.record(wall, pts)
    }
}

// ── Label ───────────────────────────────────────────────────────────

/// An observable string label (e.g. codec name, path type).
#[derive(Clone, Debug)]
pub struct Label {
    inner: Arc<Mutex<String>>,
}

impl Label {
    pub fn new(initial: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(initial.into())),
        }
    }

    pub fn set(&self, value: impl Into<String>) {
        *self.inner.lock().expect("label lock") = value.into();
    }

    pub fn get(&self) -> String {
        self.inner.lock().expect("label lock").clone()
    }
}

impl Default for Label {
    fn default() -> Self {
        Self::new("")
    }
}

// ── Stat category structs ───────────────────────────────────────────

/// Network stats. Written by the transport bridge (iroh-live or
/// web_transport_trait), read by the overlay.
#[derive(Clone, Debug)]
pub struct NetStats {
    pub rtt_ms: Metric,
    pub loss_pct: Metric,
    pub bw_down_mbps: Metric,
    pub bw_up_mbps: Metric,
    pub paths_active: Metric,
    pub path_type: Label,
    pub path_addr: Label,
    pub peer: Label,
}

impl Default for NetStats {
    fn default() -> Self {
        Self {
            rtt_ms: Metric::new(
                MetricMeta::smooth("RTT", "ms").with_thresholds(30.0, 100.0, false),
            ),
            loss_pct: Metric::new(
                MetricMeta::smooth("Loss", "%").with_thresholds(2.0, 10.0, false),
            ),
            bw_down_mbps: Metric::new(MetricMeta::smooth("Down", "Mbps")),
            bw_up_mbps: Metric::new(MetricMeta::smooth("Up", "Mbps")),
            paths_active: Metric::new(MetricMeta::responsive("Paths", "")),
            path_type: Label::default(),
            path_addr: Label::default(),
            peer: Label::default(),
        }
    }
}

/// Publish-side encode stats. Written by the encode pipeline.
#[derive(Clone, Debug)]
pub struct EncodeStats {
    pub fps: Metric,
    pub encode_ms: Metric,
    pub bitrate_kbps: Metric,
    pub codec: Label,
    pub encoder: Label,
    pub resolution: Label,
    /// Capture-to-encode path, e.g. "pw-screen/dmabuf" or "pw-screen/shm".
    pub capture_path: Label,
}

impl Default for EncodeStats {
    fn default() -> Self {
        Self {
            fps: Metric::new(MetricMeta::responsive("FPS", "").with_thresholds(25.0, 15.0, true)),
            encode_ms: Metric::new(MetricMeta::responsive("Encode", "ms")),
            bitrate_kbps: Metric::new(MetricMeta::smooth("Bitrate", "kbps")),
            codec: Label::default(),
            encoder: Label::default(),
            resolution: Label::default(),
            capture_path: Label::default(),
        }
    }
}

/// Render/decode stats. Written by the decode pipeline.
#[derive(Clone, Debug)]
pub struct RenderStats {
    pub fps: Metric,
    pub decode_ms: Metric,
    pub decoder: Label,
    pub renderer: Label,
    pub rendition: Label,
}

impl Default for RenderStats {
    fn default() -> Self {
        Self {
            fps: Metric::new(MetricMeta::responsive("FPS", "").with_thresholds(25.0, 15.0, true)),
            decode_ms: Metric::new(MetricMeta::responsive("Decode", "ms")),
            decoder: Label::default(),
            renderer: Label::default(),
            rendition: Label::default(),
        }
    }
}

/// Timing/playout stats. Written by the decode pipelines.
#[derive(Clone, Debug)]
pub struct TimingStats {
    /// Audio output ring buffer fill level.
    pub audio_buf_ms: Metric,
    /// Video playout lag: wall drift from PTS cadence (positive = behind live).
    pub video_lag_ms: Metric,
    /// Audio playout lag: wall drift from PTS cadence.
    pub audio_lag_ms: Metric,
    /// A/V delta: `video_lag - audio_lag`. Positive = video behind audio.
    pub av_delta_ms: Metric,
    /// Decoded video frames waiting in the playout buffer.
    pub video_buf: Metric,
}

impl Default for TimingStats {
    fn default() -> Self {
        Self {
            audio_buf_ms: Metric::new(
                MetricMeta::responsive("AudioBuf", "ms").with_thresholds(80.0, 200.0, false),
            ),
            video_lag_ms: Metric::new(
                MetricMeta::responsive("VideoLag", "ms").with_thresholds(50.0, 150.0, false),
            ),
            audio_lag_ms: Metric::new(
                MetricMeta::responsive("AudioLag", "ms").with_thresholds(50.0, 150.0, false),
            ),
            av_delta_ms: Metric::new(
                MetricMeta::responsive("A/V Δ", "ms").with_thresholds(20.0, 50.0, false),
            ),
            video_buf: Metric::new(MetricMeta::responsive("VideoBuf", "")),
        }
    }
}

// ── Timeline ────────────────────────────────────────────────────────

/// Per-frame timing snapshot for the timeline visualization.
#[derive(Debug, Clone)]
pub struct FrameMeta {
    pub kind: FrameKind,
    pub pts: Duration,
    pub is_keyframe: bool,
    /// Wall-clock time when the frame was received from the transport.
    pub received: Instant,
    /// Wall-clock time when decode completed.
    pub decoded: Option<Instant>,
    /// Wall-clock time when the frame was released from the playout buffer.
    pub rendered: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Video,
    Audio,
}

/// Ring buffer of frame timing entries for the timeline panel.
#[derive(Clone, Debug)]
pub struct Timeline {
    frames: Arc<Mutex<VecDeque<FrameMeta>>>,
    cap: usize,
}

impl Timeline {
    pub fn new(cap: usize) -> Self {
        Self {
            frames: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            cap,
        }
    }

    pub fn push(&self, entry: FrameMeta) {
        let mut frames = self.frames.lock().expect("timeline lock");
        if frames.len() >= self.cap {
            frames.pop_front();
        }
        frames.push_back(entry);
    }

    pub fn snapshot(&self) -> Vec<FrameMeta> {
        self.frames
            .lock()
            .expect("timeline lock")
            .iter()
            .cloned()
            .collect()
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new(600)
    }
}

// ── Composite stats ─────────────────────────────────────────────────

/// Stats passed to the decode pipeline. Groups render, timing, and
/// timeline into a single value to avoid threading a 3-element tuple.
#[derive(Clone, Debug, Default)]
pub struct DecodeStats {
    pub render: RenderStats,
    pub timing: TimingStats,
    pub timeline: Timeline,
}

/// Optional parameters for encoder pipelines.
#[derive(Debug, Clone, Default)]
pub struct EncodeOpts {
    /// Stats collectors for publish-side encode metrics.
    pub stats: Option<EncodeStats>,
}

/// All stats for a subscribe-side broadcast. Owned by `RemoteBroadcast`.
#[derive(Clone, Debug, Default)]
pub struct SubscribeStats {
    pub net: NetStats,
    pub render: RenderStats,
    pub timing: TimingStats,
    pub timeline: Timeline,
}

impl SubscribeStats {
    /// Returns a [`DecodeStats`] clone for passing to decode pipelines.
    pub fn decode_stats(&self) -> DecodeStats {
        DecodeStats {
            render: self.render.clone(),
            timing: self.timing.clone(),
            timeline: self.timeline.clone(),
        }
    }
}

/// All stats for a publish-side broadcast. Owned by `LocalBroadcast`.
#[derive(Clone, Debug, Default)]
pub struct PublishStats {
    pub net: NetStats,
    pub encode: EncodeStats,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_ema_and_history() {
        let m = Metric::new(MetricMeta::responsive("test", "ms"));
        m.record(10.0);
        m.record(20.0);
        m.record(30.0);
        assert!(m.current() > 10.0 && m.current() < 30.0);
        assert_eq!(m.history().len(), 3);
        assert!(m.has_samples());
    }

    #[test]
    fn metric_no_samples() {
        let m = Metric::new(MetricMeta::smooth("test", ""));
        assert!(!m.has_samples());
        assert_eq!(m.current(), 0.0);
        assert!(m.history().is_empty());
    }

    #[test]
    fn label_set_get() {
        let l = Label::new("initial");
        assert_eq!(l.get(), "initial");
        l.set("changed");
        assert_eq!(l.get(), "changed");
    }
}
