# Phase 3b: Jitter Buffer & A/V Sync

## Goal

Smooth playout timing, adaptive latency control, and audio/video synchronization. After this phase, playback survives network jitter without glitches, audio and video stay in lip-sync, and the user can choose between low-latency live mode and reliable delivery mode.

## Prerequisites

- Phase 3a complete (adaptive rendition switching, SubscribeBroadcast signals)

## Context: What hang Already Does

hang's `TrackConsumer` manages **group-level** latency:

- `set_max_latency(duration)` sets a staleness threshold
- `read_frame()` tracks `max_timestamp` (highest PTS seen) and computes `cutoff = max_timestamp + max_latency`
- Pending groups race: if a newer group buffers frames with timestamps past `cutoff`, all older groups are skipped
- First frame in each group is a keyframe — group boundaries are safe skip points
- Groups in `pending: VecDeque<GroupConsumer>` are ordered by sequence number
- Frames within a group are read sequentially via `GroupConsumer::read()`

**What hang does NOT do**:
- No frame-level playout timing (frames are delivered as fast as they arrive)
- No cross-track synchronization (audio and video are independent consumers)
- No adaptive depth (max_latency is set once and stays fixed)
- No jitter measurement

**Our job**: Add frame-level playout discipline on top of hang's group-level management.

## Key Design Decision: Live vs Reliable

Two fundamentally different use cases drive the design:

- **Live**: Real-time conferencing, live streaming. Skip stale data to stay close to
  the sender's clock. A dropped frame is better than accumulating latency. This is what
  `current_frame()` (render-side skip-to-latest) and hang's `max_latency` group-skip
  already approximate — we formalize it.

- **Reliable**: Recordings, file transfer over MoQ, non-interactive playback. Every
  frame must be delivered. Higher latency is acceptable. hang's group-skip is disabled
  (or set very high), and the playout buffer delivers frames in order without skipping.

These map to a `PlayoutMode` enum that replaces the previous Auto/Fixed design:

```rust
pub enum PlayoutMode {
    /// Minimize latency: skip stale groups/frames to stay near real-time.
    /// `max_latency` controls how far behind the sender we tolerate before
    /// skipping. Propagated to hang's `TrackConsumer::set_max_latency()`.
    /// Default: 150ms (matches current DEFAULT_MAX_LATENCY).
    Live { max_latency: Duration },

    /// Deliver every frame in order, no skipping. Latency grows if the
    /// network can't keep up. hang's max_latency set to Duration::MAX.
    Reliable,
}
```

### Configurable DEFAULT_MAX_LATENCY

Currently hardcoded as `const DEFAULT_MAX_LATENCY: Duration = Duration::from_millis(150)`
in `moq-media/src/subscribe.rs:32`. This needs to become configurable:

1. Move into `PlaybackConfig` (or a new `SubscribeConfig`) so callers can set it
2. `PlayoutMode::Live { max_latency }` carries the value
3. Default remains 150ms — good balance for conferencing over typical networks
4. Propagated to `OrderedConsumer::new()` at subscribe time AND updated at runtime
   via `set_max_latency()` when the user changes mode

### Interaction with existing skip mechanisms

| Layer | Live mode | Reliable mode |
|-------|-----------|---------------|
| hang `max_latency` group-skip | Active (e.g. 150ms) | Disabled (`Duration::MAX`) |
| `current_frame()` render-side skip | Active (returns latest) | Still active but buffer rarely has >1 frame |
| PlayoutBuffer frame-level timing | Targets `max_latency` depth | Delivers frames ASAP in PTS order |
| A/V sync | Skip video if behind audio | Wait (never skip) |

## Architecture

