# Replace firewheel with direct cpal + fixed-resample

## Motivation

The `AudioBackend` in `moq-media` uses firewheel v0.10 for audio graph
management, stream I/O via cpal, resampling, and peak metering. On
Android, the firewheel driver thread panics at `cx.start_stream()`
because firewheel's cpal integration does not handle Android's audio
lifecycle correctly — even when `ndk-context` is initialized. The
panic is inside firewheel-cpal, which we cannot easily patch.

Firewheel is a full audio graph engine designed for games. We use a
small fraction of it: one input stream, one output stream (sometimes
several of each), two AEC nodes, peak meters, and the graph's
resampling plumbing. Replacing it with direct cpal usage removes the
Android blocker and drops a heavy dependency tree (firewheel-core,
firewheel-graph, firewheel-cpal, firewheel-nodes, firewheel-pool,
bevy_platform, thunderdome, etc.).

## Current state

The audio backend lives in two files totaling ~1,600 lines:

- `moq-media/src/audio_backend.rs` (1,108 lines)
- `moq-media/src/audio_backend/aec.rs` (511 lines)

### What firewheel provides

1. **Audio graph** (`FirewheelContext`): node management, port
   connections, topological scheduling. We use it to wire:
   graph_in -> AecCapture -> StreamReader (input streams), and
   StreamWriter -> PeakMeter -> AecRender -> graph_out.

2. **cpal stream management** (`CpalBackend`, `CpalConfig`): opens
   input and output cpal streams, manages the real-time audio callback,
   routes audio through the graph. Handles device enumeration via cpal
   host/device IDs.

3. **Stream nodes** (`StreamWriterNode`, `StreamReaderNode`): ring-buffer
   bridges between application code and the real-time audio callback.
   Each has a `State`/`Handle` pair for thread-safe push/pull of
   interleaved f32 samples. Handles underflow/overflow detection.

4. **Resampling** (`cpal_resample_inputs` feature, `ResamplingChannelConfig`):
   converts between the application's sample rate (typically 48 kHz) and
   the device's native rate. Uses `fixed-resample` (which wraps `rubato`)
   internally.

5. **Peak meters** (`PeakMeterNode`, `PeakMeterSmoother`,
   `DbMeterNormalizer`): per-output-stream peak level tracking, updated
   every 40ms in the driver loop.

6. **Graph lifecycle**: `start_stream`, `stop_stream`,
   `can_start_stream`, `update()` tick. Our code wraps this in a 10ms
   poll loop with restart logic and backoff.

### What we built on top

- **`AudioDriver`**: a dedicated OS thread running a 10ms poll loop.
  Handles message-driven creation/removal of input and output streams,
  device switching (stop -> reconfigure -> start), stream restart with
  exponential backoff, AEC delay updates, and peak meter polling.

- **AEC integration**: two custom firewheel `AudioNode` implementations
  (`AecRenderNode`, `AecCaptureNode`) that run inside the audio graph's
  real-time callback. Each collects samples in ring buffers, processes
  10ms frames through `sonora::AudioProcessing`, and outputs the result.
  The `AecProcessor` wrapper around sonora is clean and independent of
  firewheel.

- **Device management**: resolution of platform-specific defaults (Linux
  pipewire), switching devices at runtime, fallback to default on
  disconnect.

- **Drop-based cleanup**: `StreamDropGuard` sends a message to the
  driver thread to remove graph nodes when the last handle is dropped.

### What sonora provides

`sonora` is a pure-Rust WebRTC audio processing library. It provides
echo cancellation, noise suppression, and automatic gain control. It
has no stream I/O — it takes 10ms frames of f32 samples and returns
processed frames. The `AecProcessor` in `aec.rs` already wraps it
directly, using it through `process_capture_f32_with_config` and
`process_render_f32_with_config`. The firewheel nodes are just
adapters that collect samples into 10ms frames and feed them to sonora.

Sonora stays. It does exactly what we need for AEC and noise
suppression. The refactor removes the firewheel adapter layer, not
sonora itself.

## Replacement design

### Architecture

Replace firewheel's graph with a simpler model: cpal streams connect
directly to ring buffers, with AEC and peak metering applied inline
in the cpal callbacks. No graph, no node scheduling, no port wiring.

```
                   ┌─────────────┐
  cpal input  ──>  │ ring buffer │ ──>  AEC capture  ──>  InputStream.pop_samples()
                   └─────────────┘
                   ┌─────────────┐
  OutputStream.push_samples()  ──>  │ ring buffer │ ──>  AEC render + peak ──>  cpal output
                                    └─────────────┘
```

The driver thread's 10ms poll loop goes away. Instead:
- cpal's real-time callbacks read/write ring buffers directly.
- AEC processing runs inside the callbacks (10ms frame accumulation,
  same as the current firewheel node processors).
- Peak metering runs inside the output callback.
- The `AudioDriver` thread becomes a lightweight message handler for
  device switching and stream lifecycle, woken by channel messages
  rather than polling.

### Component breakdown

#### 1. cpal stream management (replace firewheel CpalBackend)

Direct use of `cpal::Host`, `cpal::Device`, `cpal::Stream`. Open
input/output streams with `build_input_stream_raw` /
`build_output_stream_raw` (f32 format). Device enumeration already
uses `CpalBackend::enumerator()` which is a thin wrapper — replace
with `cpal::default_host()` and `cpal::available_hosts()`.

cpal 0.17 supports Android via JNI + NDK directly. The `ndk-context`
initialization in the android demo already sets up the JVM pointer
that cpal needs. The problem is firewheel's wrapper around cpal, not
cpal itself.

Estimated: ~150 lines for stream creation, device enumeration, config
building. Replaces ~100 lines of current `build_cpal_config` +
`resolve_devices` + `list_inputs` + `list_outputs`.

#### 2. Ring buffers (replace StreamWriter/StreamReaderNode)

Use `ringbuf` (already a transitive dependency via firewheel) or
`rtrb` for lock-free SPSC ring buffers between application threads
and cpal callbacks. Each stream gets one ring buffer.

The current `StreamWriterState` / `StreamReaderState` provide:
- `push_interleaved` / `read_interleaved`
- Underflow/overflow detection
- Pause/resume
- `is_ready` / `is_active` flags

Replace with a thin wrapper around a SPSC ring buffer that tracks
overflow/underflow counters and a paused flag. The `InputStream` and
`OutputStream` public types keep their current API.

Estimated: ~200 lines for the ring buffer wrapper with
overflow/underflow tracking, pause, and interleaved push/pull.

#### 3. Resampling (replace firewheel's built-in resampling)

Use `fixed-resample` directly. It is already an indirect dependency
(firewheel-cpal depends on it). The `FixedResampler` type handles
fixed-ratio resampling suitable for bridging application sample rate
(48 kHz) to device native rate.

Resampling runs in the cpal callback: for output, resample from
application rate to device rate after reading from the ring buffer.
For input, resample from device rate to application rate before
writing to the ring buffer.

Alternatively, use `fixed-resample`'s `ResamplingProd`/`ResamplingCons`
channel pair, which combines the ring buffer and resampler into a
single real-time-safe construct. This may eliminate the need for a
separate ring buffer wrapper entirely.

Estimated: ~80 lines if using `ResamplingProd`/`ResamplingCons`
(which subsume the ring buffer), or ~120 lines if using
`FixedResampler` + separate ring buffers.

#### 4. AEC processing (keep sonora, remove firewheel nodes)

The `AecProcessor` type and its sonora usage stay unchanged. The
firewheel `AudioNode` / `AudioNodeProcessor` implementations
(`AecRenderNode`, `AecCaptureNode`) get replaced with plain functions
that run inside the cpal callbacks.

Each callback accumulates samples in a `VecDeque` (same pattern as
now), processes 10ms frames through `AecProcessor`, and drains the
output ring. The current `RenderProcessor` and `CaptureProcessor`
are ~90 lines each — the replacements will be similar in size since
the algorithm is identical, just without the firewheel trait
boilerplate.

Estimated: ~150 lines total for both render and capture processing
functions (down from ~350 lines of firewheel node implementations).
The `AecProcessor` module (~160 lines) stays as-is.

#### 5. Peak meters (replace firewheel PeakMeterNode)

The current implementation uses firewheel's `PeakMeterNode` (a graph
node that computes per-channel peak dB) and `PeakMeterSmoother`
(exponential smoothing on a timer). The node runs in the audio
callback; the smoother runs in the driver thread.

Replace with inline peak computation in the output cpal callback:
compute `max(abs(sample))` per channel per callback invocation, store
in an `AtomicU32` (bit-cast f32). The `PeakMeterSmoother` can be
replaced with a simple exponential decay applied when reading the
peak value, or we can keep using firewheel's `PeakMeterSmoother` type
standalone (it does not depend on the graph).

`DbMeterNormalizer` is a trivial dB-to-normalized mapping — rewrite
in ~10 lines if we drop the firewheel dependency entirely.

Estimated: ~50 lines for peak computation + smoothing + normalization.

#### 6. Driver thread and device management

The current `AudioDriver` thread runs a 10ms poll loop that calls
`cx.update()`, drains messages, updates peak meters, and handles
restarts. With direct cpal streams, `cx.update()` disappears.

The driver thread becomes a message-driven loop (block on channel
receive, no polling). Device switching stops the old cpal streams,
creates new ones with the new device, and reconnects ring buffers.
Restart logic stays similar but operates on cpal `Stream` objects
directly rather than through firewheel.

Estimated: ~250 lines for the driver thread (down from ~450 lines
currently, since graph wiring and node management disappear).

#### 7. Public API (unchanged)

`AudioBackend`, `InputStream`, `OutputStream`, `AudioDevice`,
`DeviceId`, `AudioBackendOpts` keep their current signatures. The
`AudioStreamFactory`, `AudioSource`, `AudioSink`, and
`AudioSinkHandle` trait implementations stay identical. Callers see
no change.

## Estimated scope

| Component | Current lines | New lines | Notes |
|-----------|--------------|-----------|-------|
| cpal stream management | ~100 | ~150 | More explicit, but no graph wiring |
| Ring buffers + resampling | ~200 (firewheel nodes) | ~100 | `ResamplingProd`/`ResamplingCons` |
| AEC callback adapters | ~350 | ~150 | Same algorithm, less boilerplate |
| Peak meters | ~40 (firewheel node) | ~50 | Inline in callback |
| Driver thread | ~450 | ~250 | Message-driven, no poll loop |
| `AecProcessor` (sonora) | ~160 | ~160 | Unchanged |
| Public types + API | ~250 | ~250 | Unchanged |
| **Total** | **~1,550** | **~1,110** | ~30% reduction |

The net result is fewer lines with fewer abstractions and no external
graph engine dependency.

## Risks

### cpal Android support

