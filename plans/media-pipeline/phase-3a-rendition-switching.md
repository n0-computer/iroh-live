# Phase 3a: Adaptive Rendition Switching

## Goal

WatchTrack and AudioTrack become catalog-aware and switch renditions on-demand. Network signals (RTT, loss, bandwidth) are injected via `SubscribeBroadcast` from the transport layer. Rendition selection inspects catalog metadata (resolution, bitrate, codec), not labels. Switching is seamless: new decoder starts in parallel, output swaps on first decoded frame, then the old decoder stops.

## Prerequisites

- Phase 1 complete (Opus + H.264 codecs)
- Phase 2 complete (AV1 codec support)
- Renditions already produced on-demand by publisher (`AudioRenditions`, `VideoRenditions`)

## Architecture

```
iroh-live                                  moq-media
┌─────────────────────────┐    ┌────────────────────────────────────────────────┐
│ spawn_signal_producer() │    │                                                │
│ (polls conn.stats()     │    │ SubscribeBroadcast                             │
│  every 200ms)           │───>│   signals: Option<Watcher<NetworkSignals>>     │
└─────────────────────────┘    │   │                                            │
                               │   ├──> WatchTrack                              │
                               │   │    .set_rendition(Auto / Fixed)            │
                               │   │    .rendition_mode() -> RenditionMode      │
                               │   │    .selected_rendition() -> VideoRendition │
                               │   │                                            │
                               │   └──> AudioTrack                              │
                               │        .set_rendition(Auto / Fixed)            │
                               │        .rendition_mode() -> RenditionMode      │
                               │        .selected_rendition() -> AudioRendition │
                               │                                                │
                               │ AvRemoteTrack                                  │
                               │   .set_rendition(Auto / Fixed)                 │
                               │     → forwards to both tracks                  │
                               └────────────────────────────────────────────────┘
```

**Key constraint**: `moq-media` does not depend on `iroh`. Network signals are injected as `Option<Watcher<NetworkSignals>>` on `SubscribeBroadcast::new()`. If `None`, auto-switching never triggers and `Auto` picks middle quality.

## Steps

Each step is independently committable with tests passing.

---

### Step 1: NetworkSignals + Rendition Types

**Goal**: Define signal struct, rendition wrapper types, and the rendition mode enum.

**Files**: `moq-media/src/net.rs` (new), `moq-media/src/rendition.rs` (new)

#### NetworkSignals

```rust
/// Transport-level network quality signals, injected from the transport layer.
/// moq-media is transport-agnostic; iroh-live produces these from Connection::stats().
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkSignals {
    /// Smoothed round-trip time.
    pub rtt: Duration,
    /// Packet loss rate (0.0–1.0), computed from delta(lost_packets) / delta(sent_packets).
    pub loss_rate: f64,
    /// Estimated available bandwidth in bits/sec (cwnd * 8 / rtt_seconds).
    pub available_bps: u64,
    /// True if congestion_events counter is increasing.
    pub congested: bool,
}
```

#### RenditionMode

```rust
/// Controls which rendition a track subscribes to.
#[derive(Debug, Clone)]
pub enum RenditionMode {
    /// Automatically select based on network signals and catalog metadata.
    /// If no signals are available, picks middle quality.
    Auto,
    /// Pin to a specific rendition by its catalog key.
    Fixed(String),
}
```

#### Rendition wrapper types

```rust
/// A video rendition resolved from the catalog.
/// Wraps the catalog key and VideoConfig for inspection.
#[derive(Debug, Clone)]
pub struct VideoRendition {
    key: String,
    config: VideoConfig,
}

impl VideoRendition {
    /// The catalog key (e.g. "video-720p").
    pub fn id(&self) -> &str { &self.key }

    /// Short human-readable label: "h264 1280x720" or "av1 640x360".
    pub fn label(&self) -> String {
        let codec = &self.config.codec;
        match (self.config.coded_width, self.config.coded_height) {
            (Some(w), Some(h)) => format!("{codec} {w}x{h}"),
            _ => match self.config.bitrate {
                Some(bps) => format!("{codec} {bps}bps"),
                None => format!("{codec} {}", self.key),
            },
        }
    }

    pub fn config(&self) -> &VideoConfig { &self.config }
}

/// An audio rendition resolved from the catalog.
#[derive(Debug, Clone)]
pub struct AudioRendition {
    key: String,
    config: AudioConfig,
}

impl AudioRendition {
    /// The catalog key (e.g. "audio-hq").
    pub fn id(&self) -> &str { &self.key }

    /// Short human-readable label: "opus 128kbps" or "opus 32kbps".
    pub fn label(&self) -> String {
        let codec = &self.config.codec;
        match self.config.bitrate {
            Some(bps) => format!("{codec} {}kbps", bps / 1000),
            None => format!("{codec} {}", self.key),
        }
    }

    pub fn config(&self) -> &AudioConfig { &self.config }
}
```

