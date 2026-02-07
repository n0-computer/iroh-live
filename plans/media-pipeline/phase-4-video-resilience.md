# Phase 4: Video Resilience

## Goal
Adaptive video quality that responds to network conditions, smooth playback timing, and graceful degradation via temporal scalability. After this phase, video remains watchable even on constrained or lossy networks.

## Prerequisites
- Phase 1 complete (openh264 encoder/decoder working)
- Phase 2 optional (HW backends benefit from same ABR interface)

## Components

### 1. Adaptive Bitrate (ABR)

#### Problem
Current encoder uses fixed bitrate. On bandwidth-constrained links, encoded frames queue up in the QUIC send buffer, latency grows unboundedly, and eventually the connection stalls.

#### Design

**Bandwidth signal source**: Monitor QUIC transport backpressure from iroh/quinn.
- Option A: Check `quinn::SendStream` write readiness — if writes start blocking, bandwidth is constrained
- Option B: Monitor the mpsc channel between encoder and `TrackProducer` — if frames accumulate, downstream is congested
- Option C: Periodic `quinn::Connection::stats()` to read congestion window, RTT, bytes in flight

**Encoder bitrate control**:
- Add `set_bitrate(bps: u64)` to `VideoEncoderBackend` trait
- openh264 supports runtime bitrate changes via `SetOption(ENCODER_OPTION_BITRATE, ...)`
- cros-codecs/VTB: also support dynamic bitrate

**ABR algorithm** (simple AIMD — Additive Increase, Multiplicative Decrease):
```
every 1 second:
  if send_queue_depth > HIGH_WATERMARK:
    bitrate = max(bitrate * 0.7, min_bitrate)     # multiplicative decrease
    log("ABR: reducing bitrate to {bitrate}")
  elif send_queue_depth < LOW_WATERMARK:
    bitrate = min(bitrate + step_up, max_bitrate)  # additive increase
```

**Bitrate bounds** per preset:
| Preset | Min bitrate | Max bitrate | Step up |
|--------|-------------|-------------|---------|
| 180p   | 50 kbps     | 500 kbps    | 30 kbps |
| 360p   | 100 kbps    | 1.5 Mbps    | 80 kbps |
| 720p   | 300 kbps    | 3 Mbps      | 150 kbps |
| 1080p  | 500 kbps    | 6 Mbps      | 300 kbps |

#### Integration into `publish.rs`
- In the video encoder thread loop, after each encode cycle:
  1. Check send queue depth (channel backpressure or QUIC stats)
  2. Run ABR algorithm
  3. If bitrate changed: call `encoder.set_bitrate(new_bitrate)`
- Expose current bitrate as observable metric for UI

### 2. Video Frame Timing & Smoothing

#### Problem
`StreamClock` computes raw inter-packet delay from timestamps with no smoothing. Jittery networks cause jittery playback. Video decode loop uses `blocking_recv()` with no timing control.

#### Frame delay smoothing
Replace raw `StreamClock::frame_delay()` with EMA-smoothed delay:
```rust
struct SmoothedClock {
    last_timestamp: Option<Timestamp>,
    smoothed_delay: Duration,
    alpha: f32,  // 0.1 = heavy smoothing, 0.3 = responsive
}

impl SmoothedClock {
    fn frame_delay(&mut self, timestamp: &Timestamp) -> Duration {
        let raw_delay = /* compute from timestamps */;
        self.smoothed_delay = Duration::from_secs_f32(
            self.alpha * raw_delay.as_secs_f32()
            + (1.0 - self.alpha) * self.smoothed_delay.as_secs_f32()
        );
        self.smoothed_delay
    }
}
```

#### Frame freeze (last-frame hold)
- In `subscribe.rs` video decode loop: if no frame available within expected interval, emit `PopResult::Freeze` to signal the display should hold the last good frame
- Currently `blocking_recv()` blocks indefinitely — add a timeout equal to 2x expected frame duration
- On timeout: hold last frame, don't stall

