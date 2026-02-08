# Phase 4: Video Resilience (Revised)

## Context

iroh-live's video pipeline has zero congestion awareness — encoders run at fixed bitrate, `producer.write()` is fire-and-forget, decoders drain frames with no buffering or timing, and there's no feedback channel between subscriber and publisher. For iroh-live to be a viable WebRTC/LiveKit replacement, we need transport-aware rate control, adaptive playback, and graceful degradation.

**Key architectural advantage over WebRTC**: QUIC handles congestion control (Cubic/BBR), reliable delivery, and encryption natively. We don't need to reimplement GCC, NACK/RTX, FEC, or SRTP. Instead, we read QUIC's own bandwidth estimate from `PathStats` (cwnd, rtt, lost_packets) and let the transport handle the rest.

**Available signals** from `iroh::endpoint::ConnectionStats::path: PathStats`:
- `cwnd: u64` — congestion window in bytes
- `rtt: Duration` — smoothed round-trip time
- `congestion_events: u64` — counter
- `lost_packets: u64`, `lost_bytes: u64` — loss counters
- `current_mtu: u16`

**Key constraint**: Encoder threads (OS threads) have no access to `Connection`. Communication from the async runtime must use `watch` channels.

## Sub-phases

Each sub-phase is independently committable with tests passing.

---

### 4a: Encoder Runtime Control (P0)

**Goal**: Add `set_bitrate()` and `force_keyframe()` to encoder trait and all backends.

**Files**:
- `moq-media/src/av.rs` — extend `VideoEncoderInner` trait
- `moq-media/src/codec/video/encoder.rs` — H264Encoder (OpenH264)
- `moq-media/src/codec/video/rav1e_enc.rs` — Av1Encoder
- `moq-media/src/codec/video/vtb_enc.rs` — VtbEncoder
- `moq-media/src/codec/video/vaapi_enc.rs` — VaapiEncoder

**Trait additions** (with default impls for backward compat):
```rust
fn set_bitrate(&mut self, bitrate_bps: u64) -> Result<()> { Ok(()) }
fn force_keyframe(&mut self);  // no default — all encoders must impl
```

**Per-backend**:
| Backend | set_bitrate | force_keyframe |
|---------|-------------|----------------|
| H264 (OpenH264) | `raw_api.set_option(ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, ...)` with updated `SEncParamExt` | `raw_api.force_intra_frame(true)` |
| AV1 (rav1e) | Flush + recreate `Context` with new `EncoderConfig.bitrate` at next keyframe boundary | `ctx.flush()` triggers new keyframe sequence |
| VTB | `VTSessionSetProperty(kVTCompressionPropertyKey_AverageBitRate, ...)` | `kVTEncodeFrameOptionKey_ForceKeyFrame` in frame properties |
| VAAPI | `Tunings` with `ConstantBitrate(new_value)` via `tune()` | `FrameMetadata.force_keyframe = true` |

Also forward through `Box<dyn VideoEncoder>` impl in av.rs.

**Tests**: Per encoder: encode 30 frames, call `set_bitrate(low)`, encode 30 more, verify packet sizes decrease. Encode 10 frames, `force_keyframe()`, verify next packet is keyframe.

---

### 4b: Bandwidth Estimator (P0)

**Goal**: Read QUIC path stats, produce a bandwidth estimate via `watch` channel.

**New file**: `moq-media/src/bandwidth.rs`

**Design**:
```rust
pub struct BandwidthEstimate {
    pub available_bps: u64,   // estimated available bandwidth
    pub rtt: Duration,
    pub loss_rate: f64,       // 0.0–1.0, computed from lost_packets delta
    pub congested: bool,      // true if congestion_events increasing
}

pub struct BandwidthEstimator { ... }

impl BandwidthEstimator {
    pub fn new() -> (Self, watch::Receiver<BandwidthEstimate>);
    pub fn update(&mut self, stats: &ConnectionStats);
}
```