cpal 0.17 supports Android through JNI + NDK, not Oboe. This is the
same path firewheel uses, so if firewheel's wrapper is the problem
(likely — it calls `start_stream` before the graph is ready, or
misconfigures the cpal backend), direct cpal usage should work. If
cpal itself is broken on Android, we would need to patch cpal or use
`oboe-rs` directly. The risk is low: cpal's Android backend is used
by other projects successfully.

### Real-time safety in callbacks

cpal callbacks run on a real-time audio thread. The current firewheel
nodes already do allocation-free processing (ring buffers, pre-sized
temp vectors). The replacement must maintain this discipline: no
allocation, no locks that could contend, no syscalls. The `AecProcessor`
uses a `Mutex`, but it is only contended during `set_stream_delay`
calls (rare, device-switch time), never during frame processing
within a single callback.

### Resampling quality

`fixed-resample` uses `rubato` internally, which provides high-quality
sinc resampling. This matches or exceeds what firewheel provides (it
also uses `fixed-resample`). No regression expected.

### Losing firewheel's graph flexibility

The current code does not use firewheel's graph flexibility: the
topology is fixed (input -> AEC capture -> readers; writers -> peak ->
AEC render -> output). Replacing it with hardcoded signal flow loses
nothing we use today. If we later need a flexible graph (e.g., mixing
multiple outputs before AEC, or adding an equalizer), we would need
to build that explicitly. This is unlikely for a real-time
communication application — mixing happens at the subscriber level,
not the audio output level.

### Testing

The audio backend has no unit tests today (it requires real audio
hardware). The refactor should add tests using cpal's null backend or
mock ring buffers to verify resampling, AEC frame accumulation, and
peak metering in isolation.

## Phased approach

### Phase 1: Direct cpal streams with ring buffers

Replace `FirewheelContext` + `CpalBackend` with direct cpal stream
creation. Replace `StreamWriterNode` / `StreamReaderNode` with
`fixed-resample`'s `ResamplingProd` / `ResamplingCons` (or plain
SPSC ring buffers + `FixedResampler`). Remove the graph, keep AEC
and peak meters stubbed to pass-through.

**Goal**: audio input and output work on Linux and Android without
firewheel. AEC temporarily disabled (pass-through). Peak meters
report zero.

**Estimated effort**: 1-2 days.

### Phase 2: Inline AEC processing

Move the AEC frame accumulation logic from the firewheel node
processors into plain functions called from the cpal callbacks.
`AecProcessor` (sonora) stays unchanged.

**Goal**: AEC works exactly as before. The `aec.rs` file shrinks from
511 lines to ~200 (the `firewheel_nodes` module disappears, the
`processor` module stays).

**Estimated effort**: half a day.

### Phase 3: Peak meters and cleanup

Add inline peak computation in the output callback. Rewrite
`DbMeterNormalizer` (or extract it from firewheel if it has a
standalone path). Remove the firewheel dependency from `Cargo.toml`.
Clean up the driver thread to be message-driven.

**Goal**: feature parity with current implementation. firewheel fully
removed from the dependency tree.

**Estimated effort**: half a day.

### Phase 4: Verification

Test on Linux (pipewire, ALSA). Test on Android. Verify AEC,
resampling, device switching, peak meters, and restart-on-error all
work. Add unit tests for the ring buffer and resampling paths using
synthetic audio.

**Estimated effort**: 1 day.

**Total estimated effort**: 3-4 days.

## Whether sonora could replace more

Sonora is a pure audio processing library — it handles echo
cancellation, noise suppression, and gain control. It does not provide
audio I/O, stream management, resampling, or device enumeration. It
cannot replace cpal or the ring buffer infrastructure. It stays as the
AEC/noise-suppression engine, unchanged by this refactor.

## Dependencies after refactor

**Removed**: `firewheel` (and its transitive tree: firewheel-core,
firewheel-graph, firewheel-cpal, firewheel-nodes, firewheel-pool,
firewheel-macros, firewheel-rtaudio, firewheel-symphonium,
bevy_platform, thunderdome).

**Added**: `cpal` as a direct dependency (currently indirect via
firewheel-cpal). `fixed-resample` as a direct dependency (currently
indirect via firewheel-cpal and firewheel-nodes). Optionally `ringbuf`
or `rtrb` if not using `fixed-resample`'s built-in channel.

**Kept**: `sonora` (AEC + noise suppression).

The dependency tree shrinks substantially. `cpal` and `fixed-resample`
are already compiled today as transitive dependencies, so build times
should improve.

---

# Detailed implementation plan

Written 2026-03-19 after a full end-to-end pipeline review (see
`audio-review.md` for the status quo analysis).

We are switching to cpal git main (upcoming 0.18). Key API changes from
0.17: `StreamConfig` is `Copy` (passed by value), error enums are
`#[non_exhaustive]`, new `DeviceDescription` with structured metadata,
`DeviceId` is `Display + FromStr` with `host:id` format,
`HostTrait::device_by_id()` exists, `DeviceBusy` error variant added,
native PipeWire and PulseAudio host backends, `StreamTrait::buffer_size()`.

## Design principles

1. **Internal 48 kHz stereo.** All processing (AEC, mixing, peak metering)
   runs at 48 kHz stereo. Resampling and channel conversion happen at the
   boundary: `fixed-resample` channels between caller/device and internal
   format. Callers can request any sample rate and channel count — the
   resampling channel handles the conversion transparently.

2. **No allocations on the real-time thread.** The cpal callback must be
   allocation-free. All buffers are pre-allocated. The only data structures
   touched are lock-free ring buffers (`ringbuf` via `fixed-resample`) and
   atomics. The AEC processor's `Mutex` is only locked in the callback
   (uncontended — the only other lock site is `set_stream_delay` which
   happens during device switches, never concurrently with callbacks).

3. **AEC as close to hardware as possible.** Render-side AEC processes
   samples in the output callback *after* reading from the ring buffer and
   *before* writing to the device. Capture-side AEC processes in the input
   callback *after* reading from the device and *before* writing to the ring
   buffer. This minimizes the time offset between the AEC reference signal
   and the echo, maximizing cancellation quality.

4. **Graceful degradation.** ALSA xruns (BufferUnderrun) are logged at warn
   level but never kill the pipeline. The ring buffer's autocorrect inserts
   silence when starved and drops samples when flooded. The caller sees
   smooth audio with occasional glitches under stress, never silence.

5. **Dynamic AEC kill switch.** `AudioBackend::set_aec_enabled(bool)` sets
   an `AtomicBool` checked in both callbacks. When disabled, callbacks pass
   audio through without touching sonora. No restart needed.

## Architecture

```
                        48 kHz stereo (internal)
                     ┌──────────────────────────────┐
  cpal input ──────> │  AEC capture  →  input ring  │ ──> ResamplingCons ──> InputStream.pop_samples()
  (device rate,      │  (10ms frames,   (lock-free   │     (resamples to
   device channels)  │   sonora)         SPSC)       │      caller rate+ch)
                     └──────────────────────────────┘

                     ┌──────────────────────────────┐
  OutputStream ────> │  ResamplingProd → output ring │ ──> AEC render  ──> cpal output
  .push_samples()    │  (resamples from  (lock-free  │     (10ms frames,   (device rate,
  (caller rate+ch)   │   caller rate)    SPSC)       │      sonora)         device channels)
                     └──────────────────────────────┘
```

Each output stream gets its own `ResamplingProd → ring → cons` channel.
The output callback reads from *all* active output ring buffers, sums
(mixes) them into a stereo mix buffer, runs AEC render on the mix, then
writes to the cpal output buffer with channel mapping.

Each input stream gets its own `prod → ring → ResamplingCons` channel.
The input callback reads from the cpal input buffer, runs AEC capture,
then writes the processed stereo audio into *all* active input ring
buffers.

### Channel mapping

Callers request 1 or 2 channels. Devices provide 1 or 2 channels.
Internal processing is always stereo (2 channels at 48 kHz).

**Output path** (caller → device):
- Mono caller → internal stereo: duplicate L to L+R
- Stereo caller → internal stereo: direct
- Internal stereo → mono device: average L+R
- Internal stereo → stereo device: direct

**Input path** (device → caller):
- Mono device → internal stereo: duplicate
- Stereo device → internal stereo: direct
- Internal stereo → mono caller: average L+R
- Internal stereo → stereo caller: direct

`fixed-resample` does not support asymmetric channel counts — the
`resampling_channel` function takes a single `num_channels` shared by
producer and consumer. All resampling channels are created at stereo
(2 channels), matching the internal processing format. Channel conversion
is explicit at each boundary:

- **Output caller boundary**: `OutputStream::push_samples()` receives
  mono or stereo from the caller. If mono, duplicate L→L+R into a small
  stack buffer before calling `prod.push_interleaved()`.
- **Input caller boundary**: `InputStream::pop_samples()` reads stereo
  from the cons. If the caller wants mono, average L+R after
  `cons.read_interleaved()`.
- **Output device boundary**: After AEC render, map stereo→device
  channels in the output callback (average for mono device, direct for
  stereo).
- **Input device boundary**: Before AEC capture, map device→stereo in
  the input callback (duplicate for mono device, direct for stereo).

### Mixing multiple output streams

When multiple `OutputStream`s exist (e.g., video call audio + notification
sound), the output callback must mix them. The mix buffer is pre-allocated
at `[f32; MAX_BLOCK * 2]` where `MAX_BLOCK` is the maximum callback size
(typically 1024 frames). Each ring buffer is read into a per-stream
scratch buffer, then added sample-by-sample into the mix buffer. After
mixing, the result goes through AEC render, peak metering, and channel
mapping to the cpal output. Mix and scratch buffers are pre-allocated to
4096 frames (8192 stereo samples) to handle large callback sizes on
WASAPI exclusive mode and PipeWire with large quantum. If a callback
exceeds this, process in chunks of 4096 frames.

Clipping protection: after mixing, clamp each sample to `[-1.0, 1.0]`.
Proper gain staging (per-stream volume) is a future concern.

## Phase breakdown

### Phase 1: Core infrastructure (~350 LOC)

**Goal**: cpal streams running, ring buffers connected, audio passes
through. AEC disabled (pass-through). Peak meters report zero.

**Files touched**:
- `moq-media/src/audio_backend.rs` — full rewrite
- `moq-media/Cargo.toml` — replace firewheel deps with `cpal` (git),
  `fixed-resample`, `ringbuf`

**Work**:

1. **cpal stream management** (~80 LOC). Create `CpalStreams` struct
   holding `cpal::Stream` handles for input and output. Build streams with
   `device.build_output_stream()` / `device.build_input_stream()` using
   f32 format. Register error callbacks that send `StreamError` to a
   bounded mpsc channel (capacity 4). Streams start paused
   (`stream.pause()`); the driver starts them after ring buffers are
   connected.

