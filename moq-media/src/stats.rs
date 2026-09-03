//! The counters a debug overlay draws.
//!
//! An observable value is either a [`Metric`], which keeps a smoothed current
//! reading alongside a ring buffer of samples for a sparkline, or a [`Label`],
//! which is a string. Both are cheap to clone and safe to write from any
//! thread, so a pipeline holds its own handle rather than reaching for a
//! registry.
//!
//! They are grouped by where they come from: [`NetStats`] from the transport,
//! [`EncodeStats`] from the publish path, [`RenderStats`] and [`TimingStats`]
//! from the subscribe path. No string keys and no registration, so a metric
//! that nothing writes is visible as an unused field rather than as an empty
//! row at runtime.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// --- Metric ----------------------------------------------------------

/// Static metadata for display and thresholds.
#[derive(Debug, Clone, Copy)]
pub struct MetricMeta {
    /// The name the overlay draws.
    pub label: &'static str,
    /// The unit suffix, or the empty string for a bare count.
    pub unit: &'static str,
    /// Weight given to each new sample by the exponential moving average, in
    /// `0.0..=1.0`. Lower is smoother and slower to move.
    pub alpha: f64,
    /// How many samples the history ring buffer keeps for a sparkline.
    pub history_cap: usize,
    /// Color thresholds, or `None` to draw the value unconditionally.
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
    /// Metadata for a value worth reading as a trend rather than a reading,
    /// such as a bitrate.
    #[must_use]
    pub const fn smooth(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            alpha: 0.1,
            history_cap: 300,
            thresholds: None,
        }
    }
    /// Metadata for a value that should follow what just happened, such as a
    /// frame rate.
    #[must_use]
    pub const fn responsive(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            alpha: 0.3,
            history_cap: 300,
            thresholds: None,
        }
    }
    /// Returns the metadata with color thresholds attached.
    ///
    /// Set `inverted` for a metric where a higher value is the better one, such
    /// as a frame rate.
    #[must_use]
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
    /// Creates a metric with no samples yet.
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

    /// Records a sample.
    ///
    /// The smoothed value is read and written separately rather than under one
    /// lock, so two concurrent writers can lose an update between them. Every
    /// metric here has one writer, and a debug overlay is not worth a lock on
    /// the encode path.
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

        let mut hist = self.inner.history.lock().expect("poisoned");
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
        let hist = self.inner.history.lock().expect("poisoned");
        out.extend(hist.iter().copied());
    }

    /// Returns a copy of the history ring buffer.
    pub fn history(&self) -> Vec<(Instant, f64)> {
        let mut v = Vec::new();
        self.history_into(&mut v);
        v
    }

    /// Returns how this metric should be labelled and colored.
    pub fn meta(&self) -> &MetricMeta {
        &self.inner.meta
    }

    /// Reports whether anything has been recorded, so a caller can draw a
    /// placeholder rather than a confident zero.
    pub fn has_samples(&self) -> bool {
        self.inner.sample_count.load(Ordering::Relaxed) > 0
    }

    /// Records a [`Duration`] as milliseconds.
    pub fn record_ms(&self, d: Duration) {
        self.record(d.as_secs_f64() * 1000.0);
    }

    /// Records an FPS sample from the gap since the previous event. Ignores
    /// gaps shorter than 5ms to avoid division noise.
    ///
    /// Prefer [`Rate`] for anything a person reads. One gap is not a frame
    /// rate, and the reciprocal of one is a bad estimate of it: see [`Rate`]
    /// for why the average of those reciprocals reads high.
    pub fn record_fps_gap(&self, gap: Duration) {
        if gap >= Duration::from_millis(5) {
            self.record(1.0 / gap.as_secs_f64());
        }
    }
}

