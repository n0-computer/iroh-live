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
