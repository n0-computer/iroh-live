# Debug overlays

Consolidated stats tracking and overlay rendering for iroh-live.
Replaces ad-hoc per-example stats code with a shared infrastructure
in `moq-media` (data) and `moq-media-egui` (rendering).

## Current state

Stats are scattered across examples with no shared infrastructure:

- **`StatsSmoother`** (`iroh-live/src/util.rs`) — smooths BW up/down
  and RTT over 1-second windows. Only used by `split.rs` and `watch.rs`.
- **`FrameStats`** (`moq-media-egui/src/overlay.rs`) — fps counter +
  display delay estimate. Baseline drift detection is crude (monotonic
  accumulation, never resets).
- **`NetworkSignals`** (`moq-media/src/net.rs`) — RTT, loss rate,
  available bandwidth, congestion events. Fed to adaptive bitrate only.
- **`PlayoutClock`** (`moq-media/src/playout.rs`) — jitter (RFC 3550
  EMA), drift/reanchor stats, buffer offset. Internal, not surfaced.
- **`overlay_bar`** (`moq-media-egui/src/overlay.rs`) — paints one line
  of monospace text on semi-transparent background. No structure.

Each example assembles its own overlay text by manually polling
`conn.stats()`, `conn.paths()`, `track.decoder_name()`, etc. The
`split.rs` example has ~40 lines of manual overlay assembly.

### What's missing

- No per-frame decode time tracking
- No encode time or capture stats exposure
- No audio-specific diagnostics (buffer depth, sync offset)
- No frame skip/drop counters at API level
- No visual history (graphs, sparklines, timeline)
- No A/V sync offset measurement
- `VideoFrame` has only `timestamp: Duration` — no receive-time,
  decode-time, or render-time annotations
- Adaptive algorithm state not visible (probing, downgrade reason)

## Design

### Part 1: `MetricsCollector` in `moq-media`

A generic time-series tracker. Producers push named f64 samples;
consumers read snapshots with smoothed values and ring buffer history.

```rust
pub struct MetricsCollector {
    inner: Arc<Mutex<CollectorInner>>,
}

impl MetricsCollector {
    pub fn new() -> Self;
    pub fn register(&self, name: &str, config: MetricConfig);
    pub fn record(&self, name: &str, value: f64);
    pub fn snapshot(&self) -> MetricsSnapshot;
}

pub struct MetricConfig {
    pub label: &'static str,
    pub unit: &'static str,
    pub alpha: f64,           // EMA smoothing factor (0.1 = smooth, 0.3 = responsive)
    pub history_capacity: usize, // ring buffer size for graphs
}

pub struct MetricsSnapshot {
    pub metrics: BTreeMap<String, TimeSeries>,
}

pub struct TimeSeries {
    pub current: f64,                   // EMA-smoothed
    pub history: Vec<(Instant, f64)>,   // ring buffer copy
    pub unit: &'static str,
    pub label: &'static str,
}
```

**EMA replaces 1-second windowing.** Configurable per metric, handles
irregular sample rates, no timer logic. History ring buffer (`VecDeque`
with capacity cap) enables sparkline graphs without external crates.

**Well-known metric constants** provide the schema:

```rust
// Network (pushed by iroh-live transport bridge)
pub const NET_RTT_MS: &str = "net.rtt";
pub const NET_LOSS_PCT: &str = "net.loss";
pub const NET_BW_DOWN_MBPS: &str = "net.bw_down";
pub const NET_BW_UP_MBPS: &str = "net.bw_up";

// Capture/encode (pushed by publish pipeline)
pub const CAP_FPS: &str = "cap.fps";
pub const CAP_ENCODE_MS: &str = "cap.encode_ms";
pub const CAP_BITRATE_KBPS: &str = "cap.bitrate";

// Render/decode (pushed by subscribe pipeline)
pub const RND_FPS: &str = "rnd.fps";
pub const RND_DECODE_MS: &str = "rnd.decode_ms";

// Timing (pushed by playout clock)
pub const TMG_JITTER_MS: &str = "tmg.jitter";
pub const TMG_DELAY_MS: &str = "tmg.delay";
pub const TMG_DRIFT_MS: &str = "tmg.drift";
pub const TMG_AV_SYNC_MS: &str = "tmg.av_sync";
```

**Why `moq-media`, not `iroh-live`:** the publish and subscribe
pipelines need to record stats (decode time, encode time, playout
jitter) and they live in `moq-media`. Transport-specific stats (RTT,
loss) are pushed in from `iroh-live` via the same `record()` API. No
trait needed — the well-known constants are the contract.

### Part 1b: integration points

