# Acoustic Echo Cancellation

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | moq-media |

## Problem

In a video call, the speaker plays remote audio that the local microphone picks up. Without echo cancellation, the remote participant hears their own voice delayed by the round-trip time. AEC removes this echo by subtracting an estimate of the speaker output from the microphone input.

## Implementation

The AEC uses sonora, a Rust port of WebRTC's AEC3 algorithm. Two types handle the processing:

**`AecProcessor`** wraps `sonora::AudioProcessing` with enable/disable control. It is independent of any audio backend and operates on 10 ms frames of f32 samples. The processor is configured with both echo cancellation and high-level noise suppression enabled by default. It is `Clone` (Arc-based) and thread-safe, though the inner `AudioProcessing` instance is behind a Mutex.

**`AecState`** accumulates samples from cpal callbacks into 10 ms frames, calls the processor, and drains the output. All processing is serialized on the input callback thread to avoid Mutex contention between the separate cpal input and output callbacks.

## Signal flow

The AEC needs two signals: the audio being played back through the speaker (the *render reference*) and the audio captured by the microphone.

**Render path**: application audio flows through the output ring buffer to the cpal output callback. A copy of the mixed output is written to a render reference ring buffer (100 ms capacity at 48 kHz stereo). The AEC reads this reference to know what the speaker is playing.

**Capture path**: the cpal input callback writes microphone samples to the input ring buffer. `AecState` accumulates these into 10 ms frames, feeds each frame plus the corresponding render reference into `AecProcessor::process_capture_f32()`, and outputs the cleaned signal.

The render reference and capture signal must be time-aligned for the AEC to converge. The `set_stream_delay` method on `AecProcessor` adjusts for device-level latency differences, but this is only called during device switches (rare), not on every frame.

## Real-time safety

The cpal callbacks run on OS audio threads with strict real-time constraints. The AEC design respects this:

- All buffers (render reference ring buffer, 10 ms accumulation buffers) are pre-allocated at stream creation time.
- The cpal callbacks perform no heap allocations.
- `AecProcessor` acquires its Mutex in `process_capture_f32`, `process_render_f32`, and `set_stream_delay`. Contention is minimal because `AecState` serializes all capture and render processing on the input callback thread, so the lock is never contested in the per-frame path.

## Configuration

`AecProcessorConfig` specifies the stream configs for both the capture and render sides (sample rate, channel count). The default is 48 kHz stereo for both. The 10 ms frame size (480 samples per channel at 48 kHz) matches WebRTC's AEC3 processing granularity.