/// How long a [`Rate`] counts events for before it has a figure to report.
///
/// A second is what a frame rate is quoted in, and it is short enough that a
/// stream which stops is seen to stop.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Counts events and reports how many happened per second.
///
/// This exists because dividing one into the gap since the last event does not
/// measure a frame rate, and the reading it produced jumped around enough to be
/// unreadable. Two things were wrong with it. A single late frame is a whole
/// sample: one 20ms gap in a 30fps stream reads as 50, and the smoothing that
/// followed was chasing that rather than removing it. And averaging reciprocals
/// is biased upwards, because the short gaps contribute more than the long ones
/// cancel: gaps alternating 20ms and 47ms average 33.5ms, which is 29.9fps, but
/// their reciprocals average 35.6. So a stream delivering exactly 30 frames a
/// second in bursts read as 35 and swung by twenty.
///
/// Counting over a window has neither problem. It is what the figure claims to
/// be, and a burst inside the window moves it not at all.
#[derive(Debug)]
pub struct Rate {
    inner: Arc<Mutex<RateWindow>>,
}

#[derive(Debug)]
struct RateWindow {
    started: Instant,
    events: u32,
}

impl Clone for Rate {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for Rate {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RateWindow {
                started: Instant::now(),
                events: 0,
            })),
        }
    }
}

impl Rate {
    /// Counts one event, and returns the rate per second when the window has
    /// closed.
    ///
    /// Returns `None` the rest of the time, so a caller records a figure only
    /// when there is one to record.
    pub fn tick(&self) -> Option<f64> {
        let mut window = self.inner.lock().expect("poisoned");
        window.events += 1;
        let elapsed = window.started.elapsed();
        if elapsed < RATE_WINDOW {
            return None;
        }
        let rate = f64::from(window.events) / elapsed.as_secs_f64();
        window.started = Instant::now();
        window.events = 0;
        Some(rate)
    }
}

// --- Label -----------------------------------------------------------

/// An observable string label (e.g. codec name, path type).
#[derive(Clone, Debug)]
pub struct Label {
    inner: Arc<Mutex<String>>,
}

impl Label {
    /// Creates a label reading `initial`.
    pub fn new(initial: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(initial.into())),
        }
    }

    /// Replaces the label.
    pub fn set(&self, value: impl Into<String>) {
        *self.inner.lock().expect("poisoned") = value.into();
    }

    /// Returns a copy of the label.
    pub fn get(&self) -> String {
        self.inner.lock().expect("poisoned").clone()
    }
}

impl Default for Label {
    fn default() -> Self {
        Self::new("")
    }
}

// --- Stat category structs -------------------------------------------

/// Network stats. Written by the transport bridge (iroh-live or
/// web_transport_trait), read by the overlay.
#[derive(Clone, Debug)]
pub struct NetStats {
    /// Round trip to the peer.
    pub rtt_ms: Metric,
    /// Loss rate as a percentage, over whatever window the bridge computes.
    pub loss_pct: Metric,
    /// Throughput towards this endpoint.
    pub bw_down_mbps: Metric,
    /// Throughput away from this endpoint.
    pub bw_up_mbps: Metric,
    /// How many network paths the connection currently has.
    pub paths_active: Metric,
    /// How the connection reaches the peer, such as `direct` or `relay`.
    pub path_type: Label,
    /// The remote address in use.
    pub path_addr: Label,
    /// Who is on the other end.
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
    /// Frames per second arriving from the source, which every rung of a
    /// simulcast ladder shares.
    pub fps: Metric,
    /// How long one encode call took.
    pub encode_ms: Metric,
    /// Published bits per second, over the gap between one encode and the next.
    /// With a simulcast ladder every rung writes here, so the smoothed value
    /// sits somewhere among them rather than summing them.
    pub bitrate_kbps: Metric,
    /// The codec being encoded, such as `H264`.
    pub codec: Label,
    /// The encoder backend that opened, such as `openh264` or `vaapi`.
    pub encoder: Label,
    /// The encoded resolution.
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
    /// Frames per second reaching the renderer.
    pub fps: Metric,
    /// How long one transport read and decode took together, which is where
    /// `moq_video::decode::Consumer` does both.
    pub decode_ms: Metric,
    /// The decoder backend that opened, which can change across a rendition
    /// switch.
    pub decoder: Label,
    /// The renderer drawing the frames. Written by whatever draws them, since
    /// this crate does not.
    pub renderer: Label,
    /// The rendition currently decoding.
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
                MetricMeta::responsive("A/V delta", "ms").with_thresholds(20.0, 50.0, false),
            ),
            video_buf: Metric::new(MetricMeta::responsive("VideoBuf", "")),
        }
    }
}

