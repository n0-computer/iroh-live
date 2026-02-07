# Phase 3: Audio Resilience

## Goal
Bring audio quality to libwebrtc grade: survive packet loss gracefully, save bandwidth during silence, smooth out network jitter. After this phase, audio remains intelligible even with 10-20% packet loss.

## Prerequisites
- Phase 1 complete (Opus encoder/decoder via unsafe-libopus)

## Components

### 1. Enhanced Opus FEC (encoder + decoder coordination)

#### Encoder (`codec/audio/encoder.rs`)
FEC should already be enabled from Phase 1 (`OPUS_SET_INBAND_FEC(1)`, `OPUS_SET_PACKET_LOSS_PERC(10)`).

Enhancements:
- **Adaptive loss percentage**: periodically update `OPUS_SET_PACKET_LOSS_PERC` based on actual observed loss rate (feedback from decoder/transport). Higher loss % → encoder embeds more redundancy → more bandwidth used but better recovery.
- **Bandwidth guard**: FEC adds ~50% overhead at speech bitrates. If bitrate is already constrained, reduce FEC aggressiveness.

#### Decoder (`codec/audio/decoder.rs`)
- **1-frame lookahead buffer**: Don't decode packets immediately. Buffer one packet so that when the *next* packet arrives, we can check if the *previous* was lost:
  - Packet N arrives → buffer it
  - Packet N+1 arrives → decode N normally, buffer N+1
  - If N+1 arrives but N was lost → decode N+1 with `decode_fec=1` to recover N from N+1's embedded FEC, then decode N+1 normally
- **Consecutive loss**: If both N and N+1 are lost, fall back to PLC (`opus_decode_float(null, 0, ...)`) which generates comfort noise
- **Sequence tracking**: need packet sequence numbers to detect gaps. Currently timestamps serve this role — detect gaps by checking if timestamp delta > expected frame duration

### 2. Opus DTX + Comfort Noise

#### Encoder (`codec/audio/encoder.rs`)
DTX should already be enabled from Phase 1 (`OPUS_SET_DTX(1)`).

- When VAD detects silence, encoder emits special short packets (~1 byte) or skips entirely
- Reduces bandwidth to near-zero during silence (most of a typical call)

#### Decoder (`codec/audio/decoder.rs`)
- On receiving DTX silence indicator (very short packet) or no packet during expected interval:
  - Generate comfort noise via `opus_decode_float(null, 0, ...)` — Opus PLC produces low-level background noise that sounds natural
- **Do NOT output pure digital silence** — abrupt silence is perceived as "call dropped" by users

### 3. Audio Jitter Buffer

#### New file: `moq-media/src/audio_jitter.rs`

**Current problem** (`subscribe.rs` audio loop): Packets are pulled from mpsc channel via `try_recv()` in a 10ms tick loop and immediately decoded. Any network jitter causes:
- Underruns → glitches/silence gaps
- Overruns → unbounded latency growth (buffer never drains)

**Design**: Adaptive playout buffer replacing the direct channel→decode path.

```rust
pub struct AudioJitterBuffer {
    /// Packets ordered by timestamp, waiting for playout
    buffer: BTreeMap<u64, hang::Frame>,
    /// Target buffer depth in ms — adapts to observed jitter
    target_depth_ms: f32,
    /// Observed jitter (EMA-smoothed)
    jitter_ema: f32,
    /// Last playout timestamp
    last_playout_ts: Option<u64>,
    /// Expected frame duration in timestamp units
    frame_duration: u64,
}
```

**Core algorithm**:
1. **Insert**: `push(packet)` — insert into BTreeMap by timestamp. Discard if older than playout point (arrived too late).
2. **Pull**: `pop() -> PopResult` — called every 10ms tick:
   - If buffer has packet at or before current playout time → return `PopResult::Packet(frame)`
   - If buffer empty but within target depth → return `PopResult::Wait` (let PLC handle it)
   - If buffer empty and exceeded depth → return `PopResult::Lost` (trigger PLC)
   - If buffer overflowing (depth > 2x target) → skip oldest packets, return next valid one
3. **Adapt**: After each pop, update jitter EMA:
   ```
   jitter = |actual_arrival - expected_arrival|
   jitter_ema = α * jitter + (1-α) * jitter_ema   (α = 0.05)
   target_depth_ms = max(20, min(200, jitter_ema * 3))
   ```

**Pop result enum**:
```rust
enum PopResult {
    Packet(hang::Frame),  // Normal: decode this packet
    Lost,                 // Gap detected: trigger FEC or PLC
    Wait,                 // Buffer not ready yet: output silence or last frame
}
```

