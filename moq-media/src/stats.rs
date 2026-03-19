//! Typed metrics infrastructure for debug overlays.
//!
//! Each observable value is a [`Metric`] (EMA-smoothed current value +
//! ring buffer history for sparklines) or a [`Label`] (atomic string).
//! These are grouped into typed structs ([`NetStats`], [`CaptureStats`],
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

/// Capture/encode stats. Written by the encode pipeline.
#[derive(Clone, Debug)]
pub struct CaptureStats {
    pub fps: Metric,
    pub encode_ms: Metric,
    pub bitrate_kbps: Metric,
    pub codec: Label,
    pub encoder: Label,
    pub resolution: Label,
}

impl Default for CaptureStats {
    fn default() -> Self {
        Self {
            fps: Metric::new(MetricMeta::responsive("FPS", "").with_thresholds(25.0, 15.0, true)),
            encode_ms: Metric::new(MetricMeta::responsive("Encode", "ms")),
            bitrate_kbps: Metric::new(MetricMeta::smooth("Bitrate", "kbps")),
            codec: Label::default(),
            encoder: Label::default(),
            resolution: Label::default(),
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

/// Timing/playout stats. Written by the decode/playout pipeline.
#[derive(Clone, Debug)]
pub struct TimingStats {
    pub jitter_ms: Metric,
    pub delay_ms: Metric,
    pub drift_ms: Metric,
    pub buf_frames: Metric,
    pub frames_skipped: Metric,
    pub freezes: Metric,
}

impl Default for TimingStats {
    fn default() -> Self {
        Self {
            jitter_ms: Metric::new(
                MetricMeta::smooth("Jitter", "ms").with_thresholds(10.0, 30.0, false),
            ),
            delay_ms: Metric::new(
                MetricMeta::responsive("Delay", "ms").with_thresholds(100.0, 300.0, false),
            ),
            drift_ms: Metric::new(MetricMeta::smooth("Drift", "ms")),
            buf_frames: Metric::new(MetricMeta::responsive("Buffer", "")),
            frames_skipped: Metric::new(MetricMeta::smooth("Skipped", "")),
            freezes: Metric::new(MetricMeta::smooth("Freezes", "")),
        }
    }
}

// ── Timeline ────────────────────────────────────────────────────────

/// Per-frame timing snapshot for the timeline visualization.
#[derive(Debug, Clone)]
pub struct FrameTimingEntry {
    pub kind: FrameKind,
    pub pts: Duration,
    pub receive_wall: Instant,
    pub decode_end: Option<Instant>,
    pub render_wall: Instant,
    pub is_keyframe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Video,
    Audio,
}

/// Ring buffer of frame timing entries for the timeline panel.
#[derive(Clone, Debug)]
pub struct Timeline {
    frames: Arc<Mutex<VecDeque<FrameTimingEntry>>>,
    cap: usize,
}

impl Timeline {
    pub fn new(cap: usize) -> Self {
        Self {
            frames: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            cap,
        }
    }

    pub fn push(&self, entry: FrameTimingEntry) {
        let mut frames = self.frames.lock().expect("timeline lock");
        if frames.len() >= self.cap {
            frames.pop_front();
        }
        frames.push_back(entry);
    }

    pub fn snapshot(&self) -> Vec<FrameTimingEntry> {
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
#[derive(Clone, Debug)]
pub struct DecodeStats {
    pub render: RenderStats,
    pub timing: TimingStats,
    pub timeline: Timeline,
}

/// Optional parameters for decoder pipelines.
///
/// Bundles clock and stats to avoid telescoping constructor variants.
/// Use `Default` for simple cases (no clock, no stats).
#[derive(Debug, Clone, Default)]
pub struct DecodeOpts {
    /// Shared playout clock for A/V sync.
    pub clock: Option<crate::playout::PlayoutClock>,
    /// Stats collectors for metrics and timeline.
    pub stats: Option<DecodeStats>,
    /// Shared skip threshold in milliseconds. When the decoder falls behind
    /// by more than this amount, non-keyframes are skipped until the next
    /// keyframe to recover. `None` uses the default (500ms).
    pub skip_threshold_ms: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

/// Optional parameters for encoder pipelines.
#[derive(Debug, Clone, Default)]
pub struct EncodeOpts {
    /// Stats collectors for capture metrics.
    pub stats: Option<CaptureStats>,
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
    pub capture: CaptureStats,
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

    #[test]
    fn timeline_ring_buffer() {
        let tl = Timeline::new(3);
        let now = Instant::now();
        for i in 0..5 {
            tl.push(FrameTimingEntry {
                kind: FrameKind::Video,
                pts: Duration::from_millis(i * 33),
                receive_wall: now,
                decode_end: Some(now),
                render_wall: now,
                is_keyframe: i == 0,
            });
        }
        let snap = tl.snapshot();
        assert_eq!(snap.len(), 3); // capped at 3
    }
}