**Tests**: Unit tests for `label()` and `id()` with various config combinations (missing dimensions, missing bitrate, etc.).

---

### Step 2: Rendition Selection Traits

**Goal**: Define traits for selecting renditions based on catalog inspection and network signals. Provide default implementations aligned with industry practices (WebRTC, Zoom).

**File**: `moq-media/src/rendition.rs`

#### Traits

```rust
/// Selects a video rendition based on catalog metadata and network signals.
/// Implementations inspect VideoConfig fields (coded_width, coded_height, bitrate, codec)
/// rather than matching on rendition labels.
pub trait VideoRenditionSelector: Send + 'static {
    /// Called when signals or catalog change.
    /// Returns the rendition key to switch to, or None to keep current.
    fn select(
        &mut self,
        current: &str,
        renditions: &BTreeMap<String, VideoConfig>,
        signals: &NetworkSignals,
    ) -> Option<String>;
}

/// Selects an audio rendition based on catalog metadata and network signals.
pub trait AudioRenditionSelector: Send + 'static {
    fn select(
        &mut self,
        current: &str,
        renditions: &BTreeMap<String, AudioConfig>,
        signals: &NetworkSignals,
    ) -> Option<String>;
}
```

#### Default: `AdaptiveVideoSelector`

Bandwidth-primary selection with asymmetric timers. Aligned with WebRTC/Zoom practices: bandwidth estimation drives decisions, loss rate triggers emergency downgrades, RTT feeds into bandwidth estimation (not used directly for selection).

```rust
pub struct AdaptiveVideoSelector {
    /// Sustained good conditions required before upgrading.
    upgrade_hold: Duration,
    /// Brief hold before downgrading (avoid flapping on transient dips).
    downgrade_hold: Duration,
    /// Tracks when upgrade candidate was first identified.
    upgrade_candidate_since: Option<Instant>,
    /// Tracks when downgrade candidate was first identified.
    downgrade_candidate_since: Option<Instant>,
}

impl Default for AdaptiveVideoSelector {
    fn default() -> Self {
        Self {
            upgrade_hold: Duration::from_secs(4),
            downgrade_hold: Duration::from_millis(500),
            upgrade_candidate_since: None,
            downgrade_candidate_since: None,
        }
    }
}
```