#### Late frame detection
- Track playout clock (monotonic wall time relative to first frame)
- If decoded frame's timestamp is older than current playout time minus tolerance (e.g., 50ms):
  - Skip the frame
  - Log the skip
  - Proceed to next

#### Implementation in `subscribe.rs` video decode loop
```rust
// Replace current blocking_recv pattern:
loop {
    match input_rx.recv_timeout(frame_timeout) {
        Ok(packet) => {
            decoder.push_packet(packet)?;
            while let Some(frame) = decoder.pop_frame()? {
                if frame.timestamp + tolerance < playout_clock.now() {
                    trace!("skipping late frame");
                    continue;
                }
                output_tx.blocking_send(frame)?;
            }
        }
        Err(RecvTimeoutError::Timeout) => {
            // Frame freeze: hold last frame, don't block
            trace!("frame timeout, holding last frame");
        }
        Err(RecvTimeoutError::Disconnected) => break,
    }
}
```

### 3. OpenH264 Temporal SVC

#### What it provides
OpenH264 supports temporal scalability with up to 4 layers in a dyadic hierarchy:
- 2 layers: base at 15fps + enhancement at 30fps
- 3 layers: base at 7.5fps + mid at 15fps + enhancement at 30fps

This allows downstream (SFU or receiver) to selectively drop higher temporal layers to reduce bandwidth without re-encoding. The base layer is always decodable on its own.

#### Encoder configuration (`codec/video/openh264_enc.rs`)
- Set `iTemporalLayerNum = 2` (or 3) in openh264 encoder config
- Each encoded frame is tagged with temporal layer ID
- Base layer frames are always keyframe-independent (temporal prediction only within layer)

#### NAL layer tagging
- openh264 `EncodedBitStream` provides layer info per NAL
- Tag each `hang::Frame` with temporal layer metadata:
  ```rust
  // Extend hang::Frame or use a wrapper
  struct TaggedFrame {
      frame: hang::Frame,
      temporal_layer: u8,  // 0 = base, 1 = enhancement, ...
  }
  ```
- Or encode layer info in a frame header byte prepended to payload

#### Integration with ABR
When ABR detects bandwidth pressure:
1. First: reduce bitrate (component 1)
2. If still congested: signal receiver to drop enhancement temporal layers (reduce fps)
3. If still congested: reduce resolution (switch to lower preset if simulcast)

This provides a smooth degradation path:
```
Full quality → lower bitrate → lower framerate (drop T1) → lower resolution
```

#### Decoder handling
- openh264 decoder handles temporal SVC transparently — it can decode any subset of temporal layers
- If enhancement layer NALs are dropped before reaching decoder, it still decodes base layer correctly

### 4. Quality metrics & observability

#### Expose metrics for UI/debugging
```rust
pub struct VideoStats {
    pub current_bitrate: u64,
    pub target_bitrate: u64,
    pub framerate: f32,
    pub frames_decoded: u64,
    pub frames_dropped: u64,
    pub frames_late: u64,
    pub decode_time_avg_ms: f32,
    pub send_queue_depth: usize,
}
```
- Update per-frame in encoder/decoder loops
- Observable via `Watchable<VideoStats>` for UI consumption

## Files

| File | Change |
|---|---|
| `moq-media/src/codec/video/encoder.rs` | Add `set_bitrate()` to backend trait and public encoder |
| `moq-media/src/codec/video/openh264_enc.rs` | Dynamic bitrate, temporal SVC config |
| `moq-media/src/codec/video/decoder.rs` | Smoothed clock, late frame detection |
| `moq-media/src/publish.rs` | ABR loop in encoder thread, queue monitoring |
| `moq-media/src/subscribe.rs` | Frame timeout, frame freeze, late skip |

## Testing

### 5. Unit tests — ABR algorithm (`test_abr.rs`)

#### 5a. Multiplicative decrease
- Simulate `send_queue_depth > HIGH_WATERMARK` for 3 consecutive ticks.
- Verify bitrate decreases by 30% each tick (`bitrate * 0.7`).
- Verify bitrate never goes below `min_bitrate` for the current preset.