```
TrackConsumer::read_frame()     hang layer: group-level latency
         │                      (skips stale groups)
         ▼
    forward_frames()            async → sync bridge
         │
         ▼ mpsc
  ┌──────────────┐
  │ PlayoutBuffer │             frame-level playout timing
  │ (in decoder  │              (holds frames until playout time)
  │  thread)     │
  └──────┬───────┘
         │
    PlayoutClock  ◄────────────── shared between audio + video
    (Arc<Mutex>)                   via .clock() on tracks
         │
         ▼
      decoder
         │
         ▼
    output channel
```

### PlayoutClock

The central shared state for latency control and A/V sync. Both audio and video decoder threads reference the same instance.

```rust
/// Shared playout clock for A/V synchronization and latency control.
///
/// Returned by `.clock()` on AvRemoteTrack, AudioTrack, and VideoTrack.
/// All tracks in a broadcast share the same clock instance.
#[derive(Clone)]
pub struct PlayoutClock {
    inner: Arc<Mutex<PlayoutClockInner>>,
}

struct PlayoutClockInner {
    /// Live vs Reliable mode.
    mode: PlayoutMode,

    /// Observed inter-arrival jitter (EMA-smoothed, RFC 3550 style).
    smoothed_jitter: Duration,

    /// Wall clock ↔ media timestamp mapping, established on first frame.
    base_wall: Option<Instant>,
    base_pts: Option<hang::container::Timestamp>,

    /// Per-track latest playout PTS — for cross-track sync.
    tracks: HashMap<String, TrackSync>,
}

struct TrackSync {
    latest_pts: hang::container::Timestamp,
    last_arrival: Instant,
}

/// Controls playout buffer behavior.
#[derive(Debug, Clone)]
pub enum PlayoutMode {
    /// Minimize latency: skip stale groups/frames to stay near real-time.
    /// `max_latency` controls the staleness threshold (default: 150ms).
    Live { max_latency: Duration },

    /// Deliver every frame in order, no skipping. Accepts higher latency.
    Reliable,
}

impl Default for PlayoutMode {
    fn default() -> Self {
        Self::Live {
            max_latency: Duration::from_millis(150),
        }
    }
}
```

### PlayoutClock API

```rust
impl PlayoutClock {
    pub fn new(mode: PlayoutMode) -> Self { ... }

    // --- User-facing control ---

    /// Set playout mode. In Live mode, propagates max_latency to hang's
    /// TrackConsumer via set_max_latency(). In Reliable mode, sets
    /// hang's max_latency to Duration::MAX (disable group-skip).
    pub fn set_mode(&self, mode: PlayoutMode) { ... }

    /// Current mode.
    pub fn mode(&self) -> PlayoutMode { ... }

    /// Current effective max_latency (Live: user-set value, Reliable: MAX).
    pub fn max_latency(&self) -> Duration { ... }

    /// Current observed jitter (informational, for UI display).
    pub fn jitter(&self) -> Duration { ... }

    // --- Internal, called by decoder threads ---

    /// Record frame arrival. Updates jitter estimate.
    fn observe_arrival(&self, track_id: &str, pts: hang::container::Timestamp) { ... }

    /// When should the frame with this PTS be played out?
    /// Live: base_wall + (pts - base_pts), where base_wall = first_arrival + max_latency.
    /// Reliable: base_wall + (pts - base_pts), where base_wall = first_arrival (no extra buffering).
    fn playout_time(&self, pts: hang::container::Timestamp) -> Instant { ... }

    /// Report that a frame was played out. Updates sync tracking.
    fn report_playout(&self, track_id: &str, pts: hang::container::Timestamp) { ... }

    /// Check if this track is behind the sync target.
    /// Live: may return Skip if behind. Reliable: never returns Skip.
    fn sync_check(&self, track_id: &str, pts: hang::container::Timestamp) -> SyncAction { ... }

    /// Current max_latency value to propagate to hang's TrackConsumer.
    fn current_max_latency(&self) -> Duration { ... }
}

enum SyncAction {
    /// Play this frame normally.
    Play,
    /// Skip this frame — track is behind sync target (Live mode only).
    Skip,
    /// Wait this long before playing — track is ahead.
    Wait(Duration),
}
```

