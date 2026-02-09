# Devtools: Egui Examples Refactor & Dev Example

## Goals

1. Extract shared egui code from `rooms.rs` and `watch.rs` into `common_egui` module
2. Create a `dev` example: multi-endpoint splitscreen with rich debug overlay
3. Keep `rooms` and `watch` examples working with shared code

---

## 1. New Module: `examples/common_egui/mod.rs`

Extract from `rooms.rs` and `watch.rs` into shared module:

### `common_egui/video_view.rs` — Video texture rendering

Extracted from both examples (identical logic):

```rust
pub struct VideoView {
    track: WatchTrack,
    size: egui::Vec2,
    texture: egui::TextureHandle,
}
```

Methods: `new(ctx, track, id)`, `set_track(track)`, `render_image(ctx, available_size) -> Image`.

### `common_egui/remote_track_view.rs` — Remote peer video + overlay

Extracted from `rooms.rs`:

```rust
pub struct RemoteTrackView {
    pub id: usize,
    pub video: Option<VideoView>,
    pub session: MoqSession,
    pub broadcast: SubscribeBroadcast,
    pub stats: StatsSmoother,
    _audio_track: Option<AudioTrack>,
}
```

Methods: `new()`, `is_closed()`, `render_image()`, `render_overlay_in_rect()`, `render_overlay()`.

The overlay now includes the rendition selector (kept from original) plus the connection quality dot. The old bandwidth/RTT text display is replaced by the debug overlay system — the simple overlay still shows the rendition ComboBox and quality dot, but detailed stats move to the debug panel.

### `common_egui/grid.rs` — Video grid layout

Extracted from `rooms.rs`:

```rust
pub fn show_video_grid(ctx, ui, videos: &mut [RemoteTrackView])
```

### `common_egui/room_view.rs` — Complete room view widget

New composite widget that encapsulates the full room UI (grid + self-video preview + event handling). This is the key reusable piece for the `dev` example.

```rust
pub struct RoomView {
    room: Room,
    peers: Vec<RemoteTrackView>,
    self_video: Option<VideoView>,
    audio_ctx: AudioBackend,
    label: String,           // "A", "B", "C", ...
}
```

Methods:
- `new(room, broadcast, audio_ctx, label, egui_ctx)` — construct from room + broadcast
- `update(ctx, ui, rt)` — poll room events, render grid + self-preview
- `endpoint_id() -> EndpointId`
- `ticket() -> RoomTicket`
- `is_running() -> bool`

### `common_egui/debug_overlay.rs` — Toggleable debug overlay

Rich debug view with a top bar and expandable detail panel.

```rust
pub struct DebugOverlay {
    visible: bool,                              // detail panel visible
    expanded_sessions: HashSet<String>,         // expanded session sections
    expanded_collections: HashSet<String>,      // expanded catalog/path/buffer sections
}
```

#### Always-visible elements (per tile)

**Top-right corner**: "Debug" toggle button + rendition selector `ComboBox`.

When debug is enabled, a **stats bar** appears below the rendition selector spanning the tile width:

```
┌──────────────────────────────────────────────────────┐
│  [p720 ▼]  [Debug ■]                                │  ← always visible
│  ● 12ms  ↓ 4.5 Mbit/s  ↑ 2.1 Mbit/s  Δ 0.8s       │  ← stats bar (debug on)
│┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄│
│                                                      │
│           (debug detail panel, 0.7 alpha bg)         │
│                                                      │
└──────────────────────────────────────────────────────┘
```

Stats bar shows in big monospace numbers:
- **●** colored dot: connection quality indicator (green/yellow/red, see below)
- **RTT**: e.g. `12ms`
- **↓ BW down**: e.g. `4.5 Mbit/s`
- **↑ BW up**: e.g. `2.1 Mbit/s`
- **Δ frame delay**: time between source timestamp and local receive, e.g. `0.8s`

**Connection quality indicator**: colored dot based on RTT + loss rate:
- Green (●): RTT < 50ms AND loss rate < 1%
- Yellow (●): RTT < 150ms AND loss rate < 5%
- Red (●): RTT >= 150ms OR loss rate >= 5%

Loss rate = `lost_packets / (udp_tx.datagrams + lost_packets)` from selected path stats.

#### Detail panel (toggled by Debug button)

Covers tile area below the stats bar. Background: `Color32::from_rgba_unmultiplied(0, 0, 0, 178)` (0.7 alpha) so video remains faintly visible underneath. Content in a `ScrollArea`:

