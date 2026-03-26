# Adaptive encoding

Sender-side rate control: encoders adapt bitrate/framerate based on QUIC
congestion signals. Complements subscriber-side rendition switching.

## Status

Step 1 is partial. Remaining steps are future work.

## Steps

### Step 1: Encoder runtime control (partial)

`set_bitrate()` exists on `VideoEncoder` trait (default no-op) and
`AudioEncoder` trait. Implemented for: VAAPI (via `tune()`), VideoToolbox
(`VTSessionSetProperty`), Android (MediaCodec), Opus. `force_keyframe()`
exists on VAAPI and VTB encoders but not on the trait.

**Remaining:**
- [ ] `set_bitrate()` for openh264 (`raw_api.set_option(ENCODER_OPTION_SVC_ENCODE_PARAM_EXT)`)
- [ ] `set_bitrate()` for rav1e (flush + recreate Context with new bitrate)
- [ ] `force_keyframe()` on `VideoEncoder` trait
- [ ] `force_keyframe()` for openh264 (`raw_api.force_intra_frame(true)`)

### Step 2: Bandwidth estimator

EMA smoothing on `NetworkSignals.available_bps` with debounce (>5% change
threshold). Reserve 15% for audio + protocol overhead.

### Step 3: Wire rate control to encoder threads

In encoder loop: check signals, compute target = `available_bps * 0.85`,
clamp to preset-specific [min, max], call `set_bitrate()` if changed >5%.

### Step 4: Keyframe request channel

Subscriber requests keyframe from publisher on decode error. MoQ groups
already handle initial join (new subscribers start at keyframe).

### Step 5: Quality degradation state machine

Progressive: Full -> ReducedBitrate -> ReducedFramerate -> LowerResolution ->
Minimum -> Frozen. Consumes `NetworkSignals`, hysteresis.

### Steps 6-7: Frame pacing, quality metrics

Lower priority. QUIC already paces at transport layer.