#### Integration into `subscribe.rs`
- Replace direct `packet_rx.try_recv()` → `decoder.push_packet()` pattern
- Insert jitter buffer between channel and decoder:
  ```
  packet_rx → jitter_buffer.push() ... jitter_buffer.pop() → decoder
  ```
- On `PopResult::Lost`: call decoder's PLC path
- On `PopResult::Wait`: output silence frame (zeros)

### 4. Loss rate feedback

#### New: transport-level loss detection
- Track packet sequence numbers (from timestamps, assuming fixed frame duration)
- Count gaps → compute loss percentage over sliding window (e.g., last 100 packets)
- Feed back to encoder: `OPUS_SET_PACKET_LOSS_PERC(observed_loss)`
- This closes the FEC adaptation loop

## Files

| File | Change |
|---|---|
| `moq-media/src/codec/audio/encoder.rs` | Adaptive FEC loss %, DTX (if not in Phase 1) |
| `moq-media/src/codec/audio/decoder.rs` | 1-frame FEC lookahead, PLC, comfort noise |
| `moq-media/src/audio_jitter.rs` | New: adaptive jitter buffer |
| `moq-media/src/subscribe.rs` | Integrate jitter buffer into audio decode loop |
| `moq-media/src/lib.rs` | Add `pub mod audio_jitter` |

## Testing

### 5. Unit tests — Jitter buffer (`test_audio_jitter.rs`)

#### 5a. Ordered sequence
- Push 100 packets with monotonically increasing timestamps, pop all → all delivered in order, no loss, no skips.
- Verify `PopResult::Packet` for every pop.

#### 5b. Reordered packets
- Push packets in order [1, 3, 2, 5, 4] → pop should deliver [1, 2, 3, 4, 5] (buffer reorders).
- Verify correct timestamp ordering in output.

#### 5c. Gapped sequence (single packet loss)
- Push [1, 2, 4, 5] (gap at 3) → pop returns Packet(1), Packet(2), Lost (for 3), Packet(4), Packet(5).
- Verify `PopResult::Lost` is emitted exactly once for the gap.

#### 5d. Burst loss
- Push [1, 2, 6, 7] (gap at 3,4,5) → three Lost results between 2 and 6.
- Verify buffer handles multi-packet gaps correctly.

#### 5e. Late arrival (too old)
- Push [1, 2, 3], pop [1, 2, 3], then push [1] again → late packet discarded (already past playout point).
- Verify no duplicate output.

#### 5f. Buffer overflow
- Push 200 packets without popping → buffer depth exceeds 2x target → oldest packets skipped on next pop.
- Verify buffer doesn't grow unboundedly.

#### 5g. Empty buffer underrun
- Pop from empty buffer → `PopResult::Wait` (within target depth) or `PopResult::Lost` (exceeded depth).
- Verify no panic on empty pop.

#### 5h. Adaptive depth
- Feed packets with high jitter (±50ms) → verify `target_depth_ms` increases from initial value.
- Feed packets with low jitter (±2ms) → verify `target_depth_ms` decreases toward minimum.
- Verify `target_depth_ms` stays within [20ms, 200ms] bounds.

#### 5i. Timestamp wraparound
- Test with timestamps near u64::MAX → verify no overflow panic.

#### 5j. Frame duration detection
- Push packets with 20ms spacing → verify `frame_duration` is detected as 20ms.
- Push packets with 10ms spacing → verify detection adapts.

### 6. Unit tests — FEC & PLC (`test_fec_plc.rs`)

#### 6a. FEC recovery (1-frame lookahead)
- Encode 10 packets with FEC enabled. Drop packet 5. Feed packets [1,2,3,4,6,7,8,9,10] to decoder.
- When packet 6 arrives and 5 is detected as lost, decode packet 6 with `decode_fec=1` to recover packet 5.
- Verify 10 output frames (no gap), and recovered frame 5 is non-silent.

#### 6b. FEC with consecutive loss
- Drop packets 5 and 6. Packet 7 can only recover packet 6 via FEC, not 5.
- Verify PLC generates comfort noise for packet 5, FEC recovers packet 6.
- Output: 10 frames, frame 5 is PLC noise, frames 6-10 are decoded audio.