```
DEBUG — Endpoint A (abcd1234abcd1234)
Ticket: [Copy 📋]

Room ticket: moq1abc...xyzw
Peers: 2 connected

▼ Session: ef56 (connected)
  Remote ID:  ef567890ef567890ef567890ef567890
  Close reason: —

  ▼ Paths (2)
    ● Direct (selected) — 192.168.1.5:4433
      RTT:       12ms
      CWND:      65535
      MTU:       1200
      Lost:      42 pkts / 18.2 KB  (0.3%)
      BW up:     2.1 Mbit/s (5.2 MB total)
      BW down:   4.5 Mbit/s (12.4 MB total)
      Congestion events: 3
      Black holes: 0

    ○ Relay — relay.iroh.network
      RTT:       85ms
      CWND:      32768
      MTU:       1200
      Lost:      12 pkts / 4.1 KB  (0.8%)
      BW up:     0.0 Mbit/s (0.1 MB total)
      BW down:   0.0 Mbit/s (0.2 MB total)
      Congestion events: 1
      Black holes: 0

  ▼ Catalog
    Video: p180 (320x180), p360 (640x360), p720* (1280x720), p1080 (1920x1080)
    Audio: hq
    (* = currently selected)

  ▼ Buffering
    Video rendition: p720
    Viewport: 640x360
    Last frame ts: 14.232s
    Audio peak: 0.42

▶ Session: ab12 (connected)
  [collapsed]
```

#### API for per-path stats

Access via `MoqSession::conn().paths()` → `Watcher<PathInfoList>`. Call `.get()` on the watcher to get current `PathInfoList`, then iterate `.iter()` → `PathInfo`:

- `path_info.remote_addr()` → `TransportAddr` (`.is_ip()` / `.is_relay()`)
- `path_info.is_selected()` → bool
- `path_info.stats()` → `PathStats`:
  - `.rtt: Duration`
  - `.cwnd: u64`
  - `.current_mtu: u16`
  - `.lost_packets: u64`
  - `.lost_bytes: u64`
  - `.congestion_events: u64`
  - `.black_holes_detected: u64`
  - `.udp_tx: UdpStats { datagrams, bytes, ios }`
  - `.udp_rx: UdpStats { datagrams, bytes, ios }`
  - `.sent_plpmtud_probes: u64`
  - `.lost_plpmtud_probes: u64`

Note: `conn().paths()` returns an `impl Watcher` — need to call `.get()` synchronously. The `Watcher` trait has `fn get(&mut self) -> Self::Value`. Store the watcher in `RemoteTrackView` or create it fresh each frame.

#### Ticket copy/paste buttons

In the tile header area (visible for both setup and running states):
- **Copy ticket** button (📋 icon): copies `room.ticket().to_string()` to clipboard via `ui.output_mut(|o| o.copied_text = ticket_string)`
- On setup screen, a **Paste ticket** text field for manual entry as alternative to the dropdown

### `common_egui/env_helpers.rs` — Shared env/key helpers

Extracted from `rooms.rs`:

```rust
pub fn secret_key_from_env() -> Result<iroh::SecretKey>
pub fn topic_id_from_env() -> Result<TopicId>
```

### `common_egui/mod.rs`

```rust
pub mod video_view;
pub mod remote_track_view;
pub mod grid;
pub mod room_view;
pub mod debug_overlay;
pub mod env_helpers;
```

---

## 2. Refactor `rooms.rs`

Replace all extracted code with imports from `common_egui`:

```rust
mod common_egui;
use common_egui::{room_view::RoomView, env_helpers::*, debug_overlay::DebugOverlay};
```

The `App` struct simplifies to:

```rust
struct App {
    room_view: RoomView,
    debug_overlay: DebugOverlay,
    router: Router,
    _broadcast: PublishBroadcast,
    rt: tokio::runtime::Runtime,
}
```

`update()` calls `self.room_view.update(ctx, ui, &self.rt)` + debug toggle.

---

## 3. Refactor `watch.rs`

Replace `VideoView` with import from `common_egui::video_view::VideoView`. Keep its simpler single-stream architecture — it doesn't use `RoomView` since it's not a room.

---

## 4. New Example: `dev.rs`

### 4a. App Structure

```rust
struct DevApp {
    endpoints: Vec<EndpointInstance>,
    next_label: char,  // 'A', 'B', 'C', ...
    rt: tokio::runtime::Runtime,
    audio_ctx: AudioBackend,
}

enum EndpointInstance {
    Setup(SetupScreen),
    Running(RunningEndpoint),
}

struct SetupScreen {
    label: String,
    ticket_source: TicketSource,
    enable_camera: bool,
    enable_mic: bool,
    media_source: MediaSource,
    codec: VideoCodec,
}

enum TicketSource {
    New,                       // Generate fresh room
    FromEndpoint(usize),       // Copy ticket from endpoint at index
}

enum MediaSource {
    Capture,                   // Camera + mic
    Import(ImportFile),        // Video file
}

enum ImportFile {
    SyncTest,                  // Sync-Footage-V1-H264.mp4
    Custom(PathBuf),           // User-provided path
}

struct RunningEndpoint {
    label: String,
    room_view: RoomView,
    debug_overlay: DebugOverlay,
    router: Router,
    _broadcast: PublishBroadcast,
}
```

