# Audio Backend

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | moq-media |

The audio backend in `moq-media/src/audio_backend.rs` manages device enumeration, stream lifecycle, resampling, AEC, and peak metering. It uses cpal for platform audio I/O and sonora for echo cancellation and noise suppression.

## Architecture

Three thread categories interact:

1. **Caller threads** (tokio tasks, decode threads) push and pop samples through `OutputStream`/`InputStream` handles. Each handle owns a `ResamplingProd` or `ResamplingCons` behind a Mutex, which is uncontended because each handle has a single writer or reader.

2. **cpal callback threads** (real-time, OS-managed) read from output ring buffers and write to input ring buffers. These callbacks must be allocation-free and lock-free on the data path. They own the other end of each resampling channel.

3. **Driver thread** (OS thread, message-driven) handles stream lifecycle, device switching, and error recovery. It communicates with callbacks via bounded SPSC command channels.

## Internal processing

All internal processing runs at 48 kHz stereo. The `fixed-resample` crate provides ring buffer channels that resample at the device boundary: if a device runs at 44.1 kHz, the resampling channel converts between 44.1 kHz and 48 kHz transparently. This keeps codec and AEC processing at a single fixed rate.

## Device management

`AudioBackend` wraps the driver thread and exposes async methods for device enumeration and switching. `AudioBackendOpts` configures initial input/output devices and fallback behavior.

Device switching stops the old cpal stream and creates a new one with the target device. The resampling channels bridge the gap: the application side keeps pushing/popping at 48 kHz while the device side reconnects. A declicker (144-sample fade at 48 kHz, roughly 3 ms) smooths transitions to avoid pops.

## Peak metering

Peak meters run inline in the output callback. The smoothed peak level is exposed via `AudioSinkHandle::smoothed_peak_normalized()`, returning a value normalized to `0.0..=1.0`. No separate metering thread or allocation is involved.

## AEC integration

The AEC processor (see [audio/aec.md](audio/aec.md)) runs inline in the cpal input callback. The render reference (what the speaker is playing) feeds into the AEC alongside the microphone capture, and the AEC subtracts the echo. All AEC state accumulation and processing is serialized on the input callback thread.

## Open areas

**firewheel removal**: the audio graph engine (firewheel) is no longer used for routing, but some infrastructure remains. Replacing it with direct cpal stream management would reduce the audio backend by roughly 30% of its code.

**Android AEC**: sonora's AEC works on desktop platforms but has not been tested on Android. The Android audio stack has its own AEC (via `AudioEffect`), but integrating it requires JNI calls from the cpal callback thread, which conflicts with real-time constraints.