**Algorithm**: Called every 200ms from async runtime.
- `available_bps = cwnd * 8 / rtt_seconds` (BDP formula — QUIC's own estimate)
- EMA smoothing (alpha=0.3) to avoid oscillation
- `loss_rate` = delta(lost_packets) / delta(total_packets) over interval
- `congested` = `congestion_events` counter increased since last sample

This does NOT reimplement congestion control — it reads what QUIC has already computed. The cwnd/rtt ratio IS the bandwidth estimate.

**Integration point**: `BandwidthEstimator::update()` is called from a tokio task that has access to `session.conn().stats()`. The `watch::Receiver` is cloned to encoder threads.

**Tests**: Unit tests with synthetic `ConnectionStats` sequences: stable network, congestion event, recovery, high loss.

---

### 4c: Transport-Aware Rate Control (P0)

**Goal**: Wire bandwidth estimate to encoder threads for real-time bitrate adaptation.

**Files**:
- `moq-media/src/publish.rs` — thread bandwidth receiver into `EncoderThread::spawn_video()`
- `moq-media/src/av.rs` — add `EncoderControl` struct

**Design**:
```rust
// Passed to encoder thread at creation
pub struct EncoderControl {
    pub bandwidth_rx: watch::Receiver<BandwidthEstimate>,
    pub keyframe_rx: mpsc::Receiver<()>,
}
```

In the encoder loop, before `push_frame()`:
1. Check `bandwidth_rx.has_changed()` — if yes, compute target bitrate
2. Target = `available_bps * 0.85` (leave 15% for audio + protocol overhead)
3. Clamp to preset-specific [min, max] range
4. Call `encoder.set_bitrate(target)` if changed by >5% (debounce)
5. Check `keyframe_rx.try_recv()` — if message received, call `force_keyframe()`

**Bitrate bounds per preset**:
| Preset | Min | Max |
|--------|-----|-----|
| 180p | 50 kbps | 500 kbps |
| 360p | 100 kbps | 1.5 Mbps |
| 720p | 300 kbps | 3 Mbps |
| 1080p | 500 kbps | 6 Mbps |

**Multi-rendition allocation**: When multiple renditions are active, total bandwidth is split proportionally by pixel count. If budget drops below a rendition's minimum, that rendition gets its minimum (audio always gets its full budget first).

**Wiring from the top**: `PublishBroadcast` gains `set_encoder_control(EncoderControl)`. Examples/apps create `BandwidthEstimator` from `session.conn()`, spawn a 200ms ticker task, and pass the receiver.

**Tests**: Integration test with mock bandwidth signal — verify encoder output bitrate tracks the signal.

---

### 4d: Keyframe Request Channel (P0)

**Goal**: Subscriber can request immediate keyframe from publisher for mid-stream recovery.

**Files**:
- `moq-media/src/publish.rs` — receive keyframe requests, dispatch to encoder
- `moq-media/src/subscribe.rs` — send keyframe requests on decoder error

**Approach**: Use the `mpsc::Sender<()>` in `EncoderControl.keyframe_rx` for the encoder-thread side. The application-level signaling depends on topology:

- **Direct P2P**: Application sends a custom message over a side-channel (iroh already has gossip/messaging). This is outside moq-media's scope.
- **Within moq-media**: `PublishBroadcast` exposes `request_keyframe(track_name: &str)` which sends to the matching encoder thread's `keyframe_rx`.
- **Automatic**: Decoder thread calls `request_keyframe()` when `push_packet()` returns a decode error (corrupted reference frame).

**MoQ groups already handle initial join** — new subscribers start at the latest group (keyframe). This mechanism is for mid-stream recovery only.

**Tests**: Encode 30 frames, send keyframe request via channel, verify encoder produces keyframe within next 2 frames.

---

### 4e: Video Jitter Buffer & Frame Timing (P1)

**Goal**: Absorb network jitter with an adaptive buffer; smooth playout timing.

**New file**: `moq-media/src/jitter.rs`
**Modify**: `moq-media/src/subscribe.rs` — `WatchTrack::run_loop()`

**Design**:
```rust
pub struct VideoJitterBuffer {
    buffer: BTreeMap<Timestamp, hang::Frame>,
    target_depth: Duration,       // adaptive: 20–200ms
    min_depth: Duration,          // 20ms
    max_depth: Duration,          // 200ms
    smoothed_jitter: Duration,    // EMA of inter-arrival jitter
    playout_clock: PlayoutClock,  // monotonic playout position
}

impl VideoJitterBuffer {
    fn insert(&mut self, frame: hang::Frame);
    fn pop_ready(&mut self) -> Option<hang::Frame>;  // returns frame if playout time reached
    fn next_release_time(&self) -> Option<Instant>;
}
```

**Jitter measurement**: RFC 3550 style — track difference between expected and actual inter-arrival times. EMA smooth with alpha=0.15. Set `target_depth = smoothed_jitter * 2 + 10ms`.

**Integration into `run_loop()`**: Replace `input_rx.blocking_recv()` with `recv_timeout()` driven by jitter buffer's next release time.

```rust
loop {
    let timeout = jitter_buffer.next_release_time()
        .map(|t| t.saturating_duration_since(Instant::now()))
        .unwrap_or(Duration::from_millis(100));

    match input_rx.recv_timeout(timeout) {
        Ok(packet) => jitter_buffer.insert(packet),
        Err(RecvTimeoutError::Timeout) => {},
        Err(RecvTimeoutError::Disconnected) => break,
    }

    while let Some(packet) = jitter_buffer.pop_ready() {
        decoder.push_packet(packet)?;
        while let Some(frame) = decoder.pop_frame()? {
            output_tx.blocking_send(frame)?;
        }
    }
}
```

**Tests**: Feed frames with simulated jitter (+-30ms), verify output is smoother than input. Feed burst of 5 frames, verify they're spread over expected playout times.

---

### 4f: Frame Freeze & Late Frame Skip (P1)

**Goal**: Hold last frame on timeout (don't stall); skip frames too old to display.

**Files**: `moq-media/src/subscribe.rs`, `moq-media/src/jitter.rs`

**Late frame detection**: In `VideoJitterBuffer::pop_ready()`, if frame's playout time is more than `2 * target_depth` behind current position, drop it. Increment `frames_dropped_late` counter. Log at `trace!` level.

**Frame freeze**: Already handled by the timeout mechanism in 4e — when no frames arrive, the `recv_timeout` returns `Timeout`, the loop continues, and the UI keeps displaying the last decoded frame (since `WatchTrackFrames::current_frame()` returns the most recent).

**Frame repeat signaling**: Optionally emit a `FrameEvent::Freeze` or similar to let the UI show a "connection poor" indicator. Add `freeze_count` to stats.

**Tests**: Inject 200ms gap in frame delivery, verify no panic/stall, last frame remains accessible. Inject 5 late frames (timestamp 100ms behind playout), verify all skipped.

---

### 4g: Subscriber-Side Rendition Switching (P1)

**Goal**: Auto-switch between simulcast renditions based on receiver bandwidth.

**Files**: `moq-media/src/subscribe.rs`

**Design**: New `AdaptiveSubscriber` that wraps `SubscribeBroadcast` and manages rendition selection.

```rust
pub struct AdaptiveSubscriber {
    broadcast: SubscribeBroadcast,
    current_track: Option<WatchTrack>,
    current_rendition: String,
    /// Receive-side bandwidth estimate (bytes/sec from incoming data rate)
    receive_rate: ReceiveRateEstimator,
    /// Hysteresis timers
    upgrade_sustained_since: Option<Instant>,
    upgrade_hold: Duration,     // 3s — must sustain higher bandwidth before upgrading
}
```

**Rendition selection logic**: Each rendition's bitrate is known from catalog. Compare receive rate against rendition bitrate requirements:
- **Downgrade**: If current rendition's bitrate > 80% of receive rate for 500ms, switch down immediately
- **Upgrade**: If next-higher rendition's bitrate < 50% of receive rate sustained for 3s, switch up
- Hysteresis prevents oscillation

**Seamless switching**: Create new `WatchTrack` for target rendition. It starts at the latest MoQ group (keyframe). Wait for first decoded frame, then swap output. Drop old track.

**Receive rate estimation**: Measure incoming data volume in `forward_frames()` — sum `frame.payload.len()` per second, EMA smooth.

**Tests**: Mock catalog with 3 renditions, simulate bandwidth changes, verify correct rendition is selected with hysteresis.

---

### 4h: Graceful Degradation State Machine (P2)

**Goal**: Explicit quality levels with smooth transitions.

**New file**: `moq-media/src/quality.rs`

```rust
pub enum QualityLevel {
    Full,              // target bitrate, target fps, highest rendition
    ReducedBitrate,    // 60% bitrate, same fps and resolution
    ReducedFramerate,  // 50% fps via frame skipping in encoder loop
    LowerResolution,   // drop to next-lower rendition
    Minimum,           // lowest rendition, min bitrate, 15fps
    Frozen,            // hold last frame
}
```

**State transitions**:
- Downgrade: after `downgrade_hold` (500ms) of sustained low bandwidth, drop one level
- Upgrade: after `upgrade_hold` (5s) of sustained adequate bandwidth, raise one level
- Emergency: if bandwidth drops to near-zero, skip directly to Minimum or Frozen
- Recovery is always one level at a time (no jumping from Minimum to Full)

**Integration**: Consumes `watch::Receiver<BandwidthEstimate>`, publishes `watch::Sender<QualityLevel>`. Encoder thread reads quality level to adjust bitrate and frame skipping. Subscriber reads it for rendition switching decisions.

**Tests**: Simulate progressive bandwidth degradation, verify correct level transitions with hysteresis.

---

### 4i: Quality Metrics / Observability (P2)

**Goal**: Expose real-time stats for UI and debugging.

**New file**: `moq-media/src/video_stats.rs`

```rust
pub struct VideoPublishStats {
    pub encode_fps: f64,
    pub encode_bitrate_bps: u64,
    pub target_bitrate_bps: u64,
    pub frames_encoded: u64,
    pub keyframes_produced: u64,
    pub encode_time_avg: Duration,
    pub quality_level: QualityLevel,
}

pub struct VideoSubscribeStats {
    pub decode_fps: f64,
    pub receive_bitrate_bps: u64,
    pub frames_decoded: u64,
    pub frames_dropped_late: u64,
    pub frames_repeated: u64,
    pub decode_time_avg: Duration,
    pub jitter_buffer_depth: Duration,
    pub current_rendition: String,
    pub rendition_switches: u64,
}
```

Exposed via `Watchable<T>` on `PublishBroadcast` and `WatchTrack`. Updated every 500ms from encoder/decoder threads via atomic counters.

---

### 4j: Sender-Side Frame Pacing (P3)

**Goal**: Spread frame writes over time instead of bursting.

**Files**: `moq-media/src/publish.rs`

**Design**: Token bucket pacer in encoder thread. Rate = current target bitrate. Before each `producer.write(pkt)`, wait for sufficient tokens. Allow 2x burst for keyframes (they must arrive promptly for new subscribers).

```rust
struct FramePacer {
    tokens: f64,        // available byte budget
    rate_bps: f64,      // current rate
    last_refill: Instant,
    max_burst: usize,   // 2x average frame size
}
```

Lower priority — QUIC already handles pacing at the transport layer. This adds application-level smoothing on top.

---

## Implementation Order

```
4a: Encoder Control Interface     --+
                                    +---> 4c: Rate Control ---> 4j: Pacing (P3)
4b: Bandwidth Estimator           --+         |
                                              +---> 4h: Degradation SM (P2)
4d: Keyframe Request              (parallel)  |
                                              |
4e: Jitter Buffer + Timing        ---> 4f: Frame Freeze ---> 4g: Rendition Switching
                                                                    |
                                                            4i: Stats (P2)
```

Parallelizable tracks:
- **Track A** (sender): 4a -> 4b -> 4c -> 4j
- **Track B** (receiver): 4e -> 4f -> 4g
- **Track C** (signaling): 4d (independent)
- **Track D** (integration): 4h, 4i (after A+B)

## Verification

After each sub-phase:
```sh
cargo build --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-features
cargo fmt --check
```

End-to-end manual verification after all sub-phases:
1. Throttle network to 500kbps -> bitrate adapts down within 2-3s, latency stays <500ms
2. Remove throttle -> bitrate recovers within 10s
3. Add +-30ms jitter -> smooth playback, no visible judder
4. Inject 200ms gap -> frames skipped, playback recovers
5. Multiple renditions active -> subscriber switches to lower rendition under constraint
6. `VideoPublishStats` and `VideoSubscribeStats` update in UI during a call

## Commits

One commit per sub-phase (4a through 4j), each with descriptive message:
- `feat(codec): add set_bitrate and force_keyframe to encoder backends`
- `feat(media): add QUIC-based bandwidth estimator`
- `feat(media): wire bandwidth estimate to encoder rate control`
- `feat(media): add keyframe request channel`
- `feat(media): add adaptive video jitter buffer`
- `feat(media): add frame freeze and late frame skip`
- `feat(media): add subscriber-side rendition switching`
- `feat(media): add quality degradation state machine`
- `feat(media): add video publish/subscribe stats`
- `feat(media): add sender-side frame pacing`