### 4b. Layout

Main window is divided into tiles with **4px borders** (dark gray `Color32::from_gray(40)`).

Tile layout: same grid algo as `show_video_grid` but for endpoint tiles:
- 1 endpoint: full window
- 2 endpoints: side-by-side (2 cols)
- 3-4 endpoints: 2x2 grid
- 5-6: 3x2, etc.

Below all tiles: a full-width **"+ Add Endpoint"** button (tall, prominent).

### 4c. Setup Screen (per tile)

Rendered inside the tile when `EndpointInstance::Setup`:

```
┌─────────────────────────────────┐
│  Endpoint C                     │
│                                 │
│  Ticket: [New room ▼]           │
│    Options: New / From A / B    │
│                                 │
│  Source:  [Capture ▼]           │
│    Options: Capture / Import    │
│                                 │
│  (if Import):                   │
│    File: [Sync Test ▼]         │
│                                 │
│  [x] Camera  [x] Microphone    │
│                                 │
│  Codec: [H264 ▼]               │
│                                 │
│       [ ▶ Start ]               │
└─────────────────────────────────┘
```

**Ticket selector**: dropdown listing "New room" + all running endpoints by label ("From A — `abcd1234`", "From B — `ef567890`").

**Source selector**: "Capture" (camera+mic) or "Import" (video file).

**Import file selector**: "Sync Test (archive.org)" + "Custom..." (text field for path).

### 4d. Running State (per tile)

Each tile renders its `RoomView` + `DebugOverlay`. The tile has:
- Label badge in top-left: large letter (A/B/C/D) with endpoint ID in small monospace below
- Copy ticket button (📋) next to the label badge
- Top-right: rendition selector ComboBox + Debug toggle button
- When debug enabled: stats bar below top controls (RTT, BW, frame delay in big numbers, connection quality dot)
- When debug detail open: scrollable overlay (0.7 alpha black bg) covering area below stats bar

### 4e. Per-Endpoint Independence

Each endpoint gets its own:
- `Endpoint` (iroh QUIC endpoint with unique secret key)
- `Router`
- `Gossip`
- `Live`
- `Room`
- `PublishBroadcast` (with own capture or import)
- `AudioBackend` (own audio output for playback — `AudioBackend::new(None, None)`)

This means endpoints are fully independent processes that happen to share a window.

### 4f. Start Flow

When "Start" is pressed for a `SetupScreen`:

1. Resolve ticket:
   - `TicketSource::New` → `RoomTicket::new(topic_id_from_env()?, vec![])`
   - `TicketSource::FromEndpoint(i)` → copy `endpoints[i].ticket()`
2. Generate secret key (always fresh, no env)
3. Create endpoint, gossip, live, router
4. Create broadcast:
   - `MediaSource::Capture` → `CameraCapturer` / `AudioRenditions` (same as rooms.rs)
   - `MediaSource::Import` → use `common::import::transcode()` to publish file
5. Create `Room`, publish broadcast
6. Construct `RoomView` + `DebugOverlay`
7. Replace `EndpointInstance::Setup` with `EndpointInstance::Running`

### 4g. Import Source for Dev

For `MediaSource::Import`, we skip camera/mic and instead:
1. Download or use cached video file (the app expects the file to exist locally — print download instructions if missing)
2. Use `common::import::transcode()` + `Import` to pipe through ffmpeg
3. Publish to room as broadcast named "cam" (same as capture)

Default files:
- **Sync Test**: `https://archive.org/details/twitch-sync-footage-v1/Sync-Footage-V1-H264.mp4`

At startup, print a message if file doesn't exist:
```
To use import mode, download test files:
  curl -L -o test-media/sync-test.mp4 "https://archive.org/download/twitch-sync-footage-v1/Sync-Footage-V1-H264.mp4"
```

### 4h. Debug Overlay in Dev Example

See `common_egui/debug_overlay.rs` section above for full spec. In the dev example, each tile gets its own `DebugOverlay` instance. The debug overlay renders per-path connection stats (via `conn().paths().get()` → `PathInfoList`), with loss totals and rates from `PathStats::lost_packets`/`lost_bytes`, connection quality indicator, and catalog/buffering info.

---

## 5. File Changes Summary

### New files
- `examples/common_egui/mod.rs`
- `examples/common_egui/video_view.rs`
- `examples/common_egui/remote_track_view.rs`
- `examples/common_egui/grid.rs`
- `examples/common_egui/room_view.rs`
- `examples/common_egui/debug_overlay.rs`
- `examples/common_egui/env_helpers.rs`
- `examples/dev.rs`

