# Phase 3d: Adaptive Encoding (Future Work)

## Status: Future Work

This phase depends on Phase 3a (rendition switching + NetworkSignals). It adds sender-side adaptation: encoders adjust bitrate/framerate in response to network conditions, complementing the subscriber-side rendition switching from Phase 3a.

## Goal

Publisher-side rate control: encoders adapt bitrate and framerate based on QUIC congestion signals. Combined with Phase 3a's subscriber-side rendition switching, this provides end-to-end adaptive quality.

## Prerequisites

- Phase 3a complete (NetworkSignals infrastructure, rendition switching)
- Phase 3c optional (FEC helps but is not required)

## Steps

### Step 1: Encoder Runtime Control

**Goal**: Add `set_bitrate()` and `force_keyframe()` to encoder trait and all backends.

**Files**:
- `moq-media/src/av.rs` — extend `VideoEncoderInner` trait
- `moq-media/src/codec/video/encoder.rs` — H264Encoder (OpenH264)
- `moq-media/src/codec/video/rav1e_enc.rs` — Av1Encoder
- `moq-media/src/codec/video/vtb_enc.rs` — VtbEncoder (if exists)
- `moq-media/src/codec/video/vaapi_enc.rs` — VaapiEncoder (if exists)

**Trait additions** (with default impls):
```rust
fn set_bitrate(&mut self, bitrate_bps: u64) -> Result<()> { Ok(()) }
fn force_keyframe(&mut self);
```

**Per-backend**:
| Backend | set_bitrate | force_keyframe |
|---------|-------------|----------------|
| H264 (OpenH264) | `raw_api.set_option(ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, ...)` | `raw_api.force_intra_frame(true)` |
| AV1 (rav1e) | Flush + recreate `Context` with new bitrate at keyframe boundary | `ctx.flush()` triggers new keyframe |
| VTB | `VTSessionSetProperty(kVTCompressionPropertyKey_AverageBitRate, ...)` | `kVTEncodeFrameOptionKey_ForceKeyFrame` |
| VAAPI | `Tunings` with `ConstantBitrate(new_value)` via `tune()` | `FrameMetadata.force_keyframe = true` |

### Step 2: Bandwidth Estimator

**Goal**: Read QUIC path stats, produce bandwidth estimate. Reuses `NetworkSignals` from Phase 3a.

The `NetworkSignals.available_bps` field from Phase 3a already provides this. This step adds EMA smoothing and debouncing specifically for encoder rate control:

```rust
pub struct EncoderRateController {
    signals: Watcher<NetworkSignals>,
    last_target_bps: u64,
    ema_available_bps: f64,
}
```

**Algorithm**: EMA with alpha=0.3 on `available_bps`. Only call `set_bitrate()` if target changed by >5% (debounce). Reserve 15% for audio + protocol overhead.

### Step 3: Transport-Aware Rate Control

**Goal**: Wire bandwidth estimate to encoder threads.

```rust
pub struct EncoderControl {
    pub signals: Watcher<NetworkSignals>,
}
```

In the encoder loop, before `push_frame()`:
1. Check `signals.update()` — if changed, compute target bitrate
2. Target = `available_bps * 0.85` (leave 15% for audio + overhead)
3. Clamp to preset-specific [min, max] range
4. Call `encoder.set_bitrate(target)` if changed by >5%

**Bitrate bounds per preset**:
| Preset | Min | Max |
|--------|-----|-----|
| 180p | 50 kbps | 500 kbps |
| 360p | 100 kbps | 1.5 Mbps |
| 720p | 300 kbps | 3 Mbps |
| 1080p | 500 kbps | 6 Mbps |

### Step 4: Keyframe Request Channel

**Goal**: Subscriber can request keyframe from publisher for mid-stream recovery.

- `PublishBroadcast::request_keyframe(track_name)` sends to encoder thread
- Decoder thread calls `request_keyframe()` on decode error
- MoQ groups already handle initial join (new subscribers start at keyframe)

### Step 5: Quality Degradation State Machine

```rust
pub enum QualityLevel {
    Full,              // target bitrate, target fps, highest rendition
    ReducedBitrate,    // 60% bitrate
    ReducedFramerate,  // 50% fps via frame skipping
    LowerResolution,   // drop to next-lower rendition
    Minimum,           // lowest rendition, min bitrate, 15fps
    Frozen,            // hold last frame
}
```

Consumes `NetworkSignals`, publishes `QualityLevel`. Encoder thread reads quality level for bitrate/framerate adjustment. Progressive degradation with hysteresis.

### Step 6: Sender-Side Frame Pacing

Token bucket pacer in encoder thread. Lower priority — QUIC already paces at the transport layer.

### Step 7: Quality Metrics / Observability

```rust
pub struct VideoPublishStats {
    pub encode_fps: f64,
    pub encode_bitrate_bps: u64,
    pub target_bitrate_bps: u64,
    pub quality_level: QualityLevel,
}

pub struct VideoSubscribeStats {
    pub decode_fps: f64,
    pub current_rendition: String,
    pub rendition_switches: u64,
    pub jitter_buffer_depth: Duration,
}
```

## Implementation Order

```
Step 1: Encoder Control  ──┐
                            ├──> Step 3: Rate Control ──> Step 6: Pacing
Step 2: Rate Controller  ──┘         │
                                     └──> Step 5: Degradation SM
Step 4: Keyframe Request (parallel)
Step 7: Stats (after all above)
```

## Files

| File | Change |
|---|---|
| `moq-media/src/av.rs` | `set_bitrate()`, `force_keyframe()` on encoder trait |
| `moq-media/src/codec/video/encoder.rs` | H264 implementation |
| `moq-media/src/codec/video/rav1e_enc.rs` | AV1 implementation |
| `moq-media/src/publish.rs` | `EncoderControl`, rate control in encoder loop |
| `moq-media/src/quality.rs` | **New**: QualityLevel state machine |
| `moq-media/src/video_stats.rs` | **New**: publish/subscribe stats |

## Verification

1. Throttle to 500kbps → bitrate adapts down within 2-3s
2. Remove throttle → bitrate recovers within 10s
3. Multiple renditions active → bandwidth split proportionally
4. Stats update in UI during a call