### PlayoutBuffer

A small per-thread buffer between the mpsc channel and the decoder. Holds frames until their playout time arrives.

```rust
/// Per-decoder-thread frame buffer with playout timing.
struct PlayoutBuffer {
    /// Frames waiting for playout, ordered by PTS.
    buffer: BTreeMap<hang::container::Timestamp, hang::Frame>,
    /// Maximum frames to buffer (safety valve).
    max_frames: usize,
    /// Reference to shared clock.
    clock: PlayoutClock,
    /// Track identifier for sync reporting.
    track_id: String,
}

impl PlayoutBuffer {
    fn new(clock: PlayoutClock, track_id: String) -> Self { ... }

    /// Insert a frame received from the transport.
    fn push(&mut self, frame: hang::Frame) {
        self.clock.observe_arrival(&self.track_id, frame.timestamp);
        self.buffer.insert(frame.timestamp, frame);
        // If buffer exceeds max_frames, drop oldest.
        while self.buffer.len() > self.max_frames {
            self.buffer.pop_first();
        }
    }

    /// Pop the next frame ready for playout, or return how long to wait.
    fn pop(&mut self) -> PopResult {
        let Some((&pts, _)) = self.buffer.first_key_value() else {
            return PopResult::Empty;
        };
        let playout = self.clock.playout_time(pts);
        let now = Instant::now();
        if now >= playout {
            let frame = self.buffer.pop_first().unwrap().1;
            match self.clock.sync_check(&self.track_id, pts) {
                SyncAction::Play => {
                    self.clock.report_playout(&self.track_id, pts);
                    PopResult::Frame(frame)
                }
                SyncAction::Skip => PopResult::Skip,
                SyncAction::Wait(d) => {
                    // Put it back.
                    self.buffer.insert(pts, frame);
                    PopResult::Wait(d)
                }
            }
        } else {
            PopResult::Wait(playout - now)
        }
    }
}

enum PopResult {
    /// Decode and output this frame.
    Frame(hang::Frame),
    /// Buffer empty, wait for new frames.
    Empty,
    /// Frame not ready yet, wait this long.
    Wait(Duration),
    /// Frame is behind sync target, skip it (Live mode only).
    Skip,
}
```

## Steps

---

### Step 1: PlayoutClock

**Goal**: Shared clock with mode control, jitter measurement, and sync tracking.

**New file**: `moq-media/src/playout.rs`

**Jitter measurement** (RFC 3550):
```
For each frame arrival:
    expected_interval = frame.pts - prev_frame.pts    (media time between frames)
    actual_interval = arrival_time - prev_arrival_time  (wall time between arrivals)
    jitter_sample = |actual_interval - expected_interval|
    smoothed_jitter = smoothed_jitter + (jitter_sample - smoothed_jitter) / 16
```

**Playout time** calculation:
```
Live mode:
  On first frame: base_wall = now + max_latency, base_pts = frame.pts
  For subsequent: playout_time = base_wall + (frame.pts - base_pts)
  Initial buffering period fills max_latency worth of frames, then plays at real-time.

Reliable mode:
  On first frame: base_wall = now, base_pts = frame.pts
  For subsequent: playout_time = base_wall + (frame.pts - base_pts)
  No extra buffering — frames play as soon as PTS order allows.
```

**Coordination with hang**: `set_mode()` propagates to hang's `TrackConsumer` via
`set_max_latency()`. Live mode passes `max_latency` (e.g. 150ms). Reliable mode
passes `Duration::MAX` to disable group-skip entirely. The adaptation task (from
Phase 3a) polls `clock.current_max_latency()` and propagates changes.

**A/V sync** strategy:
- Both tracks report playout PTS via `report_playout()`
- `sync_check()` compares a track's PTS against the other track's latest PTS
- **Live mode**:
  - Video behind audio by > `max_latency / 2` → `SyncAction::Skip`
  - Video ahead of audio by > `max_latency / 4` → `SyncAction::Wait`
  - Otherwise → `SyncAction::Play`
  - Audio is the sync master (audio skips sound worse than video skips)
