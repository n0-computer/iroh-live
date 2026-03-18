//! Generic time-series metrics tracking with EMA smoothing and ring buffer
//! history for sparkline graphs.
//!
//! Producers push named `f64` samples via [`MetricsCollector::record`].
//! Consumers read snapshots via [`MetricsCollector::snapshot`] for overlay
//! rendering. Thread-safe — lock held only briefly per sample.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

// ── Well-known metric names ─────────────────────────────────────────

// Network (pushed by transport bridge in iroh-live)
pub const NET_RTT_MS: &str = "net.rtt";
pub const NET_LOSS_PCT: &str = "net.loss";
pub const NET_BW_DOWN_MBPS: &str = "net.bw_down";
pub const NET_BW_UP_MBPS: &str = "net.bw_up";

// Capture/encode (pushed by publish pipeline)
pub const CAP_FPS: &str = "cap.fps";
pub const CAP_ENCODE_MS: &str = "cap.encode_ms";
pub const CAP_BITRATE_KBPS: &str = "cap.bitrate";

// Render/decode (pushed by subscribe/decode pipeline)
pub const RND_FPS: &str = "rnd.fps";
pub const RND_DECODE_MS: &str = "rnd.decode_ms";

// Timing (pushed by playout clock)
pub const TMG_JITTER_MS: &str = "tmg.jitter";
pub const TMG_DELAY_MS: &str = "tmg.delay";
pub const TMG_DRIFT_MS: &str = "tmg.drift";
pub const TMG_REANCHOR_COUNT: &str = "tmg.reanchors";
pub const TMG_BUF_FRAMES: &str = "tmg.buf_frames";

// String labels (non-numeric metadata)
pub const LBL_PEER: &str = "lbl.peer";
pub const LBL_DECODER: &str = "lbl.decoder";
pub const LBL_RENDERER: &str = "lbl.renderer";
pub const LBL_RENDITION: &str = "lbl.rendition";
pub const LBL_CODEC: &str = "lbl.codec";
pub const LBL_RESOLUTION: &str = "lbl.resolution";
/// "D" (direct) or "R" (relayed) — set from iroh path info.
pub const LBL_PATH_TYPE: &str = "lbl.path_type";
/// Full address of the selected path (debug format of TransportAddr).
pub const LBL_PATH_ADDR: &str = "lbl.path_addr";
/// Encoder name (for publish/capture side).
pub const LBL_ENCODER: &str = "lbl.encoder";

// ── Types ───────────────────────────────────────────────────────────

/// Configuration for a metric slot.
#[derive(Debug, Clone)]
pub struct MetricConfig {
    /// Human-readable label (e.g. "RTT").
    pub label: &'static str,
    /// Unit suffix for display (e.g. "ms", "fps", "%").
    pub unit: &'static str,
    /// EMA smoothing factor (0.0–1.0). Higher = more responsive.
    pub alpha: f64,
    /// Ring buffer capacity for history (number of samples kept).
    pub history_cap: usize,
}

impl MetricConfig {
    /// Responsive metric (alpha=0.3) with 300-sample history.
    pub const fn responsive(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            alpha: 0.3,
            history_cap: 300,
        }
    }

    /// Smooth metric (alpha=0.1) with 300-sample history.
    pub const fn smooth(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            alpha: 0.1,
            history_cap: 300,
        }
    }
}

/// A single time-series metric with smoothed current value and history.
#[derive(Debug, Clone)]
pub struct TimeSeries {
    /// EMA-smoothed current value.
    pub current: f64,
    /// Ring buffer of `(timestamp, raw_value)` for sparkline graphs.
    pub history: Vec<(Instant, f64)>,
    /// Unit suffix.
    pub unit: &'static str,
    /// Human-readable label.
    pub label: &'static str,
}

/// Immutable snapshot of all metrics at a point in time.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub metrics: BTreeMap<String, TimeSeries>,
    /// String labels for non-numeric metadata (codec, decoder, render path, etc.).
    pub labels: BTreeMap<String, String>,
}

impl MetricsSnapshot {
    /// Returns the smoothed current value for a metric.
    pub fn get(&self, name: &str) -> Option<f64> {
        self.metrics.get(name).map(|ts| ts.current)
    }

    /// Returns the full time series for a metric.
    pub fn series(&self, name: &str) -> Option<&TimeSeries> {
        self.metrics.get(name)
    }

    /// Returns a string label value.
    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(|s| s.as_str())
    }
}

// ── Collector ───────────────────────────────────────────────────────

/// Thread-safe collector for named metrics.
///
/// Push samples from any thread via [`record`](Self::record). Read
/// snapshots via [`snapshot`](Self::snapshot) for overlay rendering.
#[derive(Debug, Clone)]
pub struct MetricsCollector {
    inner: Arc<Mutex<CollectorInner>>,
}

#[derive(Debug)]
struct CollectorInner {
    metrics: BTreeMap<String, MetricSlot>,
    labels: BTreeMap<String, String>,
}