#### 5b. Additive increase
- Simulate `send_queue_depth < LOW_WATERMARK` for 10 consecutive ticks.
- Verify bitrate increases by `step_up` each tick.
- Verify bitrate never exceeds `max_bitrate` for the current preset.

#### 5c. AIMD convergence
- Start at max bitrate. Simulate congestion → decrease → clear → increase → congestion cycle.
- Verify bitrate oscillates and converges to a stable value (doesn't ping-pong wildly).

#### 5d. Preset boundaries
- For each preset (180p, 360p, 720p, 1080p): verify min/max/step values are used correctly.
- Verify bitrate clamps to bounds at edges.

#### 5e. Rapid congestion response
- Simulate sudden queue spike (0 → 10x HIGH_WATERMARK in one tick).
- Verify bitrate drops aggressively in one step (not gradual over many ticks).

#### 5f. Stable network (no adaptation)
- Queue depth stays between LOW and HIGH watermarks for 100 ticks.
- Verify bitrate stays unchanged — no unnecessary oscillation.

#### 5g. set_bitrate integration
- Call `set_bitrate()` on openh264 backend with various values.
- Encode frames before and after, verify encoded size changes proportionally.

### 6. Unit tests — Frame timing (`test_frame_timing.rs`)

#### 6a. EMA smoothing
- Feed raw delays [33, 50, 33, 16, 33] ms to `SmoothedClock`.
- With α=0.1, verify smoothed output doesn't jump as wildly as raw input.
- Verify after 30 consistent 33ms inputs, smoothed delay converges to ~33ms.

#### 6b. First frame
- First frame has no previous timestamp → delay should be 0 or default (not random/panic).

#### 6c. High jitter damping
- Feed alternating 16ms/50ms delays (simulating high jitter).
- Verify smoothed output converges to ~33ms (average), not oscillating between extremes.

#### 6d. Timestamp gap
- Feed normal sequence then a 500ms gap (dropped frames).
- Verify smoothed clock doesn't produce a 500ms delay — caps at reasonable maximum.

### 7. Unit tests — Late frame detection (`test_late_frames.rs`)

#### 7a. On-time frame
- Frame arrives within tolerance of playout clock → frame is passed through.

#### 7b. Late frame skip
- Frame timestamp is 100ms behind playout clock (tolerance=50ms) → frame is skipped.
- Verify skip is logged.

#### 7c. Boundary case
- Frame timestamp is exactly at tolerance boundary → frame is kept (not skipped).

#### 7d. Burst of late frames
- 5 frames arrive all 200ms late → all skipped. Next on-time frame → passed through.
- Verify decoder doesn't stall on burst of skips.

#### 7e. Frame freeze (timeout)
- No frame arrives for 2x expected duration → `RecvTimeoutError::Timeout` triggers.
- Verify last frame is held (no panic, no stall).

### 8. Unit tests — Temporal SVC (`test_temporal_svc.rs`)

#### 8a. Layer tagging
- Encode 60 frames with 2 temporal layers → verify base layer (T0) frames have `temporal_layer=0`, enhancement (T1) frames have `temporal_layer=1`.
- With 30fps: T0 at 15fps (every other frame), T1 at 30fps.

#### 8b. Base layer standalone decode
- Encode 30 frames with 2 layers. Feed only T0 NALs to decoder (drop T1).
- Verify decoder produces 15 frames successfully (half framerate, but valid).

#### 8c. 3-layer SVC
- Encode with 3 layers. Drop T2 → 15fps. Drop T2+T1 → 7.5fps. All valid decode.

#### 8d. Layer identification from NAL
- Verify NAL units from openh264 `EncodedBitStream` correctly identify temporal layer.
- Parse layer info, verify it matches expected pattern (T0-T1-T0-T1...).

#### 8e. ABR + SVC integration
- Simulate congestion: ABR signals to drop enhancement layers.
- Verify: first attempt is bitrate reduction, second is layer dropping.
- Verify bitrate reduction → layer dropping → resolution reduction degradation path.

### 9. Unit tests — Quality metrics (`test_video_stats.rs`)

#### 9a. Counter accuracy
- Decode 100 frames, skip 5, drop 3 → verify `frames_decoded=92`, `frames_dropped=3`, `frames_late=5`.

#### 9b. Bitrate tracking
- Set encoder to 1Mbps → verify `current_bitrate` and `target_bitrate` fields match.
- ABR changes bitrate → verify `target_bitrate` updates immediately, `current_bitrate` follows after next encode.

#### 9c. Framerate calculation
- Decode 30 frames in 1 second → `framerate` ≈ 30.0 (±1).
- Decode 15 frames in 1 second (SVC T0 only) → `framerate` ≈ 15.0.

#### 9d. Decode time measurement
- `decode_time_avg_ms` is positive and reasonable (< 50ms for 720p software decode).
- Verify it's averaged over recent frames (not cumulative).

#### 9e. Observability
- Create `Watchable<VideoStats>`, update from encoder thread → verify consumer receives updates.
- Verify updates arrive at reasonable frequency (not every frame, throttled to ~1Hz).

### 10. Integration tests — `moq-media/tests/`

#### 10a. ABR simulation (`test_video_abr_sim.rs`)
Full pipeline: generate video frames → encode → simulate constrained send queue → ABR adjusts bitrate.

- **Unconstrained**: queue stays empty → bitrate stays at max → frame quality is high.
- **Moderately constrained**: queue grows slowly → bitrate decreases gradually → latency stays bounded.
- **Severely constrained**: queue spikes → bitrate drops to minimum rapidly → no stall.
- **Recovery**: constrain then unconstrain → bitrate recovers to near-max within 10 seconds.
- **Oscillation test**: alternating constraint/unconstrain every 2s → bitrate adapts without wild swings.

#### 10b. Frame timing simulation (`test_video_timing_sim.rs`)
Full pipeline with simulated jitter on packet delivery.

- **Low jitter (±5ms)**: playout is smooth, frame spacing is consistent.
- **High jitter (±30ms)**: smoothed clock absorbs jitter, playback is stable.
- **Packet burst**: 5 frames arrive simultaneously → spread over expected display times, not displayed all at once.

#### 10c. Late frame simulation (`test_video_late_sim.rs`)
- Introduce 200ms delay spike affecting 3 frames → frames are skipped, playback continues from next on-time frame.
- Verify no accumulation of stale frames in buffer.

#### 10d. Temporal SVC end-to-end (`test_temporal_svc_e2e.rs`)
- Encode 60 frames with 2 layers → drop T1 midstream (frames 30-60) → decoder output: 30fps for first half, 15fps for second half.
- Verify no decoder errors at layer transition point.
- Verify base layer is always self-contained (no dependency on dropped T1 frames).

#### 10e. Degradation path test (`test_degradation_path.rs`)
- Simulate progressively worsening network: start unconstrained → moderate → severe.
- Verify degradation follows expected path: full quality → lower bitrate → lower framerate (T1 drop) → lower resolution (if simulcast).
- Verify each step is stable and doesn't jump multiple steps at once.

#### 10f. Long-running video stability (`test_video_stability.rs`)
- Run encode→decode pipeline for 120 seconds of simulated video at 30fps.
- Verify: no memory leaks (RSS stable), no panic, metrics are accurate, bitrate stays bounded.
- Vary network conditions throughout (good → bad → good cycle).

## Verification (manual)

1. Manual: throttle network to 500kbps with `tc qdisc` → bitrate adapts down within 2-3 seconds, latency < 500ms
2. Manual: remove throttle → bitrate recovers within 10 seconds
3. Manual: add ±30ms jitter → smooth playback, no visible judder
4. Manual: introduce 200ms delay spike → late frames skipped, playback recovers
5. Manual: enable 2-layer temporal SVC, observe smooth 30fps → constrain → observe 15fps base layer
6. Manual: verify `VideoStats` updates in UI during a call