### Modified files
- `examples/rooms.rs` — gut internals, use `common_egui`
- `examples/watch.rs` — use `common_egui::video_view::VideoView`

### Cargo.toml additions
- Add `[[example]] name = "dev"` entry (only if needed — Cargo auto-discovers)
- No new dependencies needed (all egui/iroh deps already in `[dev-dependencies]`)

---

## 6. Implementation Order

1. Create `common_egui/video_view.rs` — extract `VideoView` from rooms.rs
2. Create `common_egui/remote_track_view.rs` — extract `RemoteTrackView`
3. Create `common_egui/grid.rs` — extract `show_video_grid`
4. Create `common_egui/env_helpers.rs` — extract key/topic helpers
5. Create `common_egui/debug_overlay.rs` — new debug overlay widget
6. Create `common_egui/room_view.rs` — composite room widget
7. Create `common_egui/mod.rs` — wire up module
8. Refactor `rooms.rs` to use `common_egui`
9. Refactor `watch.rs` to use `common_egui::video_view`
10. Create `dev.rs` — setup screen + multi-endpoint + debug
11. Verify all three examples compile: `cargo build --examples`
12. Test: run `rooms` and `dev` to verify behavior

---

## Future Work

### A. Network Condition Simulation

Inject artificial latency, packet loss, and bandwidth caps per-endpoint to test resilience.

**Impl plan**: Add a `NetworkCondition { latency_ms: u32, loss_pct: f32, bw_cap_kbps: Option<u32> }` config to `SetupScreen`. On start, wrap the iroh endpoint's UDP socket with a shim that delays/drops/throttles. Expose sliders in debug overlay for live adjustment. ~200 LOC in a `common_egui/net_sim.rs` + UI additions.

### B. Timeline Scrubber & Frame Inspector

Visual timeline showing frame arrival times, decode times, and gaps. Click a frame to inspect its metadata.

**Impl plan**: Add a `FrameLog` ring buffer (last 300 frames) to `RemoteTrackView` storing `(arrival_time, decode_time, timestamp, size_bytes)`. Render as horizontal bar chart in debug overlay. Click selects frame, shows detail panel. ~250 LOC in `common_egui/timeline.rs`.

### C. Recording & Playback

Record a session's raw MOQ streams to disk, replay them without network.

**Impl plan**: Add `SessionRecorder` that wraps `TrackConsumer` and writes encoded packets + timestamps to a file. Add `SessionPlayer` that reads the file and feeds a `BroadcastProducer`. UI: "Record" toggle button per session in debug overlay, "Replay" option in import source selector. ~300 LOC across `common_egui/recorder.rs` + UI.

### D. Side-by-Side A/B Comparison Mode

Split a single tile into two halves showing same stream at different renditions for visual quality comparison.

**Impl plan**: Add `CompareView` that holds two `VideoView`s subscribed to different renditions of the same broadcast. Render side-by-side with a draggable split divider. Dropdown to select two renditions. ~150 LOC in `common_egui/compare_view.rs`.

### E. Automated Test Scenarios

Script-driven test sequences: "start 4 endpoints, connect all to same room, verify all see each other within 5s, check RTT < 100ms".

**Impl plan**: Add a `TestScript` enum with steps (`AddEndpoint`, `WaitConnected(n)`, `AssertRtt(max_ms)`, `AssertPeers(count)`, `Sleep(ms)`). Parse from a simple text format. Run in background, report pass/fail in overlay. ~200 LOC in `common_egui/test_runner.rs`.

### F. Prometheus/Metrics Export

Expose all stats as Prometheus metrics on a local HTTP endpoint for grafana dashboards.

**Impl plan**: Add optional `metrics` feature with `prometheus` crate. In `RoomView::update()`, push gauge values for RTT, bandwidth, frame rate, peer count. Spawn hyper server on `localhost:9090/metrics`. ~100 LOC in `common_egui/metrics.rs` + Cargo.toml feature.

### G. Audio Waveform Visualization

Show real-time audio waveform/spectrum for each peer's audio track in the debug overlay.

**Impl plan**: Tap into `AudioSinkHandle` to get PCM samples. Compute RMS per 10ms window, store in ring buffer. Render as waveform plot using egui `plot` or manual line drawing. ~200 LOC in `common_egui/audio_viz.rs`.

### H. Adaptive Bitrate Visualization

Show which rendition is selected over time and why (bandwidth-triggered switches).

**Impl plan**: Log rendition switch events with timestamp + reason to a `Vec<(Instant, String, String)>`. Render as color-coded horizontal bar in debug overlay (green=1080p, yellow=720p, orange=360p, red=180p). ~100 LOC addition to `debug_overlay.rs`.
