# Phase 3c: Forward Error Correction (Future Work)

## Status: Future Work

This phase depends on Phase 3a (rendition switching + NetworkSignals) and Phase 3b (jitter buffers). It adds codec-level error recovery and silence handling, building on the signal infrastructure from Phase 3a and the playout buffers from Phase 3b.

## Goal

Bring audio quality to libwebrtc grade: survive packet loss gracefully via Opus FEC/PLC, save bandwidth during silence via DTX, and close the feedback loop between observed loss and encoder FEC tuning.

## Prerequisites

- Phase 3a complete (NetworkSignals infrastructure)
- Phase 3b complete (jitter buffers provide the frame timing needed for gap detection)
- Opus encoder/decoder already support FEC/DTX flags from Phase 1

## Steps

### Step 1: Enhanced Opus FEC (encoder + decoder coordination)

#### Encoder (`codec/audio/encoder.rs`)
FEC should already be enabled from Phase 1 (`OPUS_SET_INBAND_FEC(1)`, `OPUS_SET_PACKET_LOSS_PERC(10)`).

Enhancements:
- **Adaptive loss percentage**: periodically update `OPUS_SET_PACKET_LOSS_PERC` based on actual observed loss rate (from `NetworkSignals.loss_rate`). Higher loss % → encoder embeds more redundancy → more bandwidth but better recovery.
- **Bandwidth guard**: FEC adds ~50% overhead at speech bitrates. If bitrate is already constrained, reduce FEC aggressiveness.

#### Decoder (`codec/audio/decoder.rs`)
- **1-frame lookahead buffer**: Don't decode packets immediately. Buffer one packet so that when the *next* packet arrives, we can check if the *previous* was lost:
  - Packet N arrives → buffer it
  - Packet N+1 arrives → decode N normally, buffer N+1
  - If N+1 arrives but N was lost → decode N+1 with `decode_fec=1` to recover N from N+1's embedded FEC, then decode N+1 normally
- **Consecutive loss**: If both N and N+1 are lost, fall back to PLC (`opus_decode_float(null, 0, ...)`) which generates comfort noise
- **Sequence tracking**: detect gaps by checking if timestamp delta > expected frame duration

### Step 2: Opus DTX + Comfort Noise

#### Encoder (`codec/audio/encoder.rs`)
DTX should already be enabled from Phase 1 (`OPUS_SET_DTX(1)`).

- When VAD detects silence, encoder emits special short packets (~1 byte) or skips entirely
- Reduces bandwidth to near-zero during silence

#### Decoder (`codec/audio/decoder.rs`)
- On receiving DTX silence indicator or no packet during expected interval:
  - Generate comfort noise via `opus_decode_float(null, 0, ...)` — Opus PLC produces low-level background noise
- **Do NOT output pure digital silence** — abrupt silence is perceived as "call dropped"

### Step 3: Loss Rate Feedback

- Use `NetworkSignals.loss_rate` from Phase 3a (already available)
- Feed back to encoder: `OPUS_SET_PACKET_LOSS_PERC(observed_loss)`
- This closes the FEC adaptation loop

### Step 4: Video FEC Considerations

- QUIC handles reliable delivery natively, so video FEC is lower priority
- If needed: simple XOR-based parity packets across MoQ groups
- Deferred until real-world testing shows QUIC retransmission is insufficient

## Files

| File | Change |
|---|---|
| `moq-media/src/codec/audio/encoder.rs` | Adaptive FEC loss %, DTX |
| `moq-media/src/codec/audio/decoder.rs` | 1-frame FEC lookahead, PLC, comfort noise |
| `moq-media/src/subscribe.rs` | Wire loss_rate feedback to encoder (via NetworkSignals) |

## Testing

### Unit tests

- **Jitter buffer + FEC**: Push gapped sequences, verify FEC recovery
- **PLC comfort noise**: Drop packet, verify output is non-silent low-level noise
- **DTX bandwidth savings**: Encode speech + silence, verify DTX reduces bandwidth by >50%
- **Loss rate feedback**: Simulate loss → verify encoder `PACKET_LOSS_PERC` adapts

### Integration tests

- **5% random loss**: FEC recovers most, output continuous
- **10% random loss**: some PLC, no crashes
- **20% random loss**: degraded but functional
- **DTX bandwidth measurement**: 30s alternating speech/silence, measure savings

## Verification (manual)

1. Two-party call with `tc netem` adding 10% loss → audio remains intelligible
2. Mute microphone → observe DTX bandwidth drop in logs
3. Unmute → audio resumes cleanly, no click/pop