| Producer | File | What it records |
|----------|------|-----------------|
| Transport bridge | `iroh-live/src/util.rs` | RTT, loss, BW up/down, congestion events |
| Decode pipeline | `moq-media/src/pipeline.rs` | decode_ms, frame count |
| Playout clock | `moq-media/src/playout.rs` | jitter, drift, reanchor count, delay |
| Publish pipeline | `moq-media/src/publish.rs` | encode_ms, capture fps, output bitrate |
| Renderer | `moq-media-egui/src/lib.rs` | render path (categorical) |

Each producer calls `collector.record(NAME, value)` at natural points
(after decode, after playout observation, every 200ms for network).
The transport bridge is a new `spawn_stats_producer` function in
`iroh-live/src/util.rs`, similar to the existing `spawn_signal_producer`.

### Part 1c: frame timing annotations

Extend `VideoFrame` with optional timing metadata so the timeline view
can show per-frame source→receive→render gaps:

```rust
/// Timing annotations accumulated as a frame moves through the pipeline.
/// Fields are set at each stage; unset fields remain `None`.
#[derive(Debug, Clone, Default)]
pub struct FrameTiming {
    pub source_pts: Option<Duration>,
    pub receive_wall: Option<Instant>,
    pub decode_start: Option<Instant>,
    pub decode_end: Option<Instant>,
    pub render_wall: Option<Instant>,
}
```

Added as `pub timing: FrameTiming` on `VideoFrame`. Each stage stamps
its field. No per-frame allocation — `FrameTiming` is 5 `Option<T>`
fields, 80 bytes inline. The timeline view reads these to draw boxes.

Audio frames get a parallel `AudioFrameTiming` with `source_pts`,
`receive_wall`, and `playout_wall`.

### Part 2: overlay UI in `moq-media-egui`

#### Stat bars

Four categories: **NET** (blue), **CAP** (green), **RND** (orange),
**TMG** (purple). Each has a 3px left color stripe.

Three states, cycled by clicking:

1. **Collapsed** — category label + key metric (e.g. "NET 12ms").
   Bars tile left-to-right at bottom-left, ~120px each.
2. **Expanded** — full-width bar, all metrics for the category on one
   line (e.g. "NET  rtt:12ms  loss:0.1%  down:2.4Mbps  up:0.3Mbps").
3. **Full** — multi-line, one metric per line with inline sparkline
   (last 5 seconds of history drawn as a 60px wide mini line graph).

Collapsed bars stack horizontally. Expanded/full bars stack vertically
above them, pushing collapsed bars up. Styled like the existing
`overlay_bar`: monospace 11px, semi-transparent black background
(alpha 180).

```rust
pub struct DebugOverlay {
    states: HashMap<StatCategory, BarState>,
    visible: bool,
}

impl DebugOverlay {
    pub fn new() -> Self;
    pub fn toggle(&mut self);
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, snap: &MetricsSnapshot);
}
```

Click handling via `ui.interact()` on painted rects — same pattern as
egui buttons but drawn as overlay.

#### Key metrics per category (collapsed view)

| Category | Key metric | Good | Warn | Bad |
|----------|-----------|------|------|-----|
| NET | RTT (ms) | <30 | 30–100 | >100 |
| CAP | FPS | >25 | 15–25 | <15 |
| RND | FPS | >25 | 15–25 | <15 |
| TMG | Jitter (ms) | <10 | 10–30 | >30 |

Color-code the value text: green/yellow/red based on thresholds.

#### Tier ranking (from WebRTC/OBS/mpv research)

**Tier 1 — always visible (collapsed bars):**
1. RTT (ms) — network health baseline
2. FPS (decoded) — pipeline health
3. Jitter (ms) — arrival variance, drives buffer sizing
4. Dropped frames — split by cause (network/decode/render)