- **Reliable mode**:
  - Never returns `Skip` — all frames delivered
  - Wait if ahead, play otherwise
  - Audio still sync master but drift handled by waiting, not skipping

**Tests**:
- Jitter measurement: feed arrivals with known jitter, verify `smoothed_jitter` converges
- Playout timing: verify frames are scheduled at correct wall clock times
- Sync: simulate audio ahead of video, verify video catches up. Vice versa.
- Mode switch: `Live { 150ms }` → `Reliable`, verify hang max_latency goes to MAX
- Mode switch: `Reliable` → `Live { 100ms }`, verify hang max_latency updated
- Live skip: verify stale frames are skipped when behind
- Reliable no-skip: verify NO frames are skipped even under jitter

---

### Step 2: PlayoutBuffer

**Goal**: Per-thread frame buffer with playout discipline.

**File**: `moq-media/src/playout.rs`

**Buffer sizing**:
- `max_frames = 60` (2 seconds at 30fps) — safety valve, should never fill in normal operation
- In practice, buffer holds `target_latency / frame_duration` frames (e.g. 150ms / 33ms ≈ 5 frames)
- Overflow drops oldest frames (they're too late anyway)

**Tests**:
- Push 10 frames with 33ms PTS spacing, pop with clock → frames released at correct intervals
- Push frames with jitter → buffer absorbs, output is smooth
- Overflow: push 100 frames without popping → oldest dropped, no unbounded growth
- Empty pop → `PopResult::Empty`

---

### Step 3: Video Integration

**Goal**: Integrate PlayoutBuffer into VideoTrack's decoder thread.

**Files**: `moq-media/src/subscribe.rs`

Replace current `VideoTrack::run_loop()`:

```rust
fn run_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<hang::Frame>,
    output_tx: mpsc::Sender<DecodedFrame>,
    mut viewport_watcher: Watcher<(u32, u32)>,
    mut decoder: impl VideoDecoder,
    target_pixel_format: PixelFormat,
    clock: PlayoutClock,
    track_id: String,
) -> Result<()> {
    let mut buffer = PlayoutBuffer::new(clock.clone(), track_id);

    loop {
        if shutdown.is_cancelled() { break; }

        // Determine wait time: either next playout or poll interval.
        let timeout = match buffer.peek_wait() {
            Some(wait) => wait.min(Duration::from_millis(5)),
            None => Duration::from_millis(50),  // idle poll
        };

        // Try to receive new frames (non-blocking or with timeout).
        match input_rx.recv_timeout(timeout) {
            Ok(packet) => buffer.push(packet),
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Pop and decode all ready frames.
        loop {
            match buffer.pop() {
                PopResult::Frame(packet) => {
                    if viewport_watcher.update() {
                        let (w, h) = viewport_watcher.peek();
                        decoder.set_viewport(*w, *h);
                    }
                    decoder.push_packet(packet)?;
                    while let Some(frame) = decoder.pop_frame()? {
                        // pixel format conversion...
                        let _ = output_tx.blocking_send(frame);
                    }
                }
                PopResult::Skip => continue,
                PopResult::Wait(_) | PopResult::Empty => break,
            }
        }
    }
    Ok(())
}
```

**Frame freeze**: When no frames arrive, the timeout keeps the loop alive. The UI's `current_frame()` returns the last decoded frame. No explicit freeze logic needed — the output channel simply has no new frames.

**Tests**:
- End-to-end: encode 30 frames → transport → VideoTrack with PlayoutClock → decoded frames arrive with smooth timing
- Jitter: add ±30ms random delay to frame delivery → output is smoother than input
- Gap: inject 200ms gap → no panic, last frame held

---

### Step 4: Audio Integration

**Goal**: Integrate PlayoutBuffer into AudioTrack's decoder thread.

**Files**: `moq-media/src/subscribe.rs`

Replace current `AudioTrack::run_loop()`:

```rust
fn run_loop(
    mut decoder: impl AudioDecoder,
    mut packet_rx: mpsc::Receiver<hang::Frame>,
    mut sink: impl AudioSink,
    shutdown: &CancellationToken,
    clock: PlayoutClock,
    track_id: String,
) -> Result<()> {
    let mut buffer = PlayoutBuffer::new(clock, track_id);
    const INTERVAL: Duration = Duration::from_millis(10);
    let loop_start = Instant::now();

    for i in 0.. {
        if shutdown.is_cancelled() { break; }

        // Drain all available packets into buffer.
        loop {
            match packet_rx.try_recv() {
                Ok(packet) => buffer.push(packet),
                Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => break,
            }
        }

        // Pop and decode all ready frames.
        loop {
            match buffer.pop() {
                PopResult::Frame(packet) => {
                    if !sink.is_paused() {
                        decoder.push_packet(packet)?;
                        if let Some(samples) = decoder.pop_samples()? {
                            sink.push_samples(samples)?;
                        }
                    }
                }
                PopResult::Skip => continue,
                PopResult::Wait(_) | PopResult::Empty => break,
            }
        }

        // Maintain 10ms tick for audio cadence.
        let target = i * INTERVAL;
        let elapsed = Instant::now().duration_since(loop_start);
        let sleep = target.saturating_sub(elapsed);
        if !sleep.is_zero() {
            thread::sleep(sleep);
        }
    }
    Ok(())
}
```

Audio keeps its 10ms tick cadence (needed for smooth audio output). The PlayoutBuffer adds timing discipline without changing the fundamental loop structure.

**Tests**:
- Smooth delivery: packets arrive evenly → audio plays without gaps
- Jittery delivery: ±20ms arrival jitter → audio output stays smooth
- Sync: audio + video PlayoutClock shared → lip-sync maintained

---

### Step 5: Track `.clock()` API

**Goal**: Expose PlayoutClock on all track types for user latency control.

**Files**: `moq-media/src/subscribe.rs`

```rust
impl VideoTrack {
    pub fn clock(&self) -> &PlayoutClock { &self.handle.clock }
}

impl VideoTrackHandle {
    pub fn clock(&self) -> &PlayoutClock { &self.clock }
}

impl AudioTrack {
    pub fn clock(&self) -> &PlayoutClock { &self.clock }
}

impl AvRemoteTrack {
    /// Shared clock for both audio and video.
    pub fn clock(&self) -> &PlayoutClock { &self.clock }
}
```

AvRemoteTrack creates the PlayoutClock and passes it to both tracks. If only audio or only video is present, the clock still works (single-track mode, no sync needed).

**Propagation to hang**: The adaptation task (from Phase 3a) periodically checks `clock.current_max_latency()` and calls `consumer.set_max_latency()` on active TrackConsumers. This keeps hang's group-level skip threshold aligned with our playout target.

---

## Implementation Order

```
Step 1: PlayoutClock ──> Step 2: PlayoutBuffer ──┬──> Step 3: Video integration
                                                  ├──> Step 4: Audio integration
                                                  └──> Step 5: .clock() API
```

All steps are sequential (each builds on the previous).

## Files

| File | Change |
|---|---|
| `moq-media/src/playout.rs` | **New**: `PlayoutClock`, `PlayoutBuffer`, `PlayoutMode`, `SyncAction` |
| `moq-media/src/subscribe.rs` | Integrate buffer into VideoTrack/AudioTrack run loops, add `.clock()` |
| `moq-media/src/lib.rs` | Export `playout` module |

## Verification

After each step:
```sh
cargo build --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-features
cargo fmt --check
```

End-to-end manual verification:
1. Two-party call → plays normally (no regression), check `.clock().jitter()` in UI
2. Add ±30ms jitter with `tc netem` → smooth playback, no glitches
3. Add ±100ms jitter → buffer depth adapts up, smooth but higher latency
4. Remove jitter → buffer depth adapts down within seconds
5. `clock.set_mode(Fixed(Duration::from_millis(50)))` → low latency, may stutter on jitter
6. `clock.set_mode(Fixed(Duration::from_millis(300)))` → smooth, higher latency
7. `clock.set_mode(PlayoutMode::Auto { min: .., max: .. })` → adapts to conditions
8. Clapper test: audio and video stay in lip-sync (< 50ms drift)

## Commits

One commit per step:
- `feat(media): add PlayoutClock with Live/Reliable modes and A/V sync`
- `feat(media): add PlayoutBuffer for frame-level playout timing`
- `feat(media): integrate playout buffer into VideoTrack decoder loop`
- `feat(media): integrate playout buffer into AudioTrack decoder loop`
- `feat(media): expose .clock() API on tracks for latency control`

## Implementation Notes

### Existing Skip Mechanisms to Preserve

There are currently **two** skip-to-latest mechanisms, at different layers:

1. **hang `OrderedConsumer` group-skip** (`moq-media/src/subscribe.rs`):
   - `OrderedConsumer::new(inner, max_latency)` — constructed with `DEFAULT_MAX_LATENCY` (150ms)
   - `read_frame()` computes `cutoff = max_timestamp + max_latency` and skips stale groups
   - Used at lines 198 and 233 in `subscribe.rs` for video and audio track consumers
   - **Change needed**: Accept `max_latency` from `PlayoutMode` instead of hardcoded constant

2. **`current_frame()` render-side skip** (various):
   - Drains the mpsc channel and returns only the newest frame
   - Used by viewer/watch examples and `VideoDecoderPipeline`
   - Cheap last-mile catchup — keep as-is in both modes

### Making DEFAULT_MAX_LATENCY Configurable

Current: `const DEFAULT_MAX_LATENCY: Duration = Duration::from_millis(150)` in `subscribe.rs`.

Plan:
1. Add `playout_mode: PlayoutMode` field to `PlaybackConfig` (or new `SubscribeConfig`)
2. `SubscribeBroadcast::watch_and_listen()` reads mode → passes to `OrderedConsumer::new()`
3. `PlayoutClock::set_mode()` calls `consumer.set_max_latency()` at runtime
4. Default stays `PlayoutMode::Live { max_latency: 150ms }`

### Jitter Measurement Simplification

The original plan had adaptive target_latency that auto-adjusts between min/max based
on jitter. With Live/Reliable, we simplify:

- **Live mode**: `max_latency` is user-set (not auto-adapted). Jitter measurement is
  still useful for UI display and future adaptive bitrate decisions, but doesn't drive
  the buffer depth directly. This is simpler and more predictable.
- **Reliable mode**: Jitter measurement still runs (informational) but doesn't affect
  playout — frames are delivered in order regardless.
- **Future**: Can add `PlayoutMode::Adaptive { min, max }` later if auto-adaptation
  proves valuable. Start simple.

### Initial Buffering ("Priming Period")

In Live mode, the first `max_latency` worth of frames are buffered before playout
starts (the `base_wall = now + max_latency` offset). After this initial fill, playback
runs at real-time. If the stream starts mid-group, the first keyframe may arrive with
a burst of frames — the buffer absorbs this naturally.

The user said they don't care about the first second — the priming period is acceptable.
What matters is lowest latency once live.

### Keyframe Interval Interaction

Keyframe interval (now configurable, default = framerate = 1 GOP/sec) interacts with
group-skip: hang skips entire groups, each starting with a keyframe. Shorter keyframe
intervals = more frequent skip opportunities = lower worst-case latency in Live mode.
For live conferencing, 1-second GOPs are standard. For reliable delivery, GOP size is
irrelevant since nothing is skipped.