#[derive(Debug)]
struct MetricSlot {
    config: MetricConfig,
    current: f64,
    history: RingBuf,
    sample_count: u64,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CollectorInner {
                metrics: BTreeMap::new(),
                labels: BTreeMap::new(),
            })),
        }
    }

    /// Registers a named metric. Idempotent — re-registering updates config.
    pub fn register(&self, name: impl Into<String>, config: MetricConfig) {
        let mut inner = self.inner.lock().expect("metrics lock");
        let name = name.into();
        inner.metrics.entry(name).or_insert_with(|| MetricSlot {
            config,
            current: 0.0,
            history: RingBuf::new(300),
            sample_count: 0,
        });
    }

    /// Records a sample. No-op if the metric is not registered.
    pub fn record(&self, name: &str, value: f64) {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("metrics lock");
        if let Some(slot) = inner.metrics.get_mut(name) {
            if slot.sample_count == 0 {
                slot.current = value;
            } else {
                let a = slot.config.alpha;
                slot.current = a * value + (1.0 - a) * slot.current;
            }
            slot.sample_count += 1;
            slot.history.push(now, value);
        }
    }

    /// Sets a string label (non-numeric metadata like codec name, decoder, etc.).
    pub fn set_label(&self, name: &str, value: impl Into<String>) {
        let mut inner = self.inner.lock().expect("metrics lock");
        inner.labels.insert(name.to_string(), value.into());
    }

    /// Returns a snapshot of all registered metrics and labels.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let inner = self.inner.lock().expect("metrics lock");
        let metrics = inner
            .metrics
            .iter()
            .map(|(k, slot)| {
                (
                    k.clone(),
                    TimeSeries {
                        current: slot.current,
                        history: slot.history.to_vec(),
                        unit: slot.config.unit,
                        label: slot.config.label,
                    },
                )
            })
            .collect();
        MetricsSnapshot {
            metrics,
            labels: inner.labels.clone(),
        }
    }

    /// Registers the standard set of metrics with default configs.
    pub fn register_defaults(&self) {
        self.register(NET_RTT_MS, MetricConfig::smooth("RTT", "ms"));
        self.register(NET_LOSS_PCT, MetricConfig::smooth("Loss", "%"));
        self.register(NET_BW_DOWN_MBPS, MetricConfig::smooth("Down", "Mbps"));
        self.register(NET_BW_UP_MBPS, MetricConfig::smooth("Up", "Mbps"));
        self.register(CAP_FPS, MetricConfig::responsive("FPS", ""));
        self.register(CAP_ENCODE_MS, MetricConfig::responsive("Encode", "ms"));
        self.register(CAP_BITRATE_KBPS, MetricConfig::smooth("Bitrate", "kbps"));
        self.register(RND_FPS, MetricConfig::responsive("FPS", ""));
        self.register(RND_DECODE_MS, MetricConfig::responsive("Decode", "ms"));
        self.register(TMG_JITTER_MS, MetricConfig::smooth("Jitter", "ms"));
        self.register(TMG_DELAY_MS, MetricConfig::smooth("Delay", "ms"));
        self.register(TMG_DRIFT_MS, MetricConfig::smooth("Drift", "ms"));
        self.register(TMG_REANCHOR_COUNT, MetricConfig::smooth("Reanchors", ""));
        self.register(TMG_BUF_FRAMES, MetricConfig::responsive("Buffer", ""));
    }
}

// ── Ring buffer ─────────────────────────────────────────────────────

#[derive(Debug)]
struct RingBuf {
    buf: Vec<(Instant, f64)>,
    cap: usize,
    write_pos: usize,
    len: usize,
}

impl RingBuf {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            cap,
            write_pos: 0,
            len: 0,
        }
    }

    fn push(&mut self, ts: Instant, value: f64) {
        if self.buf.len() < self.cap {
            self.buf.push((ts, value));
            self.len = self.buf.len();
        } else {
            self.buf[self.write_pos] = (ts, value);
        }
        self.write_pos = (self.write_pos + 1) % self.cap;
    }

    fn to_vec(&self) -> Vec<(Instant, f64)> {
        if self.len < self.cap {
            self.buf.clone()
        } else {
            // Return in chronological order: write_pos..end, then 0..write_pos
            let mut out = Vec::with_capacity(self.cap);
            out.extend_from_slice(&self.buf[self.write_pos..]);
            out.extend_from_slice(&self.buf[..self.write_pos]);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_record_and_snapshot() {
        let collector = MetricsCollector::new();
        collector.register("test", MetricConfig::responsive("Test", "ms"));

        collector.record("test", 10.0);
        collector.record("test", 20.0);
        collector.record("test", 30.0);

        let snap = collector.snapshot();
        let ts = snap.series("test").unwrap();
        assert!(ts.current > 10.0 && ts.current < 30.0, "EMA should smooth");
        assert_eq!(ts.history.len(), 3);
        assert_eq!(ts.unit, "ms");
        assert_eq!(ts.label, "Test");
    }

    #[test]
    fn unregistered_metric_ignored() {
        let collector = MetricsCollector::new();
        collector.record("nonexistent", 42.0); // should not panic
        let snap = collector.snapshot();
        assert!(snap.get("nonexistent").is_none());
    }

    #[test]
    fn ring_buffer_wraps() {
        let mut ring = RingBuf::new(3);
        let now = Instant::now();
        ring.push(now, 1.0);
        ring.push(now, 2.0);
        ring.push(now, 3.0);
        ring.push(now, 4.0); // overwrites 1.0
        ring.push(now, 5.0); // overwrites 2.0

        let v = ring.to_vec();
        assert_eq!(v.len(), 3);
        // Should be in chronological order: 3, 4, 5
        assert_eq!(v[0].1, 3.0);
        assert_eq!(v[1].1, 4.0);
        assert_eq!(v[2].1, 5.0);
    }
}