**Tier 2 — show on expand:**
5. Loss rate (%)
6. Bandwidth up/down (Mbps)
7. Decode time (ms, last/avg/peak — mpv's approach)
8. Jitter buffer level (ms)
9. A/V sync offset (ms, signed — positive = video ahead)
10. Current rendition + limitation reason

**Tier 3 — full view with sparklines:**
11. All of above with 5-second history graphs
12. Encode time, capture fps (send side)
13. Drift + reanchor count
14. Congestion events

### Part 2b: timeline panel

Starts as a collapsed label/button ("TIME" in purple) stacked with the
other bars. Click once: renders a ~100px tall mini-timeline overlaid on
the video (like the expanded bars). Click again: expands to the bottom
half of the available rect, showing full detail with zoom/scroll.
Click again: collapses back. Same three-state cycle as the stat bars,
same integration API — just `overlay.show(ui, rect, snapshot)` handles
everything. Must remain trivial to add to any example.

```
┌──────────────────────────────────────────────┐
│ RTT ──────/\──────/\─────────────────  40px  │
│ recv delay ─────────────/──────────          │
├──────────────────────────────────────────────┤
│ Video  [██] [██] [██] [██] [██] [██]  60px  │
├──────────────────────────────────────────────┤
│ Audio  [█][█][█][█][█][█][█][█][█][█]  40px │
├──────────────────────────────────────────────┤
│ -10s     -8s     -6s     -4s     -2s    now  │
└──────────────────────────────────────────────┘
```

**RTT strip** (top 40px): cyan polyline, last 10 seconds, one sample
per 100ms. Second line in purple shows receive delay drift (wall delta
minus PTS delta from baseline). If the purple line diverges from
horizontal, clock drift or degradation is visible.

**Video strip** (60px): each decoded frame as a box, width =
inter-frame interval mapped to pixels (min 3px). Color encodes
decode-to-render latency: green (<5ms), yellow (5–15ms), red (>15ms).
Keyframes get a 1px white border. Hover tooltip shows PTS, receive
offset, decode time.

**Audio strip** (40px): each 20ms Opus frame as a box (min 2px).
Color: blue (on-time, within 5ms of target), orange (>10ms drift).

**X-axis**: wall-clock time, "now" at right edge. Default 10-second
window. Mouse wheel scrolls, Ctrl+wheel zooms (5s–30s range).
1-second grid lines as subtle vertical markers.

**Data structure:**

```rust
pub struct TimelineData {
    video: VecDeque<FrameTiming>,   // cap: 600
    audio: VecDeque<FrameTiming>,   // cap: 600
    rtt: VecDeque<(Instant, f32)>,  // cap: 200
}
```

62 KB total at capacity. Populated from pipeline hooks at natural
points (after decode, after playout observation, every 100ms for RTT).

**Implementation**: custom `Painter` drawing (~200 lines), matching
the existing `overlay.rs` pattern. No `egui_plot` dependency — custom
painting gives full control over per-frame coloring and is simpler.

## Implementation checklist

### Phase 1: stats infrastructure

- [x] `moq-media/src/stats.rs` — `MetricsCollector`, `MetricsSnapshot`,
  `TimeSeries`, ring buffer, well-known constants (~250 lines)
- [x] `moq-media/src/lib.rs` — add `pub mod stats`
- [x] `moq-media/src/pipeline.rs` — record `decode_ms`, fps, jitter,
  drift, buffer len after decode
- [ ] `moq-media/src/playout.rs` — record jitter, drift, delay
  (done in pipeline.rs instead, reading from clock)
- [ ] `moq-media/src/publish.rs` — record encode_ms, capture fps
- [x] `moq-media/src/subscribe.rs` — add `MetricsCollector` field to
  `RemoteBroadcast`, expose via `metrics()` / `set_metrics()`
- [x] `iroh-live/src/util.rs` — `spawn_stats_recorder` for transport
  stats (RTT, loss, BW)
- [ ] `rusty-codecs/src/format.rs` — add `FrameTiming` to `VideoFrame`

### Phase 2: overlay UI

- [x] `moq-media-egui/src/overlay.rs` — `DebugOverlay`, `StatCategory`,
  `BarState`, collapsed/expanded/full painting, click handling
- [x] `moq-media-egui/src/overlay.rs` — sparkline drawing for full mode
- [x] Update `split.rs` — replace manual overlay with `DebugOverlay`
- [x] Update `watch.rs` — replace manual overlay with `DebugOverlay`

### Phase 3: timeline panel

- [ ] `moq-media-egui/src/timeline.rs` — `TimelinePanel`, custom
  painting (RTT line, video/audio frame boxes, axis)
- [ ] `moq-media/src/subscribe.rs` — populate `FrameTiming` on
  receive and decode
- [ ] `moq-media/src/playout.rs` — stamp `render_wall` on playout
- [ ] Feed `TimelineData` from pipeline to egui widget

### Phase 4: polish

- [ ] A/V sync offset measurement (audio playout time vs video render
  time for matching PTS range)
- [ ] Drop/skip counters split by cause (network, decode, render)
- [ ] Adaptive algorithm state visibility (current rendition,
  limitation reason, probe state)
- [ ] Freeze detection (no new frame for >2× expected interval)

## Estimated effort

| Phase | New lines | Modified lines | Crates touched |
|-------|-----------|----------------|----------------|
| 1 | ~350 | ~80 | moq-media, iroh-live, rusty-codecs |
| 2 | ~300 | ~50 | moq-media-egui, examples |
| 3 | ~250 | ~40 | moq-media-egui, moq-media |
| 4 | ~100 | ~30 | moq-media, moq-media-egui |

Total: ~1000 new lines, ~200 modified. Phases are independent after
phase 1 — phases 2 and 3 can be done in parallel.