#### 6c. PLC comfort noise quality
- Drop a single packet, trigger PLC (`opus_decode_float(null, 0, ...)`).
- Verify output is NOT digital silence (all zeros) — it should be low-level noise.
- Verify amplitude of PLC output is < 10% of normal signal amplitude (it's comfort noise, not loud).

#### 6d. PLC consecutive frames
- Drop 3 consecutive packets → PLC generates 3 frames of comfort noise.
- Verify each frame is non-silent and amplitude decays gradually (Opus PLC fades).

#### 6e. No FEC recovery without FEC data
- Encode packets with FEC disabled (`OPUS_SET_INBAND_FEC(0)`), drop a packet, try `decode_fec=1`.
- Verify decoder handles this gracefully (falls back to PLC, no crash).

### 7. Unit tests — DTX (`test_dtx.rs`)

#### 7a. Silence detection
- Feed 1 second of digital silence to encoder with DTX enabled.
- Verify packets are emitted but are very small (≤ 3 bytes) or encoder returns empty.

#### 7b. Speech→Silence→Speech transition
- Feed 500ms speech, 1s silence, 500ms speech.
- Verify: normal packets → tiny/no packets → normal packets. No audible click or pop at transitions.

#### 7c. Bandwidth savings
- Encode 10 seconds: 2s speech + 6s silence + 2s speech.
- Measure total encoded bytes. Compare with DTX disabled. DTX should use < 50% of non-DTX bandwidth.

#### 7d. Decoder handling of DTX packets
- Feed DTX silence indicator packets to decoder → should produce comfort noise (not silence).
- Feed gap (no packets for 200ms during DTX) → decoder PLC fills with comfort noise.

### 8. Unit tests — Loss rate feedback (`test_loss_feedback.rs`)

#### 8a. Loss rate calculation
- Simulate 100 packets with 10 dropped → observed loss = 10%.
- Verify sliding window calculation matches expected rate within ±2%.

#### 8b. Feedback loop
- Set initial `OPUS_SET_PACKET_LOSS_PERC(5)`. Simulate 20% loss for 2 seconds.
- Verify encoder's loss percentage is updated to ~20% (within tolerance).
- Reduce simulated loss to 0% for 2 seconds → verify encoder's loss percentage decreases.

#### 8c. Bandwidth guard
- At low bitrate (32kbps), high loss → FEC should be limited (not consume > 50% of bandwidth).
- Verify FEC doesn't cause bitrate to double when bandwidth is already constrained.

#### 8d. Stable network (no adaptation needed)
- Simulate 0% loss for 5 seconds → loss percentage stays at 0 or minimum.
- No unnecessary encoder reconfiguration.

### 9. Integration tests — `moq-media/tests/`

#### 9a. Packet loss simulation (`test_audio_loss_sim.rs`)
Full pipeline: generate speech-like audio → encode with FEC → simulate packet loss → jitter buffer → decode.

- **5% random loss**: FEC recovers most. Verify: output length matches input length (±1 frame), no silence gaps > 20ms.
- **10% random loss**: some PLC. Verify: output length matches, PLC frames are present but no crashes.
- **20% random loss**: degraded but functional. Verify: no panics, no unbounded silence, output is continuous.
- **30% random loss**: heavily degraded. Verify: still doesn't crash, output is continuous (even if low quality).
- **0% loss (baseline)**: perfect roundtrip, verify output closely matches input.

#### 9b. Jitter simulation (`test_audio_jitter_sim.rs`)
Full pipeline with timing: generate audio → encode → add random delay ±50ms to packet delivery → jitter buffer → decode.

- Verify smooth playout: consecutive output frames have consistent 20ms spacing (within ±5ms).
- Verify no underruns: no silence gaps in output.
- Verify no overflow: buffer depth stays bounded.
- Verify adaptive depth: buffer adjusts to observed jitter level.

#### 9c. Combined loss + jitter (`test_audio_combined_sim.rs`)
- 10% loss + ±30ms jitter → verify audio remains continuous and intelligible.
- 20% loss + ±50ms jitter → verify no crashes, audio is degraded but present.

#### 9d. DTX bandwidth measurement (`test_dtx_bandwidth.rs`)
- Encode 30 seconds: alternating 3s speech / 3s silence.
- With DTX: measure total bytes. Without DTX: measure total bytes.
- Assert DTX version uses < 60% of non-DTX bandwidth.

#### 9e. Long-running stability (`test_audio_stability.rs`)
- Run encode→jitter buffer→decode pipeline for 60 seconds of simulated audio.
- Verify: no memory leaks (RSS stable), no panic, buffer depth stable, loss rate tracking accurate.

## Verification (manual)

1. Manual: two-party call with `tc netem` adding 10% loss → audio remains intelligible
2. Manual: call with ±50ms jitter → smooth playback, no glitches
3. Manual: mute microphone → observe DTX bandwidth drop in logs
4. Manual: unmute → audio resumes cleanly, no click/pop