**Selection algorithm** (modeled after WebRTC GCC/REMB and Zoom's simulcast switching):

1. **Rank renditions** by quality: `pixel_count` (w×h) descending, fall back to `bitrate` descending.
2. **Estimate per-rendition bitrate**: use `VideoConfig::bitrate` if set, otherwise approximate from pixel count (2 bits/pixel × framerate, default 30fps).
3. **Loss-based emergency downgrade**: if `loss_rate > 0.10` (sustained for `downgrade_hold`) → drop to next-lower rendition. If `loss_rate > 0.20` → drop to lowest immediately (no hold). This matches WebRTC's approach where >10% loss triggers quality reduction.
4. **Bandwidth-based downgrade**: if current rendition's estimated bitrate > 85% of `available_bps` (sustained for `downgrade_hold`) → next-lower rendition. The 85% threshold leaves headroom for protocol overhead and transient spikes.
5. **Bandwidth-based upgrade**: if next-higher rendition's estimated bitrate < 70% of `available_bps` (sustained for `upgrade_hold`) → upgrade. The 1.4× headroom requirement (70% = 1/1.4) prevents oscillation, matching the 1.2–1.5× probe margin used in WebRTC and Zoom.
6. **Congestion hold**: if `congested` is true → reset upgrade timer, hold current. Never upgrade during active congestion.
7. **Loss recovery hysteresis**: after a loss-triggered downgrade, require `loss_rate < 0.02` sustained for `upgrade_hold` before considering upgrade. The 2% threshold matches WebRTC's "good conditions" baseline.

**What about RTT?** RTT is not used directly for rendition selection. It feeds into bandwidth estimation (`available_bps = cwnd * 8 / rtt`), which is the primary selection signal. This matches industry practice: RTT spikes cause `available_bps` to drop, which triggers bandwidth-based rules above.

#### Default: `AdaptiveAudioSelector`

Simpler: ranks by `AudioConfig::bitrate`. Downgrades when `available_bps < 100kbps` or `loss_rate > 0.10`. Upgrades after sustained good conditions (same asymmetric timing as video).

**Tests**: Mock catalogs + synthetic signals → verify selection, hysteresis, loss triggers, upgrade probing, congestion hold.

---

### Step 3: SubscribeBroadcast Signals

**Goal**: `SubscribeBroadcast::new()` accepts an optional network signals watcher. Tracks created from it automatically inherit the signals.

**Files**: `moq-media/src/subscribe.rs`

```rust
impl SubscribeBroadcast {
    pub async fn new(
        broadcast_name: String,
        broadcast: BroadcastConsumer,
        signals: Option<Watcher<NetworkSignals>>,  // NEW
    ) -> Result<Self> { ... }

    /// Access the network signals watcher, if any.
    pub fn signals(&self) -> Option<&Watcher<NetworkSignals>> { ... }
}
```

All existing callers pass `None` for backwards compatibility. `iroh-live` passes the watcher from `spawn_signal_producer()`.

**iroh-live side** (`iroh-live/src/util.rs`):

```rust
/// Spawns a task that polls conn.stats() every 200ms and publishes NetworkSignals.
pub fn spawn_signal_producer(
    conn: &Connection,
    shutdown: CancellationToken,
) -> Watcher<NetworkSignals> {
    let watchable = Watchable::new(NetworkSignals::default());
    let watcher = watchable.watch();
    let conn = conn.clone();
    tokio::spawn(async move {
        let mut prev_lost = 0u64;
        let mut prev_sent = 0u64;
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    let stats = conn.stats();
                    let path = &stats.path;
                    let delta_lost = path.lost_packets.saturating_sub(prev_lost);
                    let delta_sent = path.sent_packets.saturating_sub(prev_sent);
                    let loss_rate = if delta_sent > 0 {
                        delta_lost as f64 / delta_sent as f64
                    } else { 0.0 };
                    let rtt_secs = path.rtt.as_secs_f64();
                    let available_bps = if rtt_secs > 0.0 {
                        (path.cwnd as f64 * 8.0 / rtt_secs) as u64
                    } else { 0 };
                    prev_lost = path.lost_packets;
                    prev_sent = path.sent_packets;
                    watchable.set(NetworkSignals {
                        rtt: path.rtt,
                        loss_rate,
                        available_bps,
                        congested: path.congestion_events > 0 && delta_lost > 0,
                    }).ok();
                }
            }
        }
    });
    watcher
}
```

**iroh-live integration** (`iroh-live/src/live.rs`):

```rust
pub async fn connect_and_subscribe(
    &self,
    remote: impl Into<EndpointAddr>,
    broadcast_name: &str,
) -> Result<(MoqSession, SubscribeBroadcast)> {
    let mut session = self.connect(remote).await?;
    let signals = spawn_signal_producer(session.conn(), ...);
    let broadcast = session.subscribe(broadcast_name).await?;
    let broadcast = SubscribeBroadcast::new(
        broadcast_name.to_string(),
        broadcast,
        Some(signals),  // auto-inject
    ).await?;
    Ok((session, broadcast))
}
```

---

### Step 4: WatchTrack Rendition Switching

**Goal**: WatchTrack switches its active rendition on-demand. Output channel stays stable. Switching is seamless: new decoder starts before old stops.

**Files**: `moq-media/src/subscribe.rs`

#### Design

WatchTrack holds `SubscribeBroadcast` internally, enabling it to create new consumers for different renditions:

```rust
pub struct WatchTrack {
    video_frames: WatchTrackFrames,     // stable output — rx never changes
    handle: WatchTrackHandle,
}

pub struct WatchTrackHandle {
    selected_rendition: Arc<Mutex<VideoRendition>>,
    rendition_mode: Arc<Mutex<RenditionMode>>,
    viewport: Watchable<(u32, u32)>,
    mode_tx: mpsc::Sender<RenditionMode>,
    _guard: WatchTrackGuard,
}

pub struct WatchTrackFrames {
    rx: mpsc::Receiver<DecodedFrame>,   // never replaced across switches
}
```

#### Seamless switching

The adaptation task manages decoder thread lifecycle. On rendition change, old and new decoders run in parallel briefly:

```
  signals watcher ───>┌──────────────────────────────────┐
  catalog watcher ───>│      adaptation task              │
  mode_tx         ───>│      (tokio task)                 │
                      │                                    │
                      │ on rendition change:               │
                      │ 1. resolve target rendition        │
                      │ 2. new TrackConsumer               │
                      │ 3. new decoder thread              │
                      │    writes to staging_tx            │
                      │ 4. await first frame on staging_rx │
                      │ 5. forward frame to output_tx      │
                      │ 6. cancel old decoder thread       │
                      │ 7. redirect new decoder → output_tx│
                      │ 8. update selected_rendition       │
                      └────────────────┬───────────────────┘
                                       │
                                  output_tx (stable)
                                       │
                                  ┌────▼────┐
                                  │ rx      │ (WatchTrackFrames)
                                  └─────────┘
```

The staging channel (`staging_tx`/`staging_rx`) is a temporary mpsc used only during the switch. The new decoder writes to it until the adaptation task receives the first frame. That first frame is forwarded to `output_tx`, the old decoder is cancelled, and the new decoder is rewired to write directly to `output_tx`. This ensures the consumer sees no gap — the last frame from the old decoder is followed immediately by the first frame from the new decoder.

On `RenditionMode::Auto` without signals → pick middle quality from catalog and stay.
On `RenditionMode::Auto` with signals → use `VideoRenditionSelector` to adapt.
On `RenditionMode::Fixed(key)` → subscribe to that specific rendition.

#### WatchTrackHandle API

```rust
impl WatchTrackHandle {
    /// Set rendition mode: Auto (adaptive) or Fixed (pinned).
    pub fn set_rendition(&self, mode: RenditionMode) {
        self.mode_tx.try_send(mode).ok();
    }

    /// Current rendition mode (Auto or Fixed).
    pub fn rendition_mode(&self) -> RenditionMode {
        self.rendition_mode.lock().unwrap().clone()
    }

    /// Currently playing video rendition (id, label, config).
    pub fn selected_rendition(&self) -> VideoRendition {
        self.selected_rendition.lock().unwrap().clone()
    }

    pub fn set_viewport(&self, w: u32, h: u32) { ... }
}
```

#### Constructors

```rust
impl WatchTrack {
    /// Create from consumer (current behavior, no switching).
    pub(crate) fn from_consumer<D: VideoDecoder>(...) -> Result<Self> { ... }

    /// Create with rendition switching support.
    pub(crate) fn adaptive<D: VideoDecoder>(
        broadcast: SubscribeBroadcast,
        initial_mode: RenditionMode,
        playback_config: &DecodeConfig,
        shutdown: CancellationToken,
        selector: Box<dyn VideoRenditionSelector>,
    ) -> Result<Self> { ... }
}
```

**Tests**:
- 2 renditions in catalog. `set_rendition(Fixed("video-360p"))`. Verify frames from new rendition (check resolution).
- Seamless switch: verify no gap in output frames during switch (last old frame timestamp < first new frame timestamp, no duplicates).
- Adaptive with selector: bad signals → downgrade. Good signals → upgrade after hysteresis.

---

### Step 5: AudioTrack Rendition Switching

**Goal**: AudioTrack switches its active rendition on-demand. Same seamless switching pattern as WatchTrack.

**Files**: `moq-media/src/subscribe.rs`

Same adaptation task pattern as WatchTrack, with a staging channel for seamless switching:

```rust
pub struct AudioTrack {
    selected_rendition: Arc<Mutex<AudioRendition>>,
    rendition_mode: Arc<Mutex<RenditionMode>>,
    handle: Box<dyn AudioSinkHandle>,
    mode_tx: mpsc::Sender<RenditionMode>,
    shutdown_token: CancellationToken,
    _adaptation_task: AbortOnDropHandle<()>,
}
```

AudioTrack needs `AudioSink` to be re-creatable across switches. Add `cloned_boxed()`:

```rust
pub trait AudioSink: AudioSinkHandle {
    fn format(&self) -> Result<AudioFormat>;
    fn push_samples(&mut self, buf: &[f32]) -> Result<()>;
    fn handle(&self) -> Box<dyn AudioSinkHandle>;
    fn cloned_boxed(&self) -> Box<dyn AudioSink>;  // NEW
}
```

#### Seamless audio switching

Same staging pattern: new decoder thread writes to a staging channel. Adaptation task waits for first decoded audio samples from the new decoder, forwards them to the sink, cancels old decoder, then rewires new decoder to write directly to the sink. Audio output stays continuous.

#### API

```rust
impl AudioTrack {
    pub fn set_rendition(&self, mode: RenditionMode) {
        self.mode_tx.try_send(mode).ok();
    }

    /// Current rendition mode (Auto or Fixed).
    pub fn rendition_mode(&self) -> RenditionMode {
        self.rendition_mode.lock().unwrap().clone()
    }

    /// Currently playing audio rendition (id, label, config).
    pub fn selected_rendition(&self) -> AudioRendition {
        self.selected_rendition.lock().unwrap().clone()
    }
}
```

**Tests**: 2 renditions. Switch. Verify audio continues without gap. Adaptive: high loss → LQ, restore → HQ after hysteresis.

---

### Step 6: AvRemoteTrack Integration

**Goal**: AvRemoteTrack provides high-level `set_rendition()` that forwards to both tracks, plus per-track control.

**Files**: `moq-media/src/subscribe.rs`, `iroh-live/src/live.rs`

```rust
impl AvRemoteTrack {
    pub fn new<D: Decoders>(
        broadcast: SubscribeBroadcast,
        audio_out: impl AudioSink,
        playback_config: PlaybackConfig,
    ) -> Result<Self> { ... }

    /// Set rendition mode for both audio and video.
    pub fn set_rendition(&self, mode: RenditionMode) {
        if let Some(video) = &self.video {
            video.handle.set_rendition(mode.clone());
        }
        if let Some(audio) = &self.audio {
            audio.set_rendition(mode);
        }
    }

    /// Set video rendition mode independently.
    pub fn set_video_rendition(&self, mode: RenditionMode) { ... }

    /// Set audio rendition mode independently.
    pub fn set_audio_rendition(&self, mode: RenditionMode) { ... }
}
```

Signals live on `SubscribeBroadcast`, so the existing constructor works for both adaptive and non-adaptive cases. The presence of signals on the broadcast determines behavior.

**Tests**: Integration test — AvRemoteTrack with signals, verify both tracks switch together.

---

## Implementation Order

```
Step 1: NetworkSignals + Rendition types ──┐
                                           ├──> Step 3: SubscribeBroadcast signals
Step 2: Selection traits                 ──┘         │
                                                Step 4: WatchTrack switching ──┐
                                                Step 5: AudioTrack switching ──┼──> Step 6: AvRemoteTrack
                                                                               │
                                                                          iroh-live wiring
```

## Files

| File | Change |
|---|---|
| `moq-media/src/net.rs` | **New**: `NetworkSignals` struct |
| `moq-media/src/rendition.rs` | **New**: `RenditionMode`, `VideoRendition`, `AudioRendition`, selection traits, default selectors |
| `moq-media/src/subscribe.rs` | `SubscribeBroadcast` signals arg, WatchTrack/AudioTrack adaptive switching with seamless decoder handoff |
| `moq-media/src/av.rs` | `AudioSink::cloned_boxed()` addition |
| `moq-media/src/lib.rs` | Export new modules |
| `iroh-live/src/live.rs` | `connect_and_subscribe` injects signals automatically |
| `iroh-live/src/util.rs` | `spawn_signal_producer()` |
| `iroh-live/examples/watch.rs` | Show rendition label in UI |
| `iroh-live/examples/rooms.rs` | Show rendition labels |

## Verification

After each step:
```sh
cargo build --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-features
cargo fmt --check
```

End-to-end manual verification:
1. Two-party call → video/audio plays normally (no regression)
2. Throttle network to 200kbps → auto-switches to lowest renditions within 1s
3. Remove throttle → upgrades back within 5s (sustained headroom required)
4. `set_rendition(Fixed("video-180p"))` → immediate seamless switch
5. `set_rendition(Auto)` → resumes adaptive behavior
6. `rendition_mode()` returns correct mode, `selected_rendition().label()` shows correct info
7. During switch: no visible frame gap or audio glitch

## Commits

One commit per step:
- `feat(media): add NetworkSignals, RenditionMode, and rendition wrapper types`
- `feat(media): add rendition selection traits with bandwidth-primary default selectors`
- `feat(media): inject optional network signals into SubscribeBroadcast`
- `feat(media): add seamless rendition switching to WatchTrack`
- `feat(media): add seamless rendition switching to AudioTrack`
- `feat(media): wire rendition switching into AvRemoteTrack and iroh-live`