// --- Timeline --------------------------------------------------------

/// Per-frame timing snapshot for the timeline visualization.
#[derive(Debug, Clone)]
pub struct FrameMeta {
    /// Which medium the frame belongs to.
    pub kind: FrameKind,
    /// The frame's presentation timestamp.
    pub pts: Duration,
    /// Whether the frame can be decoded without a predecessor.
    pub is_keyframe: bool,
    /// Wall-clock time when the frame was received from the transport.
    pub received: Instant,
    /// Wall-clock time when decode completed.
    pub decoded: Option<Instant>,
    /// Wall-clock time when the frame was released from the playout buffer.
    pub rendered: Instant,
}

/// Which medium a [`FrameMeta`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A decoded picture.
    Video,
    /// A decoded block of samples.
    Audio,
}

/// Ring buffer of frame timing entries for the timeline panel.
#[derive(Clone, Debug)]
pub struct Timeline {
    frames: Arc<Mutex<VecDeque<FrameMeta>>>,
    cap: usize,
}

impl Timeline {
    /// Creates a timeline keeping the last `cap` frames.
    pub fn new(cap: usize) -> Self {
        Self {
            frames: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            cap,
        }
    }

    /// Records one frame, discarding the oldest once the buffer is full.
    pub fn push(&self, entry: FrameMeta) {
        let mut frames = self.frames.lock().expect("poisoned");
        if frames.len() >= self.cap {
            frames.pop_front();
        }
        frames.push_back(entry);
    }

    /// Returns a copy of every frame still in the buffer, oldest first.
    pub fn snapshot(&self) -> Vec<FrameMeta> {
        self.frames
            .lock()
            .expect("poisoned")
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

// --- Composite stats -------------------------------------------------

/// All stats for a subscribe-side broadcast. Owned by `RemoteBroadcast`.
#[derive(Clone, Debug, Default)]
pub struct SubscribeStats {
    /// Written by the transport bridge outside this crate.
    pub net: NetStats,
    /// Written by the video decode path.
    pub render: RenderStats,
    /// Written by whatever paces playout. Nothing in this crate does yet, so
    /// these read empty on a plain subscription.
    pub timing: TimingStats,
    /// Written by whatever paces playout, alongside [`SubscribeStats::timing`].
    pub timeline: Timeline,
}

/// All stats for a publish-side broadcast. Owned by `LocalBroadcast`.
#[derive(Clone, Debug, Default)]
pub struct PublishStats {
    /// Written by the transport bridge outside this crate.
    pub net: NetStats,
    /// Written by the video publish path.
    pub encode: EncodeStats,
}

// --- Tests -----------------------------------------------------------

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

#[cfg(test)]
mod rate_tests {
    use std::{thread::sleep, time::Duration};

    use super::Rate;

    /// Nothing is reported before the window closes, so a reader never sees a
    /// figure derived from two frames.
    #[test]
    fn a_rate_reports_nothing_until_its_window_closes() {
        let rate = Rate::default();
        for _ in 0..10 {
            assert!(rate.tick().is_none());
        }
    }

    /// The figure is a count over the window, so delivery that arrives in
    /// bursts reads the same as delivery that is evenly spaced. The old
    /// reciprocal-of-one-gap measure read this stream at about 36fps while it
    /// swung between 21 and 50.
    #[test]
    fn a_burst_reads_the_same_as_an_even_stream() {
        let rate = Rate::default();
        let mut reported = None;
        // Bursts of ten, 350ms apart. The window closes during the fourth,
        // which is the first tick that can report at all: three bursts span
        // 700ms and the figure is only produced once a full second has passed.
        for _ in 0..4 {
            for _ in 0..10 {
                if let Some(value) = rate.tick() {
                    reported = Some(value);
                }
            }
            sleep(Duration::from_millis(350));
        }
        let reported = reported.expect("a window of over a second has to report");
        assert!(
            (25.0..35.0).contains(&reported),
            "thirty events in ~1.05s should read near 30fps, got {reported}",
        );
    }
}