2. **Device enumeration** (~60 LOC). `list_inputs()`, `list_outputs()`
   using cpal 0.18's `DeviceDescription` for names. `resolve_devices()`
   for platform-specific defaults (PipeWire on Linux). Use
   `host.device_by_id()` for saved device IDs.

3. **Ring buffer channels** (~40 LOC). For each output stream: create a
   `resampling_channel::<f32, 2>(channels, caller_rate, 48000, config)`
   pair. The producer lives with the `OutputStream` handle; the consumer
   lives in the output callback's shared state. For input streams: create
   a channel with `(48000, caller_rate)` — the producer lives in the
   input callback; the consumer lives with `InputStream`.

   Channel config for network audio output:
   ```rust
   ResamplingChannelConfig {
       latency_seconds: 0.3,
       capacity_seconds: 3.0,
       underflow_autocorrect_percent_threshold: Some(25.0),
       overflow_autocorrect_percent_threshold: Some(75.0),
       ..Default::default()
   }
   ```

4. **Output callback** (~80 LOC). Pre-allocate `mix_buf: [f32; 8192]`
   and `stream_buf: [f32; 8192]` (4096 frames stereo) in the callback
   closure. Each tick:
   - Zero the mix buffer for `num_frames * 2` samples
   - For each active output stream cons: `cons.read()` into
     `stream_buf`, add to `mix_buf`
   - Clamp mix to `[-1.0, 1.0]`
   - (Phase 2: AEC render here)
   - (Phase 3: peak metering here)
   - Write mix to cpal output buffer with channel mapping (stereo→mono
     average if device is mono, stereo→stereo direct)
   - If callback delivers >4096 frames, process in chunks

   The list of active consumers is stored in an `ArcSwap<Vec<Cons>>`.
   The driver thread clones the current Vec, modifies the clone, and
   calls `store()`. The callback calls `load()` — lock-free, no
   priority inversion risk. This eliminates the Mutex concern entirely.

5. **Input callback** (~60 LOC). Read from cpal input, channel-map to
   stereo, (Phase 2: AEC capture), write to all active input stream
   producers. Same `ArcSwap<Vec<Prod>>` pattern as output.

6. **Pre-fill on stream start** (~10 LOC). After creating a resampling
   channel for an output stream, push `latency_seconds` worth of
   silence (0.3s × 48000 × 2 = 28800 samples) into the producer before
   making it available to the callback. This ensures the first callback
   finds data and prevents immediate underrun.

7. **Driver thread** (~100 LOC). Message-driven loop using
   `mpsc::Receiver::recv()` (blocking, no polling). Handles:
   - `CreateOutputStream` → create resampling channel, add cons to output
     callback's list, return `OutputStream` handle
   - `CreateInputStream` → create resampling channel, add prod to input
     callback's list, return `InputStream` handle
   - `RemoveStream` → remove from callback list by ID
   - `SwitchDevice` → stop streams, create new streams with new device,
     re-register all ring buffer consumers/producers
   - `CpalError` → log at warn for BufferUnderrun, restart only on
     DeviceNotAvailable

   **Error handling**: the error callback sends `StreamError` to a
   separate mpsc channel. The driver thread checks this channel on each
   message receive (via `try_recv`). `BufferUnderrun` → warn log, no
   action (cpal's own recovery handles it). `DeviceNotAvailable` →
   attempt restart with backoff. `StreamInvalidated` → full restart.

8. **Pause/resume declicker** (~30 LOC). When pausing, apply a 3ms
   linear fade-out in the output callback. When resuming, apply a 3ms
   fade-in. Track state with an atomic enum (Playing, FadingOut,
   Paused, FadingIn). The callback checks this per-stream and applies
   the gain ramp sample-by-sample. Without this, pausing mid-callback
   produces an audible click.

9. **Public types** (~30 LOC). `OutputStream`, `InputStream` with same
   public API as current. `push_samples()` calls
   `prod.push_interleaved()` and logs `PushStatus` results.
   `pop_samples()` calls `cons.read_interleaved()` and handles
   `ReadStatus`.

### Phase 2: AEC integration (~200 LOC)

**Goal**: echo cancellation works. Dynamic enable/disable.

**Files touched**:
- `moq-media/src/audio_backend.rs` — add AEC to callbacks
- `moq-media/src/audio_backend/aec.rs` — remove `firewheel_nodes` module,
  keep `processor` module unchanged, add callback helpers

**Work**:

1. **AEC architecture — serialize on input callback** (~100 LOC).

   The current firewheel graph processes render and capture AEC on a
   single thread. Moving to separate cpal callbacks would introduce
   Mutex contention between input and output callbacks — both lock
   `AecProcessor`'s inner `Mutex<AudioProcessing>`.

   Solution: the **output callback does not call sonora**. It writes
   the mixed stereo output into a lock-free SPSC "render reference"
   ring buffer (`ringbuf::HeapRb`). The **input callback** reads the
   render reference, calls `process_render_f32` then
   `process_capture_f32` sequentially on the same thread — no
   contention. This matches firewheel's single-threaded model.

   ```rust
   // Owned by input callback only — no Mutex needed
   struct AecState {
       processor: AecProcessor,
       render_ref_cons: ringbuf::HeapCons<f32>,  // from output callback
       render_buf: [VecDeque<f32>; 2],
       capture_buf: [VecDeque<f32>; 2],
       frame_size: usize,  // 480 at 48kHz
       temp_src: [Vec<f32>; 2],
       temp_dst: [Vec<f32>; 2],
       enabled: Arc<AtomicBool>,
   }
   ```

   The output callback writes mixed stereo interleaved into
   `render_ref_prod` (lock-free SPSC push). The input callback
   drains `render_ref_cons` into `render_buf`, processes render
   frames, then processes capture frames, all single-threaded.

   Pre-allocate all VecDeques to `max(frame_size * 4, 8192)` and
   temp vectors to `frame_size`. The render reference ring buffer
   capacity is 4800 samples (100ms at 48kHz stereo) — enough to
   absorb callback timing jitter between input and output.

2. **Integrate into callbacks** (~40 LOC). The output callback: after
   mixing all streams, write the stereo mix into the render reference
   ring buffer, then write to cpal output (the output callback does NOT
   call sonora). The input callback: drain the render reference ring
   buffer, process render frames, then read from cpal input, channel-map
   to stereo, process capture frames, and write to input ring buffers.

3. **AEC kill switch** (~20 LOC). `AudioBackend::set_aec_enabled(bool)`
   stores to `Arc<AtomicBool>` shared with both callback states. The
   `process_render` / `process_capture` functions check this flag before
   calling sonora. When disabled, the accumulation buffers still drain
   (to prevent stale data if re-enabled), but sonora is not called.

4. **Stream delay tracking** (~30 LOC). When streams start or devices
   switch, compute `input_to_output_latency` from cpal's
   `OutputCallbackInfo::timestamp()` and `InputCallbackInfo::timestamp()`.
   Call `processor.set_stream_delay()`. Update when the delta changes
   by >1ms.

5. **Remove firewheel_nodes module** (~30 LOC net deletion). Delete the
   `firewheel_nodes` module from `aec.rs` (~350 lines). Keep the
   `processor` module unchanged (~160 lines). Total `aec.rs` shrinks
   from 511 to ~160 lines.

### Phase 3: Peak metering and polish (~100 LOC)

**Goal**: feature parity. firewheel fully removed.

**Files touched**:
- `moq-media/src/audio_backend.rs` — add peak metering
- `moq-media/Cargo.toml` — remove firewheel, confirm cpal + fixed-resample

**Work**:

1. **Inline peak metering** (~40 LOC). After AEC render in the output
   callback, compute `max(abs(sample))` per channel across the buffer.
   Store as `AtomicU32` (f32 bit-cast). On read, apply exponential
   smoothing and dB normalization:

   ```rust
   struct PeakState {
       peak_l: AtomicU32,  // f32 bits
       peak_r: AtomicU32,
   }
   ```

   The smoother runs on the reader side (UI thread) — no real-time
   concern. The callback only does `store(Relaxed)`.

2. **DbMeterNormalizer replacement** (~20 LOC). Port the three-field
   normalizer (min_db, max_db, knee_db) from firewheel or rewrite:
   `(db - min) / (max - min)` clamped to `[0.0, 1.0]`.

3. **PeakMeterSmoother replacement** (~30 LOC). Exponential decay:
   `smoothed = smoothed * decay + (1 - decay) * peak`. Reset peak
   atomics after read. Decay factor based on update interval (40ms
   default).

4. **Remove firewheel** (~10 LOC). Remove `firewheel` and all sub-crates
   from `Cargo.toml`. Replace with:
   ```toml
   cpal = { git = "https://github.com/RustAudio/cpal", branch = "main" }
   fixed-resample = { version = "0.9", features = ["resampler"] }
   ```

### Phase 4: Error handling and resilience (~80 LOC)

**Goal**: butter-smooth audio under all conditions.

**Files touched**:
- `moq-media/src/audio_backend.rs` — error handling improvements
- `moq-media/src/pipeline.rs` — decode thread buffer awareness

**Work**:

1. **CPAL error handling** (~30 LOC). The error callback sends
   `StreamError` to a bounded channel. The driver thread processes them:
   - `BufferUnderrun`: warn log with timestamp and buffer occupancy.
     No action — cpal 0.18 auto-recovers ALSA xruns.
   - `DeviceNotAvailable`: attempt device fallback or restart with
     backoff (500ms → 1s → 2s → 4s, reset on success).
   - `StreamInvalidated`: full restart.
   - `DeviceBusy` (on stream build): retry with backoff.

2. **Underrun rate limiting** (~20 LOC). Track underrun count with a
   sliding window (AtomicU32 + timestamp). If >20 underruns in 5
   seconds, escalate to stream restart with larger buffer size
   (double `latency_seconds`).

3. **Buffer health logging** (~30 LOC). In `OutputStream::push_samples`,
   check `prod.occupied_seconds()`:
   - < 50ms: warn log "audio buffer critically low"
   - < 100ms: debug log "audio buffer low"
   - > 2.5s: warn log "audio buffer nearly full"

   In the audio decode loop (`pipeline.rs`), when `push_samples` returns
   and the sink is an OutputStream, check buffer health. If critically
   low and no packets available, push a frame of silence to prevent
   underrun.

### Phase 5: Device switching improvements (~60 LOC)

**Goal**: seamless device switches without audio dropout.

**Files touched**:
- `moq-media/src/audio_backend.rs` — device switch logic

**Work**:

1. **Atomic stream swap** (~40 LOC). Instead of stopping/starting
   streams in sequence, build the new cpal streams *before* stopping
   the old ones. The callback's consumer/producer lists use `ArcSwap`
   (from Phase 1), so the swap is lock-free. The old streams are
   dropped after the swap, which stops the old callbacks.

   If the new device has a different sample rate, clear all ring
   buffers (brief silence gap) and recreate the resampling channels
   with the new rate before swapping. If rates match, preserve the
   existing ring buffers for seamless transition.

2. **Crossfade** (~20 LOC). When swapping streams, the new callback
   starts with a 10ms linear fade-in from silence. This prevents clicks
   from the abrupt start. The old callback gets a 10ms fade-out before
   being dropped.

### Phase 6: Testing (~150 LOC)

**Goal**: automated tests for the non-hardware parts.

**Files touched**:
- `moq-media/src/audio_backend.rs` — `#[cfg(test)]` module
- `moq-media/tests/audio_backend.rs` — integration tests (optional)

**Work**:

1. **Ring buffer + resampling** (~40 LOC). Create a resampling channel,
   push known sine wave, read back, verify frequency/amplitude within
   tolerance.

2. **AEC pass-through** (~30 LOC). Create `AecProcessor` with disabled
   flag, push stereo frames, verify output equals input.

3. **Channel mapping** (~40 LOC). Test mono→stereo duplication,
   stereo→mono averaging, identity mapping.

4. **Peak metering** (~20 LOC). Push known signal, verify peak values.

5. **PushStatus handling** (~20 LOC). Overflow and underflow scenarios
   with small ring buffers.

## Line count estimates

| Phase | New LOC | Deleted LOC | Net change |
|-------|---------|-------------|------------|
| 1: Core | ~350 | ~800 | -450 |
| 2: AEC | ~200 | ~350 | -150 |
| 3: Peak | ~100 | ~40 | +60 |
| 4: Errors | ~80 | ~50 | +30 |
| 5: Device | ~60 | ~30 | +30 |
| 6: Tests | ~150 | 0 | +150 |
| **Total** | **~940** | **~1270** | **-330** |

Final `audio_backend.rs` + `aec.rs`: ~950 lines (down from ~1620).

## Files touched summary

- `moq-media/src/audio_backend.rs` — full rewrite (phases 1-5)
- `moq-media/src/audio_backend/aec.rs` — remove firewheel_nodes, keep
  processor, add callback helpers (phase 2)
- `moq-media/Cargo.toml` — swap deps (phase 3)
- `moq-media/src/pipeline.rs` — buffer health in decode loop (phase 4)
- `moq-media/src/lib.rs` — possible re-export changes (minor)

## Dependencies after refactor

**Remove**: `firewheel` (entire tree: firewheel-core, firewheel-graph,
firewheel-cpal, firewheel-nodes, firewheel-pool, firewheel-macros,
bevy_platform, thunderdome).

**Add**:
```toml
cpal = { git = "https://github.com/RustAudio/cpal", branch = "main" }
fixed-resample = { version = "0.9", features = ["resampler"] }
```

**Keep**: `sonora` (AEC + noise suppression).

## Implementation checklist

### Phase 1: Core infrastructure
- [x] cpal 0.18 (git main) dependency added, firewheel removed from Cargo.toml
- [x] `fixed-resample` and `arc-swap` added as direct dependencies
- [x] Device enumeration with cpal 0.18 API
- [x] Platform-specific device resolution (PipeWire on Linux)
- [x] cpal output stream creation with f32 typed callbacks
- [x] cpal input stream creation with f32 typed callbacks
- [x] Error callback sends `StreamError` to bounded mpsc channel
- [x] Resampling channels created at stereo (2ch) with `ResamplingChannelConfig`
- [x] Mono→stereo duplication in `OutputStream::push_samples()` for mono callers
- [x] Stereo→mono averaging in `InputStream::pop_samples()` for mono callers
- [x] Stereo→device channel mapping in output callback
- [x] Device→stereo channel mapping in input callback
- [x] Output callback: mix multiple streams, clamp, write to device
- [x] Input callback: read from device, write to all input producers
- [x] Callback stream lists use message-passing (`sync_channel` + `try_recv`) — lock-free on data path
- [x] Mix buffer pre-allocated to 4096 frames (8192 stereo samples)
- [x] Pre-fill output ring buffer with `latency_seconds` silence on creation
- [x] Pause/resume declicker (3ms fade in/out, atomic state machine)
- [x] Driver thread: message-driven loop (blocking recv, no polling)
- [x] Driver: CreateOutputStream / CreateInputStream / RemoveStream / SwitchDevice
- [x] `PushStatus` return value logged in `push_samples()`
- [x] `ReadStatus` handled in `pop_samples()`
- [x] Public API unchanged: `AudioBackend`, `OutputStream`, `InputStream`
- [x] `OutputStream` not Clone — separate `OutputHandle` for pause/resume/metering
- [x] `cargo check` passes, `cargo clippy` clean

### Phase 2: AEC integration
- [x] Render reference SPSC ring buffer (output callback → input callback) via `ringbuf`
- [x] `AecState` struct with pre-allocated VecDeques (capacity 8192)
- [x] Output callback writes mixed stereo to render reference ring
- [x] Input callback drains render reference, processes render frames
- [x] Input callback processes capture frames (all sonora calls single-threaded)
- [x] `AudioBackend::set_aec_enabled(bool)` with `AtomicBool`
- [x] AEC bypass when disabled (pass-through, still drain accumulators)
- [x] `set_stream_delay()` API reserved (not yet wired to driver)
- [x] `firewheel_nodes` module removed from `aec.rs`
- [x] `processor` module unchanged
- [x] No tracing macros in data callbacks (use atomic counters)

### Phase 3: Peak metering and polish
- [x] Inline peak computation in output callback (`max(abs(sample))`)
- [x] `AtomicU32` peak storage (f32 bit-cast)
- [x] `PeakMeterSmoother` replacement (exponential decay, attack=0.3, release=0.05)
- [x] `DbMeterNormalizer` replacement (min/max/knee mapping, -60..0 dB)
- [x] `smoothed_peak_normalized()` returns correct values
- [x] firewheel fully removed from dependency tree
- [x] `cargo check --all-features` passes without firewheel

### Phase 4: Error handling and resilience
- [x] `BufferUnderrun` → warn log, no restart (cpal auto-recovers)
- [x] `DeviceNotAvailable` → restart with backoff (500ms→4s)
- [x] `StreamInvalidated` → full restart
- [x] `DeviceBusy` — N/A, cpal 0.18 does not expose this variant
- [x] Underrun rate limiter: >20 in 5s → restart
- [x] `PushStatus::UnderflowCorrected` logged at debug — fixed-resample auto-pads silence
- [x] `PushStatus::OverflowOccurred` logged at warn — caller producing faster than consumer drains
- [x] Silence insertion in `audio_decode_loop` after 200ms without packets (`pipeline.rs`)

### Phase 5: Device switching
- [x] Build new cpal streams before stopping old ones
- [x] Ring buffers cleared on device switch — fresh resampling channels created per tracked stream
- [x] Crossfade: fade-out on old streams (3ms via declicker), fade-in on new streams after rebuild

### Phase 6: Tests
- [x] AEC pass-through: disabled flag, output equals input
- [x] Channel mapping: mono→stereo, stereo→mono
- [x] Peak metering: known signal → correct peak values
- [x] PushStatus handling: overflow and underflow logging
- [x] Declicker: constants verified (3ms = 144 samples)

---

## Expert review

Reviewed 2026-03-18 against the detailed implementation plan, the current
`audio_backend.rs` and `aec.rs` sources, firewheel-cpal 0.10's callback
implementation, firewheel-nodes 0.10's `StreamWriterNode` processor,
`fixed-resample` 0.9.2's `resampling_channel` API, and cpal 0.18's trait
and error definitions.

### A. Real-time safety

**A1. VecDeque growth in AEC accumulators — IMPORTANT**

The plan pre-allocates `render_buf` and `capture_buf` as
`VecDeque::with_capacity(frame_size * 4)`. At 48 kHz, `frame_size` is 480,
so each VecDeque holds 1,920 samples before reallocation. A typical cpal
callback delivers 256-1024 frames. After pushing one callback's worth of
samples and draining completed 480-sample frames, the residual never exceeds
479 samples per channel — well within the 1,920 capacity.

However, the plan does not account for pathological callback sizes. Some
platforms (WASAPI, PipeWire with large quantum) can deliver 2048 or even
4096 frames in a single callback. If the callback delivers >1,920 frames
before any drain occurs, the VecDeque will reallocate. The current firewheel
implementation has the same vulnerability but is saved by firewheel's own
`max_block_frames` cap of 1024.

**Recommendation**: Pre-allocate to `max(frame_size * 4, 8192)` and add a
debug assertion that the VecDeque never exceeds capacity. Alternatively, use
a fixed-size circular buffer that drops oldest samples on overflow rather
than growing — this guarantees real-time safety at the cost of a glitch in
the pathological case, which is strictly better than an allocation.

**A2. `Arc<Mutex<Vec<Cons>>>` with `try_lock()` — IMPORTANT**

The plan acknowledges the priority inversion risk and proposes `try_lock()`
with silence fallback. This is the correct mitigation. However, the plan
does not address what happens to the *driver thread* when it modifies the
list.

The driver thread must hold the lock while it pushes or removes a consumer
from the Vec. A Vec push can reallocate. If the driver thread is mid-push
(reallocating the Vec) when the callback fires and calls `try_lock()`, the
callback gets silence. That is acceptable. But the driver thread itself
should pre-allocate the Vec to a reasonable capacity (say, 16 streams) to
minimize the window during which the lock is held.

A better long-term approach is `arc-swap` with `ArcSwap<Vec<Cons>>`: the
driver builds a new Vec, swaps atomically, and the callback reads the old
one until the next swap. This eliminates the lock entirely. The plan
mentions `ArcSwap` as an alternative but defers it. Given that this is a
real-time communication application where every callback matters, I would
make `ArcSwap` the default choice from phase 1.

**Recommendation**: Use `ArcSwap<Vec<Cons>>` instead of `Arc<Mutex<Vec<Cons>>>`.
The driver clones the current Vec, modifies the clone, and swaps. The
callback calls `load()` (lock-free) every tick. This is ~5 more lines of
code and eliminates the entire priority inversion concern.

**A3. Tracing macros in callbacks — MINOR**

The plan mentions "warn log with timestamp" for BufferUnderrun in the error
callback. The error callback runs on whatever thread cpal uses to deliver
errors, not the audio callback itself, so this is safe. However, ensure that
no `tracing::warn!` or `tracing::debug!` calls appear inside the data
callbacks (input and output). The `tracing` subscriber may allocate (e.g.,
`fmt::Layer` allocates a string for each event). The current firewheel code
has the same risk in its processor path.

**Recommendation**: Use `AtomicU64` counters for underrun/overflow events in
the callback. Log them from the driver thread on a periodic check (e.g.,
every message drain cycle).

**A4. Atomic ordering — MINOR**

`Relaxed` ordering for the peak meter `AtomicU32` stores and the AEC
`AtomicBool` enabled flag is correct. These are single-producer
single-consumer flag/value patterns where eventual visibility is sufficient.
The callback writes peak values; the UI thread reads them. A stale read by
one callback period (5ms) is imperceptible.

The existing `AecProcessor` uses `SeqCst` for `enabled` and `set_enabled`.
This is stronger than necessary — `Relaxed` would suffice — but the
overhead is negligible. The plan's use of `Relaxed` for the new
`AtomicBool` is fine.

### B. Channel mapping correctness

**B1. `fixed-resample` does NOT support asymmetric channel counts — CRITICAL**

The `resampling_channel` function takes a single `num_channels: NonZeroUsize`
parameter shared between producer and consumer. Both sides operate on the
same channel count. The plan says: "Channel conversion at the caller
boundary is handled by `ResamplingProd::push_interleaved()` /
`ResamplingCons::read_interleaved()` when the resampling channel is created
with different channel counts for producer and consumer sides." This is
incorrect. The API does not support this.

The plan then hedges: "If `fixed-resample` does not support asymmetric
channel counts, we add a thin adapter." This adapter is not optional — it
is required, and the plan needs to specify exactly where it goes and how
it works.

**Recommendation**: All resampling channels operate at stereo (2 channels),
matching the internal processing format. Channel conversion happens outside
the resampling channel:

- **Output path**: The caller pushes mono or stereo samples to
  `OutputStream::push_samples()`. If the caller is mono, duplicate L to
  L+R *before* calling `prod.push_interleaved()`. The resampling channel
  is always 2-channel.

- **Input path**: The `InputStream::pop_samples()` reads stereo from the
  resampling channel. If the caller wants mono, average L+R *after* calling
  `cons.read_interleaved()`.

- **Device boundary**: In the output callback, the mixed stereo buffer is
  channel-mapped to device format *after* AEC render. In the input callback,
  device audio is channel-mapped to stereo *before* AEC capture.

This is ~20 lines of inline code at each boundary. Add it explicitly to the
phase 1 work item.

**B2. Unnecessary resampling bypass — MINOR**

When both the caller and the device are 48 kHz stereo, `fixed-resample`
detects that `in_sample_rate == out_sample_rate` and skips the resampler
entirely (it only creates the ring buffer). The plan's architecture
introduces two resampling channels per output stream: caller-to-internal
(always 48 kHz stereo internally) and internal-to-device. When the caller
is 48 kHz stereo, the first channel is a no-op ring buffer. When the device
is 48 kHz stereo, there is no second channel at all — the callback writes
directly.

**Recommendation**: The plan should clarify that when the device is already
48 kHz, no second resampling step occurs. The callback reads from the
internal ring buffer and writes to the cpal output buffer directly. Only
when the device rate differs from 48 kHz does a resampler run inside the
callback. This resampler should be created once (not per-callback) and
owned by the callback closure. The plan implies this but does not state it.

### C. Mixing correctness

**C1. Sample-by-sample addition — correct**

Additive mixing is the standard approach for real-time audio. It is
mathematically correct for PCM signals. No issue here.

**C2. Clamping vs. normalization — correct as proposed**

Clamping to `[-1.0, 1.0]` is the right choice for real-time mixing. Dynamic
normalization (dividing by stream count) would cause level pumping when
streams are added or removed. Per-stream gain is correctly deferred to a
future concern.

**C3. Streams added/removed mid-callback — addressed by A2**

With `try_lock()`, the callback uses the last successfully locked snapshot.
If a stream was just added but the lock fails, the new stream simply misses
one callback (~5ms). This is inaudible. With `ArcSwap`, the callback always
sees a consistent snapshot. Either approach is correct.

**C4. Mix buffer sizing — IMPORTANT**

The plan pre-allocates `mix_buf: [f32; 2048]` and `stream_buf: [f32; 2048]`.
At stereo, this is 1024 frames. Most callbacks deliver 256-1024 frames, so
this is sufficient for typical cases. But cpal does not guarantee a maximum
callback size. WASAPI exclusive mode, PipeWire with large quantum, or ALSA
with a large period can deliver more than 1024 frames.

`firewheel-cpal` handles this by resizing its `input_buffer` dynamically
(line 979: `self.input_buffer.resize(num_input_samples, 0.0)`). This
allocates in the callback — a real-time safety violation that firewheel
tolerates because it is rare.

**Recommendation**: Pre-allocate to 4096 frames (8192 stereo samples). This
covers all reasonable callback sizes. If a callback delivers more, either
clamp and process in chunks of 4096 (preferred — no allocation, bounded
latency) or accept the rare allocation with a logged warning.

### D. AEC correctness

**D1. Non-multiple-of-480 callback sizes — correct as designed**

The plan's 10ms frame accumulation handles this correctly. Samples accumulate
in the VecDeque. When 480 samples per channel are available, a frame is
processed. Residual samples carry over to the next callback. This is the
same pattern as the current firewheel node implementation and is correct.

The output side has the same accumulation: processed frames go into an
output VecDeque, and the callback drains as many samples as it needs. If
fewer processed samples are available than the callback needs, the plan
should output silence for the missing samples (as the current implementation
does at lines 329-335 of `aec.rs`). The plan's `process_render` description
does not explicitly state this. Ensure the output drain handles the case
where `out_ring.len() < num_frames`.

**D2. AEC signal ordering — correct**

The plan places AEC render *after* reading from the ring buffer and *before*
writing to the device. This is correct: sonora's render path needs to see
exactly what the speaker will play, as close to the DAC as possible.

AEC capture runs *after* reading from the device and *before* writing to
the ring buffer. This is correct: sonora's capture path needs the raw
microphone signal before any other processing.

This ordering maximizes the correlation between the render reference and
the actual echo, which is the core requirement for effective cancellation.

**D3. AEC Mutex contention between input and output callbacks — CRITICAL**

The `AecProcessor` wraps `sonora::AudioProcessing` in a `Mutex`. Both
`process_render_f32` and `process_capture_f32` lock this Mutex. On
full-duplex audio systems (all modern Linux ALSA, PipeWire, WASAPI), the
input and output callbacks run on separate threads and can fire
simultaneously.

If the output callback holds the AEC Mutex while processing a render frame
(~10us for 480 samples through sonora), and the input callback fires and
tries to lock it, the input callback blocks. On a real-time audio thread,
any blocking is a potential glitch. The current firewheel implementation
avoids this because firewheel processes both input and output in a single
graph tick on a single thread — the AEC render and capture nodes never run
concurrently.

The plan moves to separate cpal callbacks for input and output, introducing
this contention that did not exist before.

**Recommendation**: Replace the `Mutex<AudioProcessing>` with two separate
sonora instances sharing state through sonora's own API, or use `try_lock()`
in both callbacks with a bypass-on-contention fallback.

The better approach: run AEC on only one of the two callbacks and use a
lock-free ring buffer to pass data to the other. Specifically:

1. The **output callback** reads from output ring buffers, mixes, and writes
   the mixed stereo into a lock-free SPSC "render reference" ring buffer.
   It does NOT call sonora.

2. The **input callback** reads from the device, reads the render reference
   from the SPSC ring buffer, calls `process_render_f32` then
   `process_capture_f32` on the same lock hold (sequential, no contention),
   and writes the processed capture to the input ring buffers.

This serializes all sonora calls on a single thread (the input callback
thread), matching the current firewheel behavior. The render reference ring
buffer introduces ~5ms of additional delay between render and capture, which
sonora's `set_stream_delay` already accounts for.

Alternatively, if the AEC `Mutex` contention is measured to be consistently
<1us (likely, given sonora's processing speed), `try_lock()` with
pass-through-on-failure is acceptable. But this must be validated with
profiling on the target platforms.

### E. Error handling

**E1. Ignoring BufferUnderrun — correct with caveat**

cpal 0.18 separates `BufferUnderrun` from `DeviceNotAvailable`. On ALSA,
cpal calls `snd_pcm_prepare()` after an xrun, which usually recovers the
stream. Ignoring the error and continuing is correct.

The caveat: if `BufferUnderrun` fires repeatedly (>10 times per second),
it indicates a systemic problem (buffer too small, CPU overloaded, device
misconfigured). The plan should add a rate limiter that escalates to a
restart after N consecutive underruns within T seconds.

**Recommendation**: Track underrun count with a sliding window. If >20
underruns in 5 seconds, attempt a stream restart with a larger buffer size.

**E2. Backoff strategy — correct**

500ms -> 1s -> 2s -> 4s with reset on success is reasonable. The
`DeviceBusy` retry on stream build is a good addition for cpal 0.18.

**E3. In-flight samples during device switch — IMPORTANT**

The plan's phase 5 proposes building new streams before stopping old ones
(atomic swap). This is the right approach. However, the ring buffers contain
samples that were resampled for the old device's sample rate. If the new
device has a different sample rate, these samples play at the wrong pitch
until the ring buffer drains.

**Recommendation**: On device switch, if the new device's sample rate
differs from the old one, clear all ring buffers (inserting a brief silence
gap) and recreate the resampling channels with the new rate. If the rates
match, the ring buffers can be preserved.

### F. Comparison with firewheel

**F1. What firewheel does better: declicking**

Firewheel's `StreamWriterNode` processor uses a `Declicker` with an
equal-power 3dB fade curve for pause/resume transitions. This prevents
clicks when streams are paused or resumed. The plan's phase 5 mentions a
10ms linear fade-in/fade-out for device switches, but does not mention
declicking for pause/resume. The current `OutputStream::pause()` calls
`handle.pause_stream()` which triggers firewheel's declicker.

**Recommendation** (IMPORTANT): Add a simple linear fade-out (2-5ms) when
pausing and fade-in when resuming. A `Declicker` state machine is ~30 lines.
Without this, pausing an output stream mid-callback will produce an audible
click.

**F2. What firewheel does better: silence detection**

Firewheel's writer node optionally checks for silence in the output and
propagates a silence mask through the graph. This allows downstream nodes
to skip processing when the input is silent. The plan does not replicate
this, which is fine — with direct cpal callbacks and only AEC+peak-meter
processing, the savings from silence detection are negligible (AEC already
has its own silence detection via sonora's VAD).

**F3. What firewheel does wrong: treating xruns as fatal**

The `audio-review.md` documents this thoroughly. Firewheel's
`poll_status()` propagates any cpal `StreamError` as a fatal graph error.
The plan's direct cpal approach with error discrimination
(BufferUnderrun vs. DeviceNotAvailable vs. StreamInvalidated) is a
significant improvement.

**F4. What firewheel does wrong: Mutex around ResamplingProd**

Firewheel wraps `ResamplingProd` in `Arc<Mutex<>>` (line 328 of writer.rs:
`prod: Arc<Mutex<fixed_resample::ResamplingProd<f32, MAX_CHANNELS>>>`).
The plan's direct approach puts the producer in the `OutputStream` handle
(no Mutex needed — single-writer from the decode thread) and the consumer
in the callback closure. This is cleaner and avoids the Mutex contention
that exists in the current architecture.

**F5. Behaviors to replicate explicitly**

1. **Pre-fill with latency padding**: Firewheel/fixed-resample fills the
   ring buffer with `latency_seconds` worth of zeros on creation. The plan
   correctly replicates this in phase 4 (pre-fill on stream start). Make
   sure this happens in phase 1, not phase 4 — without pre-fill, the very
   first callback will underrun.

2. **`is_ready()` / `channel_started` handshake**: Firewheel's writer node
   has a two-phase readiness protocol: the producer does not push until
   `channel_started` is set by the consumer. The `fixed-resample` channel
   has the same mechanism via `input_stream_ready` / `output_stream_ready`
   AtomicBools. The plan should ensure it uses these (they are built into
   `ResamplingProd::push_interleaved()` — it returns `OutputNotReady` until
   the consumer has called `read` at least once).

### Summary of findings

| # | Issue | Severity | Section |
|---|-------|----------|---------|
| A1 | VecDeque may grow under pathological callback sizes | IMPORTANT | A |
| A2 | `Mutex<Vec<Cons>>` — use ArcSwap instead | IMPORTANT | A |
| A3 | No tracing in data callbacks | MINOR | A |
| A4 | Relaxed atomics correct | OK | A |
| B1 | fixed-resample does not support asymmetric channel counts | CRITICAL | B |
| B2 | Clarify no-op resampling when device is 48 kHz | MINOR | B |
| C1 | Additive mixing correct | OK | C |
| C2 | Clamping correct | OK | C |
| C3 | Mid-callback stream changes handled | OK | C |
| C4 | Mix buffer may be too small for large callbacks | IMPORTANT | C |
| D1 | Frame accumulation handles non-480 callbacks | OK | D |
| D2 | AEC signal ordering correct | OK | D |
| D3 | AEC Mutex contention between input/output callbacks | CRITICAL | D |
| E1 | BufferUnderrun handling needs rate limiting | MINOR | E |
| E2 | Backoff strategy correct | OK | E |
| E3 | In-flight samples at wrong rate after device switch | IMPORTANT | E |
| F1 | Missing declicker for pause/resume | IMPORTANT | F |
| F5.1 | Pre-fill must happen in phase 1, not phase 4 | IMPORTANT | F |
| F5.2 | Preserve fixed-resample readiness handshake | MINOR | F |

### Verdict: APPROVED WITH CHANGES

The plan is sound in its overall architecture and phasing. The motivation
is correct, the dependency reduction is significant, and the direct cpal
approach solves real problems that firewheel introduces. Two issues require
resolution before implementation begins:

1. **B1 (CRITICAL)**: The channel mapping layer must be explicitly designed,
   since `fixed-resample` does not support asymmetric channel counts. All
   resampling channels must be stereo; mono conversion happens outside them.

2. **D3 (CRITICAL)**: The AEC Mutex contention between separate input and
   output callbacks is a new concurrency hazard that the current
   single-threaded firewheel graph does not have. Either serialize all sonora
   calls onto one callback thread (render reference ring buffer approach) or
   use `try_lock()` with measured validation.

The IMPORTANT issues (A1, A2, C4, E3, F1, F5.1) should be addressed during
implementation but do not require plan revision — they are implementation
details that fit within the existing phase structure.

---

## Implementation review (post-implementation)

Reviewed 2026-03-18 against the completed `audio_backend.rs` (~1635 lines)
and `aec.rs` (~367 lines).

### A. Real-time safety

**A1. Heap allocations in output callback — OK**

The output callback (`output_callback`) operates on pre-allocated
`mix_buf` and `stream_buf` vectors (both `MAX_MIX_FRAMES * 2 = 8192`
elements, allocated at stream build time). No `Vec::push`, no `String`,
no `format!`. The `entries.push()` in the command drain path does
allocate if the `Vec<OutputEntry>` exceeds its capacity (pre-allocated
to 16), but this only fires when a new stream is added — not on the
audio data path. Acceptable: stream creation is rare and the Vec was
pre-allocated with headroom.

`ringbuf::HeapProd::push_slice` does not allocate — it writes into
pre-allocated ring buffer memory and returns the count of items
pushed. `ResamplingCons::read_interleaved` does not allocate either —
it reads from the internal ring buffer into the caller-provided slice.

**A2. Heap allocations in input callback — IMPORTANT**

Same analysis as output: pre-allocated `stereo_buf` and `Vec<InputEntry>`
with capacity 16. The `InputCmd::Add` variant boxes the `ResamplingProd`
(`Box::new(prod)`) on the *driver thread* (line 1312), not in the
callback. The callback unboxes it (`*prod`, line 922) which is a move,
not an allocation. Clean.

However, the AEC `VecDeque::push_back` calls in `aec.rs` lines 284-285,
317-318, 341-342, 346-347 *can* allocate if the VecDeque exceeds its
pre-allocated capacity of 8192. The render and capture accumulation
buffers drain every 480 samples, so the residual never exceeds 479
samples per channel under normal operation. A pathological callback of
4096 frames would push 4096 samples before draining, well within the
8192 capacity. The `out_buf_l`/`out_buf_r` are cleared at the start of
each `process_stereo_interleaved` call (line 322-323), so they grow to
at most `num_frames` which is bounded by `MAX_MIX_FRAMES` (4096) from
the input callback's `stereo_buf` pre-allocation. Safe in practice, but
a callback larger than 8192 frames would trigger reallocation. No
platform currently delivers callbacks this large, but a debug assertion
would catch regressions.

**A3. `std::sync::mpsc::sync_channel` `try_recv` in callbacks — OK**

`try_recv` on `std::sync::mpsc::Receiver` is non-blocking and
allocation-free. It performs a CAS loop on the internal queue. The
channel capacity is 32, bounded, pre-allocated. This is a sound
alternative to `ArcSwap`. The tradeoff: `try_recv` does a brief
spinlock-style CAS on the channel's internal mutex, which is
technically a contended lock (the driver thread's `send()` acquires
the same internal lock). In practice, the driver thread sends commands
infrequently (stream add/remove), so contention probability per
callback is negligible. `ArcSwap` would be strictly better for
real-time guarantees, but the practical difference is immaterial.

**A4. No tracing in callbacks — OK**

Neither `output_callback` nor `input_callback` contains any `tracing`
macro invocations. The `log_push_status` and `handle_read_status`
functions do contain `warn!`/`debug!`/`info!` calls, but these run
on the caller thread (in `push_samples`/`pop_samples`), not in the
cpal callback. The error callback uses `try_send` on a `SyncSender`,
which is also allocation-free.

**A5. Atomic ordering — OK (updated post-final-review)**

Peak state uses `Relaxed` — correct for single-writer (callback) /
single-reader (UI). The `fade_state` and `paused` atomics use
`Release` on stores (caller thread) and `Acquire` on loads (callback
thread) — correct for cross-thread visibility. The `aec_enabled` flag
in `AecState` uses `Relaxed` (line 273 of `aec.rs`), matching the
single-flag pattern. The `AecProcessor` internal `enabled` flag uses
`Acquire`/`Release` (updated from `SeqCst`).

**A6. AEC `Mutex` in callback path — IMPORTANT**

The `AecProcessor::process_capture_f32` and `process_render_f32`
methods (lines 117-158 of `aec.rs`) lock `Mutex<AudioProcessing>`.
Both are called exclusively from `AecState::process_stereo_interleaved`,
which runs only in the input callback. The output callback never calls
the processor — it writes to the render reference ring buffer instead.
This means the Mutex is effectively uncontended in the callback path.
The only other lock site is `set_stream_delay` (line 162), called
during device switches on the driver thread. If a device switch
coincides with an input callback, the callback blocks on the Mutex.
This window is sub-microsecond for `set_stream_delay` (just sets an
i32). Acceptable, but `set_stream_delay` should ideally not be called
while the input stream is running, or should use a lock-free mechanism.

### B. Channel mapping correctness

**B1. Mono-to-stereo in `push_samples` — OK**

The chunked approach (lines 373-383) processes up to 512 mono samples
per iteration, duplicating each into a stack-allocated `[f32; 1024]`
stereo buffer, then pushes the stereo slice. The chunk size of 512
fills exactly 1024 stereo samples — the buffer is fully utilized with
no overflow risk. The final chunk handles the remainder correctly via
`(samples.len() - offset).min(512)`.

**B2. Stereo-to-mono in `pop_samples` — OK (fixed post-final-review)**

Previously allocated a `Vec` on every call. Now uses a cached
`stereo_temp: Vec<f32>` field on `InputStream` that grows once on
first use and is reused on subsequent calls. Zero allocations in
steady state.

**B3. Device boundary mapping — OK**

Output callback (lines 870-886): stereo-to-mono averages L+R, stereo-to-
stereo copies directly, multi-channel fills remaining channels with
zero. Input callback (lines 937-951): mono-to-stereo duplicates,
multi-channel extracts first two channels. All four boundaries
(caller push, caller pop, device output, device input) handle mono
and stereo correctly.

### C. AEC correctness

**C1. Render reference ring buffer capacity — OK**

`RENDER_REF_CAPACITY` is `48000 / 10 * 2 = 9600` samples (100ms at
48 kHz stereo). The output callback writes at most `MAX_MIX_FRAMES * 2
= 8192` stereo samples per callback. The input callback drains the
ring buffer at the start of each invocation. Even if the output callback
runs twice before the input callback runs once, 8192 * 2 = 16384
samples would overflow the 9600-sample ring. This is a potential
issue under scheduling jitter: if output runs two full callbacks
ahead, `push_slice` silently drops samples (returns the count pushed,
which is less than requested). The dropped samples degrade AEC quality
but do not cause crashes or allocations. In practice, both callbacks
fire at the same ~5ms period, so the ring buffer holds ~2 callback
periods — adequate for normal jitter.

**C1a. Render reference overflow under heavy scheduling jitter — MINOR**

If the output callback runs significantly ahead (two full-size
callbacks without an intervening input drain), the render reference
ring overflows and silently drops the excess. Sonora's echo model
receives incomplete render data, reducing cancellation quality. The
impact is a brief AEC degradation, not a crash or audio artifact.
Increasing `RENDER_REF_CAPACITY` to 200ms (19200 samples) would
provide more headroom at negligible memory cost (75 KB). Not urgent.

**C2. All sonora calls serialized on input callback — OK**

`process_render_f32` and `process_capture_f32` are both called from
`AecState::process_stereo_interleaved`, which is called exclusively
from `input_callback`. The output callback never touches sonora. This
matches the plan's recommended architecture and eliminates the D3
concern from the pre-implementation review.

**C3. AEC disabled path — OK**

When `enabled` is false (line 290), the render buffers are still
drained in frame-sized chunks (lines 309-313), preventing unbounded
growth. Capture frames pass through unmodified (lines 344-349). The
`out_buf` clearing (lines 322-323) and re-filling with source data
ensures the output path works identically in both modes.

**C4. VecDeque growth prevention — OK**

All six VecDeques are pre-allocated to `BUF_CAPACITY = 8192`. The
`out_buf` pair is cleared every call. The render and capture
accumulation buffers hold at most one callback's worth of residual
(< 480 samples) after draining. Growth would require a single callback
delivering > 8192 frames, which no current platform does. The
`render_scratch` Vec is pre-allocated to `BUF_CAPACITY * 2 = 16384`,
matching the maximum interleaved read size.

### D. Mixing correctness

**D1. Additive mixing — OK**

The output callback (lines 797-838) reads each stream into
`stream_buf`, applies per-sample fade gain, and adds to `mix_buf`.
This is standard additive PCM mixing.

**D2. Clamping — OK**

Lines 842-855 clamp the mix buffer to `[-1.0, 1.0]` and compute peak
values in a single pass. Clamping happens after mixing and before
writing to the device buffer. Correct.

**D3. Stream add/remove between callbacks — OK**

Commands are drained via `try_recv` at the top of each callback
invocation (lines 762-782). The `entries` Vec is mutated in-place.
A new stream's first callback inclusion may be delayed by one
callback period (~5ms) if the command arrives mid-callback. A removed
stream's last contribution is the current callback where `Remove` is
processed (retain filters it out before the next mix). No stale
references, no dangling state.

**D4. Per-stream peak values — MINOR**

Lines 858-860 store the *mixed* peak into every active stream's peak
state. This means all output streams report the same peak level —
the peak of the combined mix, not of their individual contributions.
For a single-stream case this is correct. For multiple streams, the
per-stream peak meter is misleading (it reflects the mix, not the
individual stream). This is a UI-level concern, not a correctness
issue.

### E. Error handling

**E1. BufferUnderrun — OK**

Lines 1155-1169: counted with a sliding 5-second window. If >20
underruns in 5 seconds, logs a warning and calls `attempt_restart`.
Otherwise, the underrun is silently absorbed. cpal auto-recovers the
ALSA stream. Rate limiting is implemented as recommended.

**E2. DeviceNotAvailable and StreamInvalidated — OK**

Both trigger `attempt_restart` (lines 1171-1178). The restart uses
exponential backoff (500ms to 4s, lines 1190-1206). On success, the
backoff resets. On failure, it doubles. Clean implementation.

**E3. Ring buffers on device switch — OK**

`switch_devices_internal` (lines 1482-1504) drops old streams, then
calls `start_cpal_streams`, which creates fresh resampling channels
for all tracked streams (lines 1237-1248, 1307-1316). The old
producer/consumer pairs are replaced via the `Arc<Mutex>` swap. This
means all in-flight samples in the old ring buffers are discarded —
a brief silence gap. This is the correct behavior regardless of
whether sample rates match, because the resampling channels are
always recreated. The plan recommended preserving buffers when rates
match; the implementation takes the simpler (always-recreate) path,
which introduces a ~300ms gap on device switch (the latency pre-fill
period). Acceptable for an infrequent operation.

### F. Original review findings — status

**B1 (CRITICAL): fixed-resample asymmetric channels — RESOLVED**

All resampling channels are created at `INTERNAL_CHANNELS = 2`
(stereo). Mono-to-stereo conversion happens in `push_samples`
(line 371-383). Stereo-to-mono conversion happens in `pop_samples`
(line 437-452). Device boundary conversions happen in the callbacks
(output lines 870-886, input lines 937-951). Exactly as recommended.

**D3 (CRITICAL): AEC Mutex contention — RESOLVED**

All sonora calls are serialized on the input callback thread via the
render reference ring buffer pattern. The output callback writes mixed
audio into the SPSC ring; the input callback drains it and processes
both render and capture frames sequentially. No contention between
callbacks.

**A1 (IMPORTANT): VecDeque pre-allocation >= 8192 — RESOLVED**

`BUF_CAPACITY = 8192` is used for all six VecDeques in `AecState`
(lines 249-254). Matches the recommendation exactly.

**A2 (IMPORTANT): ArcSwap vs Mutex for consumer lists — PARTIALLY
RESOLVED**

The implementation uses `std::sync::mpsc::sync_channel` with
`try_recv` instead of either `ArcSwap` or `Arc<Mutex<Vec>>`. This
is a third approach not discussed in the original review. The callback
owns the `Vec<OutputEntry>` directly and mutates it in response to
commands. The driver thread never accesses the callback's Vec; it
sends commands through the bounded channel. This eliminates both the
`Mutex` contention concern and the `ArcSwap` overhead. The `try_recv`
call on `sync_channel` performs a brief CAS, which is technically
a lock, but the contention window is a few nanoseconds (only when
the driver sends a command simultaneously). Strictly speaking,
`ArcSwap::load()` is a single atomic load with no CAS, so it is
marginally better for real-time guarantees. In practice, the command
channel approach is equivalent and arguably simpler to reason about.

**C4 (IMPORTANT): Mix buffer >= 4096 frames — RESOLVED**

`MAX_MIX_FRAMES = 4096`. Both `mix_buf` and `stream_buf` are
allocated to `4096 * 2 = 8192` stereo samples (lines 1264-1265).
The callback processes in chunks of `MAX_MIX_FRAMES` when the cpal
buffer exceeds this size (lines 789-889). Matches the recommendation.

**E3 (IMPORTANT): Clear ring buffers on device switch — RESOLVED**

All resampling channels are recreated on device switch, discarding
all in-flight samples. See section E3 above.

**F1 (IMPORTANT): Declicker for pause/resume — RESOLVED**

Full declicker state machine implemented with four states
(`FADE_PLAYING`, `FADE_OUT`, `FADE_PAUSED`, `FADE_IN`) and a 3ms
linear fade (144 samples at 48 kHz). The `pause()`/`resume()` methods
set the fade state atomically; the output callback applies per-sample
gain ramps (lines 810-836). Fade progress advances per stereo frame
(every other sample index, via `i % 2 == 1`). On fade-out completion,
the state transitions to `FADE_PAUSED` and the stream is skipped
entirely. On fade-in completion, the state returns to `FADE_PLAYING`.

**F5.1 (IMPORTANT): Pre-fill in phase 1 — PARTIALLY RESOLVED**

The implementation does *not* explicitly pre-fill the ring buffer with
silence on stream creation. It relies on `fixed-resample`'s built-in
`latency_seconds` parameter (0.3s for output channels, 0.15s for
input), which inserts the initial latency padding internally. The
`ResamplingProd::push_interleaved` returns `OutputNotReady` until the
consumer has read at least once, and the consumer's first read returns
the latency-padded silence. Whether this constitutes "pre-fill" depends
on `fixed-resample`'s implementation. If it does not pre-fill the ring
buffer, the first callback will read silence via the underflow
autocorrect mechanism (25% threshold). This produces the same result
with a brief initial underrun log rather than clean silence. Functional,
but the initial underrun log noise could be avoided with explicit
pre-fill.

### G. New findings

**G1. OutputStream `Arc<Mutex<ResamplingProd>>` — MINOR**

The `OutputStream` wraps its producer in `Arc<Mutex<ResamplingProd>>`
(line 287). The Mutex is locked by the caller thread in `push_samples`
(uncontended — single writer) and by the driver thread on device switch
(to swap in a fresh producer, line 1239). These two operations never
overlap in practice because device switches happen in response to
driver messages, not during active pushing. The Arc is needed so the
`TrackedOutput` can hold a reference for device-switch rebuilds. This
is the right tradeoff: the Mutex exists for the rare device-switch
path, and contention in the normal path is zero.

**G2. InputStream Clone with shared SPSC consumer — IMPORTANT**

`InputStream` derives `Clone` (line 414) and wraps the consumer in
`Arc<Mutex<ResamplingCons>>`. If two clones call `pop_samples`
concurrently, they contend on the Mutex and each receives a disjoint
subset of samples — one gets even frames, the other gets odd frames.
This produces garbled audio on both consumers. The SPSC ring buffer
is fundamentally single-consumer; wrapping it in a Mutex makes
concurrent access "safe" in the Rust sense but semantically incorrect.

The `AudioSource` trait requires `cloned_boxed` (line 426), which
enables this pattern. If cloning is intended for ownership transfer
(move to another task), concurrent reading is unlikely. But the API
does not prevent it, and nothing documents that cloning an
`InputStream` and reading from both copies produces nonsensical audio.

Recommendation: either remove `Clone` from `InputStream` and have
`cloned_boxed` create a new resampling channel that receives the same
input data (true fan-out), or document prominently that clones share
state and must not read concurrently.

**G3. `OutputHandle` separation — OK**

The `OutputHandle` provides pause/resume/metering without access to
the sample producer. This is clean: the `OutputStream` owns the
producer for `push_samples`, while the handle can be cloned cheaply
for UI controls. The `AudioSinkHandle` trait delegates cleanly.

**G4. Device switch drops old streams before building new — MINOR**

`switch_devices_internal` (lines 1495-1498) sets the old cpal streams
to `None` before calling `start_cpal_streams`. This means there is a
brief window where no cpal stream is active — the speaker goes silent
and the microphone stops capturing. The plan recommended building new
streams *before* dropping old ones (atomic swap with crossfade). The
implementation takes the simpler stop-then-start approach. The silence
gap is bounded by `start_cpal_streams`'s execution time (typically
10-50ms for device open + stream build). Audible but brief. A future
improvement could overlap old and new streams.

**G5. `fade_progress` not reset on FADE_OUT start — MINOR**

When `pause()` is called, it stores `FADE_OUT` but does not reset
`fade_progress` to 0. If the stream was previously fading in and
`pause()` is called mid-fade, `fade_progress` retains its previous
value. The fade-out gain starts from `1.0 - (progress / 144)`, which
may not be 1.0 if progress > 0. This produces a discontinuity: the
audio level jumps from the current fade-in gain to the fade-out gain
at the stored progress point. The inverse also applies: `resume()`
does not reset `fade_progress`, so a fade-in after a partial fade-out
starts from the wrong point.

Recommendation: reset `fade_progress = 0` when transitioning to
`FADE_OUT` or `FADE_IN`. This is a one-line fix in both `pause()` and
`resume()`, but it requires the `fade_progress` to be atomic or
communicated through the command channel since it is owned by the
callback. The simplest fix is to have the callback itself reset
`fade_progress` when it detects a state transition (compare current
fade state to previous).

**G6. `VecDeque::drain` allocates internally — OK (verified: it does
not)**

`VecDeque::drain(..n)` in `aec.rs` line 310-312 does not allocate.
It returns an iterator that adjusts the VecDeque's head pointer. The
dropped elements are simply overwritten on subsequent pushes. No
concern.

**G7. Peak metering stores mixed peak for all streams — MINOR**

As noted in D4, all output streams share the same peak value (the
mix). Individual stream peak tracking would require computing peak
per-stream before mixing. This is a feature gap, not a bug — the
current firewheel implementation had the same limitation due to
graph-level peak metering.

**G8. No `set_stream_delay` call — MINOR**

The plan's phase 2 work item 4 specifies computing
`input_to_output_latency` and calling `processor.set_stream_delay()`
when streams start or devices switch. The implementation declares
`set_stream_delay` on `AecProcessor` (line 162 of `aec.rs`) but never
calls it. Sonora defaults to 0ms stream delay. The actual delay
depends on cpal buffer sizes and the render reference ring buffer
transit time (~5-10ms). Without calibration, sonora's echo model may
under-cancel because it searches for echo correlation at the wrong
time offset. The impact depends on the room and speaker/mic geometry.
For casual voice calls this is likely inaudible; for high-quality AEC
in echo-prone environments it could matter.

### Summary of post-implementation findings

| # | Finding | Severity |
|---|---------|----------|
| A1 | Pre-allocated buffers, no callback allocations | OK |
| A2 | AEC VecDeque growth possible at >8192 frame callbacks | OK (no platform delivers this) |
| A3 | `sync_channel` `try_recv` acceptable real-time tradeoff | OK |
| A4 | No tracing in callbacks | OK |
| A5 | Atomic orderings correct (Release/Acquire for cross-thread) | OK |
| A6 | AEC Mutex uncontended (only input callback + rare device switch) | OK |
| B1 | Channel mapping correct at all four boundaries | OK |
| B2 | `pop_samples` mono path uses cached `stereo_temp` buffer | OK |
| C1 | Render reference ring adequate for normal jitter | OK |
| C1a | Render reference may overflow under heavy scheduling jitter | MINOR |
| C2 | All sonora calls serialized on input callback | OK |
| C3 | AEC disabled path drains buffers correctly | OK |
| C4 | VecDeque pre-allocation matches recommendation | OK |
| D1-D3 | Mixing correct, clamped, mid-callback changes handled | OK |
| D4 | Peak metering reports mix level for all streams | MINOR |
| E1-E2 | Error handling with rate limiting and backoff | OK |
| E3 | Ring buffers recreated on device switch | OK |
| F-all | All original review findings addressed | OK |
| G1 | `Arc<Mutex<ResamplingProd>>` tradeoff acceptable | OK |
| G2 | `InputStream` Clone shares SPSC consumer — concurrent reads garble | IMPORTANT |
| G3 | `OutputHandle` separation clean | OK |
| G4 | Device switch has brief silence gap (stop-then-start) | MINOR |
| G5 | `fade_progress` not reset on state transition — discontinuity | MINOR |
| G7 | Peak metering is mix-level, not per-stream | MINOR |
| G8 | `set_stream_delay` never called — AEC may under-cancel | MINOR |

### Verdict: APPROVED

Both CRITICAL findings from the pre-implementation review (B1: asymmetric
channels, D3: AEC Mutex contention) are fully resolved. The architecture
is sound: callbacks are allocation-free on the data path, AEC processing
is correctly serialized, channel mapping is explicit at every boundary,
and error handling degrades gracefully.

One IMPORTANT finding remains: G2 (`InputStream` Clone semantics). This
is a design-level concern rather than a runtime bug — concurrent clone
reads produce garbled audio, but the current codebase likely does not
exercise this pattern. It should be addressed before the API stabilizes,
either by removing Clone or by implementing true fan-out.

The MINOR findings (B2, C1a, D4, G4, G5, G7, G8) are quality-of-life
improvements that can be addressed incrementally. None affects
correctness under normal operation. G5 (fade progress discontinuity) is
the most user-facing of these — a quick fix when convenient.

---

## Final expert review (2026-03-19)

Independent review of the completed implementation against real-time audio
best practices. Three findings were addressed immediately:

### Addressed findings

**1. Atomic ordering (IMPORTANT — FIXED)**

`fade_state` and `paused` atomics were using `Relaxed` ordering for
cross-thread communication (caller thread stores, callback thread loads).
Changed to `Release` on stores (pause/resume/toggle) and `Acquire` on
loads (callback). The callback-internal stores (`FADE_PAUSED`,
`FADE_PLAYING` at fade completion) also use `Release` for consistency.
`toggle_pause` uses `AcqRel` on the `fetch_xor`.

**2. AEC `SeqCst` → `Acquire`/`Release` (OPTIONAL — FIXED)**

`AecProcessor::is_enabled()` changed from `SeqCst` to `Acquire`.
`AecProcessor::set_enabled()` changed from `swap(SeqCst)` to
`store(Release)`. Eliminates unnecessary cache coherency overhead.

**3. InputStream per-call allocation (MINOR — FIXED)**

`pop_samples()` mono path was allocating `vec![0.0; stereo_len]` on every
call. Added `stereo_temp: Vec<f32>` field to `InputStream` that grows
once on first use and is reused on subsequent calls. Eliminates ~50
allocations/second for mono input streams.

### Remaining recommendations (not addressed — deferred)

- Stress test for rapid pause/resume during callback execution
- `fade_progress` reset on state transition (G5 from post-impl review)
- `set_stream_delay` wiring to driver (G8 from post-impl review)

---

## Post-refactor review (2026-03-19)

Expert review comparing the audio backend against WebRTC ADM,
GStreamer, OBS, and Firewheel. Minor issues were fixed inline; medium
and major findings are documented below with suggested solutions.

### Fixed inline

These were addressed directly in code:

1. **Fade L/R imbalance** — the fade gain loop iterated per-sample,
   giving L and R slightly different gains within a frame.
   Restructured to compute gain once per frame and apply to both
   channels identically.

2. **Input callback panic on oversized data** — `stereo_buf` slice
   panicked if a device delivered >4096 frames. Added chunked
   processing matching the output callback pattern.

3. **InputStream::stereo_temp not pre-allocated** — allocated on first
   `pop_samples` call. Now pre-allocated to 2048 samples.

4. **AEC hardcoded 48000 literal** — replaced with
   `super::super::INTERNAL_RATE` for maintainability.

5. **AEC processor ordering inconsistency** — `is_enabled` /
   `set_enabled` used `Acquire` / `Release` while all other sites used
   `Relaxed`. Unified to `Relaxed` — pure boolean branch, no dependent
   state.

### MAJOR: AEC processor Mutex on real-time thread

**Location**: `aec.rs` processor module, lines 130-134, 154-158.

`AecProcessor` wraps sonora behind `Arc<Mutex<AudioProcessing>>`. The
input callback acquires this Mutex via `process_capture_f32` and
`process_render_f32`. Currently safe because all calls happen on the
single input callback thread, and `set_stream_delay` / `set_enabled`
are unused or use atomics. But if `set_stream_delay` is wired up
later, the callback will contend with the driver thread.

WebRTC ADM never locks a mutex on the audio thread. State changes go
through lock-free SPSC queues or atomics.

**Solution**: Make `AecState` own the `AudioProcessing` directly
(not through `AecProcessor`), removing shared access from the callback
path. If sharing is needed later, use `try_lock` with passthrough
fallback, or communicate commands via an atomic/SPSC channel.

### MEDIUM: VecDeque can allocate on audio thread

**Location**: `aec.rs` state module, lines 284-285, 317-318, 343-349.

`VecDeque::push_back()` reallocates if capacity is exceeded. Initial
capacity is 8192, but `out_buf` accumulates across callbacks (now that
the clear bug is fixed) and has no cap.

**Solution**: Either (a) cap `out_buf` length and drain excess when it
grows beyond a threshold (e.g., 2× frame_size), or (b) replace
VecDeque with fixed-capacity ring buffers that discard on overflow.
Option (a) is simpler and sufficient — the steady-state `out_buf`
length is bounded by the frame-size mismatch residual.

### MEDIUM: AEC processing errors silently discarded

**Location**: `aec.rs` state module, lines 300, 336.

Both `process_render_f32` and `process_capture_f32` return
`Result<(), sonora::Error>`, discarded with `let _ = ...`. Can't log
on the audio thread (allocation risk).

**Solution**: Add an `AtomicU32` error counter to `AecState`. The
driver thread periodically checks and logs it. Alternatively, an
`AtomicBool` "aec_errored" flag logged once on transition.

### MEDIUM: Output resampling latency is 300ms

**Location**: `audio_backend.rs`, `create_output_channel`.

`latency_seconds: 0.3` adds 300ms to the playout path. Combined with
network jitter buffers, total mouth-to-ear latency can exceed 500ms.
WebRTC targets 10-20ms audio playout latency.

**Solution**: Reduce to 0.05-0.1s (50-100ms). fixed-resample handles
underflow gracefully (pads silence), so occasional underflow at low
latency is preferable to consistent high latency. Make configurable
via `AudioBackendOpts`. Input latency (currently 150ms) should also
drop to 30-50ms.

### MEDIUM: No clock drift correction between input and output

**Location**: Architectural — render reference ring buffer.

Input and output cpal streams run on independent hardware clocks. The
AEC render reference ring buffer (100ms capacity) will overflow or
underflow over time, degrading echo cancellation.

**Solution**: Monitor render reference ring buffer fill level. Log
drift rate at debug level. For production quality, implement
micro-resampling on the render reference stream, or periodically
adjust `set_stream_delay_ms` based on observed drift.

### MEDIUM: Device switch doesn't handle rebuild failure

**Location**: `audio_backend.rs`, `switch_devices_internal`.

Drops old streams before starting new ones. If `start_cpal_streams`
fails, the audio subsystem is left with no active streams and no way
to recover.

**Solution**: Either (a) start new streams before dropping old ones
and swap atomically, or (b) on failure, attempt restart with previous
device IDs, or (c) trigger `attempt_restart` from the error path.
Option (c) is simplest and consistent with the existing error recovery
pattern.

### MINOR: negotiate_stream_config doesn't prefer stereo

When multiple config ranges match the same preferred rate, the first
is accepted regardless of channel count. A 6-channel config could be
selected over a 2-channel one.

**Solution**: Among matching configs, prefer channel count closest to
2 for output, 1 for input.

### MINOR: Render reference ring overflow is silent

`render_ref_prod.push_slice()` return value is discarded. If the ring
is full, samples are silently lost and AEC quality degrades.

**Solution**: Track overflow count with an atomic counter, log
periodically from the driver thread.

### MINOR: No explicit buffer size hint to cpal

`StreamConfig.buffer_size` defaults to `BufferSize::Default`. On some
ALSA backends this can be large (2048+ frames). PipeWire is usually
well-tuned.

**Solution**: Consider requesting 480 frames (10ms at 48kHz) when low
latency matters. Configurable via `AudioBackendOpts`.
