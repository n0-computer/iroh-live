# Phase 3e: Runtime Audio Device Switching & Fallback

## Status: Pending

## Goal

Make `AudioBackend` resilient to device changes: switch input/output devices at runtime without consumers (encoders, decoders, `AudioTrack`s) noticing, and automatically fall back to the system default when a device disconnects unexpectedly.

## Problem

Today, `AudioBackend::new(input, output)` starts a Firewheel audio graph bound to fixed devices. If headphones are unplugged or the user wants to switch microphones:

1. **No switch API** — the only option is to create a new `AudioBackend`, which invalidates all existing `InputStream`/`OutputStream` handles.
2. **Unexpected disconnect is unhandled** — the commented-out `StreamStoppedUnexpectedly` block in `AudioDriver::run()` logs an error but all streams silently go inactive. `OutputStream::push_samples` discards audio; `InputStream::pop_samples` returns `None` forever.
3. **RoomPublisherSync workaround** — `set_audio_backend()` + `set_audio(false)` + `set_audio(true)` tears down and rebuilds the entire encode pipeline just to switch a mic.

The fix: a new `SwitchDevice` driver message that stops the CPAL stream, starts a new one on the requested device, re-activates all stream nodes, and swaps the inner handles — making the switch invisible to consumers holding `InputStream`/`OutputStream`.

## Firewheel API Constraints

Key findings from reviewing Firewheel 0.10 internals:

1. **`cx.stop_stream()` + `cx.start_stream(new_config)` works** — graph topology (nodes, edges) is fully preserved across cycles. `stop_stream()` only drops the backend handle and calls `graph.deactivate()` (sets `needs_compile = true`).

2. **NodeIDs remain valid** after stop/start. Node storage lives in an `Arena<NodeEntry>` unaffected by stream lifecycle.

3. **Handles are clones, not references** — `StreamWriterState::handle()` returns `Mutex<Self>` via `Clone`. The consumer's `Arc<Mutex<StreamWriterState>>` is a *separate copy* from the graph-internal state. After `stop_stream()`, the old handle becomes permanently inactive (`is_ready() = false`). To re-activate, you must call `cx.node_state_mut::<StreamWriterState>(id).start_stream(...)` on the *graph-internal* state, then get a **fresh** handle via `cx.node_state::<StreamWriterState>(id).handle()`.

4. **`can_start_stream()`** must be polled after `stop_stream()` before calling `start_stream()` — waits for the processor to be returned from the audio thread.

5. **`UpdateError::StreamStoppedUnexpectedly(Option<cpal::StreamError>)`** — `Some(StreamError::DeviceNotAvailable)` = device unplugged. CPAL has no device-change event; detection happens via error callback during streaming, surfaced through `cx.update()`.

**Implication**: "transparent to consumers" requires an indirection layer that swaps the inner handle atomically.

## Design

### AudioDriverOpts

Replace the two `Option<DeviceId>` constructor args with a structured opts type:

```rust
pub struct AudioDriverOpts {
    /// Initial input device. `None` = system default.
    pub input_device: Option<DeviceId>,
    /// Initial output device. `None` = system default.
    pub output_device: Option<DeviceId>,
    /// When a device disconnects unexpectedly, automatically switch to
    /// the system default device. Default: `true`.
    pub fallback_to_default: bool,
}

impl Default for AudioDriverOpts {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            fallback_to_default: true,
        }
    }
}
```

`AudioBackend::new` changes signature:

```rust
// Before
pub fn new(input_device: Option<DeviceId>, output_device: Option<DeviceId>) -> Self
// After
pub fn new(opts: AudioDriverOpts) -> Self
```

Keep `AudioBackend::default()` working (uses `AudioDriverOpts::default()`). The existing `AudioBackend::new(None, None)` call sites update to `AudioBackend::new(AudioDriverOpts::default())` or `AudioBackend::default()`.

### Swappable Handle Indirection

Since Firewheel `handle()` returns a *clone* of the node state (not a reference into the graph), old handles go permanently inactive after `stop_stream()`. To make switching transparent, wrap handles in a swappable layer:

```rust
/// Wrapper that lets the driver swap the inner handle on device switch.
/// Consumers hold `Arc<SwappableWriter>` — the outer Arc never changes.
struct SwappableWriter {
    inner: Mutex<Mutex<StreamWriterState>>,
}

impl SwappableWriter {
    fn new(handle: Mutex<StreamWriterState>) -> Self {
        Self { inner: Mutex::new(handle) }
    }

    /// Replace the inner handle. Called by the driver after re-activating the node.
    fn swap(&self, new_handle: Mutex<StreamWriterState>) {
        *self.inner.lock().unwrap() = new_handle;
    }

    /// Lock the inner handle for push/read operations.
    fn lock(&self) -> impl DerefMut<Target = StreamWriterState> + '_ {
        // Lock outer, then lock inner — the outer lock is only briefly held
        // during swap, so contention is minimal.
        MutexGuard::map(self.inner.lock().unwrap(), |inner| inner.get_mut().unwrap())
    }
}
```

Same pattern for `SwappableReader`. `OutputStream` and `InputStream` change from holding `Arc<Mutex<StreamWriterState>>` to `Arc<SwappableWriter>` / `Arc<SwappableReader>`.

**Alternative (simpler)**: Use `ArcSwap<Mutex<StreamWriterState>>` from the `arc-swap` crate — zero-cost reads, atomic pointer swap. Worth evaluating if the double-Mutex ergonomics are awkward.

The driver tracks each stream's `Arc<Swappable*>` alongside its `NodeID`, so after re-activation it can call `swappable.swap(fresh_handle)`.

### New DriverMessage: SwitchDevice

```rust
enum DriverMessage {
    OutputStream { ... },
    InputStream { ... },
    SwitchDevice {
        input_device: Option<Option<DeviceId>>,   // None = don't change, Some(None) = system default, Some(Some(id)) = specific
        output_device: Option<Option<DeviceId>>,
        reply: oneshot::Sender<Result<()>>,
    },
}
```

Public API on `AudioBackend`:

```rust
impl AudioBackend {
    /// Switch the input device. `None` = system default.
    pub async fn switch_input(&self, device: Option<DeviceId>) -> Result<()>

    /// Switch the output device. `None` = system default.
    pub async fn switch_output(&self, device: Option<DeviceId>) -> Result<()>

    /// Switch both devices at once.
    pub async fn switch_devices(
        &self,
        input: Option<DeviceId>,
        output: Option<DeviceId>,
    ) -> Result<()>
}
```

### AudioDriver: Device Switch Procedure

When `SwitchDevice` is received, the driver performs a graph-level stream restart:

1. **Stop the CPAL stream** — `cx.stop_stream()`. This drops the CPAL backend and calls `graph.deactivate()`. All graph-internal node states become inactive. Consumer handles (clones) also become inactive.

2. **Wait for processor return** — Poll `cx.can_start_stream()` in a tight loop (with short sleeps). The processor must be returned from the audio thread before a new stream can start.

3. **Rebuild CPAL config** with new device IDs (applying Linux pipewire fallback if needed).

4. **Start new CPAL stream** — `cx.start_stream(new_config)`. Firewheel recompiles the graph and connects to new hardware. The graph topology (nodes, connections) is preserved.

5. **Re-activate existing streams and swap handles** — For each tracked stream writer/reader node:
   - Call `cx.node_state_mut::<StreamWriterState>(id).start_stream(target_sample_rate, new_device_sample_rate, resampling_config)` to start fresh resampling.
   - Queue the returned activation event via `cx.queue_event_for(node_id, event)`.
   - Get a fresh handle: `cx.node_state::<StreamWriterState>(id).handle()`.
   - Swap into the consumer's `SwappableWriter`: `tracked.swappable.swap(fresh_handle)`.

6. **Update AEC** — Reconfigure `aec_processor.set_stream_delay()` for the new device's latency (from `cx.stream_info().input_to_output_latency_seconds`).

7. **Reply** with `Ok(())` or the error.

### Stream Tracking

The driver must track all active stream nodes to re-activate them after a switch. Add to `AudioDriver`:

```rust
struct TrackedInputStream {
    node_id: NodeID,
    format: AudioFormat,
    swappable: Arc<SwappableReader>,
}
struct TrackedOutputStream {
    node_id: NodeID,
    peak_meter_id: NodeID,
    format: AudioFormat,
    swappable: Arc<SwappableWriter>,
}

struct AudioDriver {
    // ... existing fields ...
    input_streams: Vec<TrackedInputStream>,
    output_streams: Vec<TrackedOutputStream>,
    current_input_device: Option<DeviceId>,
    current_output_device: Option<DeviceId>,
    opts: AudioDriverOpts,
}
```

`input_stream()` and `output_stream()` push into these vecs. The `Arc<Swappable*>` is shared between the tracked entry and the consumer's `InputStream`/`OutputStream`. Streams are never removed (Firewheel nodes are not removed from the graph in the current API), but inactive/dropped ones are harmless — re-activation on a stopped handle is a no-op.

### Handling StreamStoppedUnexpectedly

Replace the commented-out block in `AudioDriver::run()`:

```rust
if let Err(e) = self.cx.update() {
    error!("audio backend error: {:?}", &e);
    match &e {
        UpdateError::StreamStoppedUnexpectedly(_) if self.opts.fallback_to_default => {
            warn!("audio device disconnected, falling back to system default");
            if let Err(e) = self.switch_device_internal(None, None) {
                error!("fallback to default device failed: {e:#}");
            }
        }
        _ => {}
    }
}
```

The `switch_device_internal` method is the same logic as handling `SwitchDevice` but called synchronously from within the driver loop (no message passing needed).

Note: After `StreamStoppedUnexpectedly`, `cx.can_start_stream()` may not return `true` immediately — the driver must poll/wait before calling `start_stream()`. A short busy-wait with `thread::sleep(1ms)` is acceptable since this is an OS thread and the wait is typically < 10ms.

### Simplifying RoomPublisherSync

With device switching on the backend, `RoomPublisherSync` no longer needs to tear down and rebuild audio pipelines when the device changes:

```rust
// Before (current code):
pub fn set_audio_backend(&mut self, backend: AudioBackend) { ... }
// set_audio(false); set_audio_backend(new); set_audio(true);

// After:
pub async fn switch_audio_device(&self, device: Option<DeviceId>) -> Result<()> {
    self.audio_ctx.switch_input(device).await
}
```

The `set_audio_backend` method can be deprecated or removed.

## Implementation Steps

Each step leaves the workspace compiling.

### Step 1: AudioDriverOpts

- Add `AudioDriverOpts` struct to `moq-media/src/audio.rs`
- Change `AudioBackend::new` to take `AudioDriverOpts`
- Change `AudioDriver::new` to take `AudioDriverOpts`, store in `self.opts`
- Update all call sites (`AudioBackend::new(None, None)` → `AudioBackend::default()`)
- Store `current_input_device` / `current_output_device` on the driver

### Step 2: Swappable Handles + Stream Tracking

- Add `SwappableWriter` / `SwappableReader` wrapper types (double-Mutex or `ArcSwap`)
- Change `OutputStream::handle` from `Arc<Mutex<StreamWriterState>>` to `Arc<SwappableWriter>`
- Change `InputStream::handle` from `Arc<Mutex<StreamReaderState>>` to `Arc<SwappableReader>`
- Update all `handle.lock()` call sites in `OutputStream`/`InputStream` methods
- Add `TrackedInputStream` / `TrackedOutputStream` structs (with `Arc<Swappable*>`)
- Add tracking vecs to `AudioDriver`, push from `input_stream()` / `output_stream()`

### Step 3: SwitchDevice Message

- Add `SwitchDevice` variant to `DriverMessage`
- Add `switch_input`, `switch_output`, `switch_devices` to `AudioBackend`
- Implement `switch_device_internal` on `AudioDriver`:
  - `cx.stop_stream()`
  - Poll `cx.can_start_stream()` in tight loop with `thread::sleep(1ms)`
  - Rebuild `CpalConfig` with new devices (apply Linux pipewire fallback)
  - `cx.start_stream(new_config)`
  - For each tracked stream: `cx.node_state_mut::<T>(id).start_stream(...)` + `queue_event_for` + get fresh `handle()` + `swappable.swap(fresh)`
  - Update AEC delay from `cx.stream_info().input_to_output_latency_seconds`
- Wire `SwitchDevice` message to `switch_device_internal` in `handle_message`

### Step 4: Unexpected Disconnect Fallback

- Replace the commented-out `StreamStoppedUnexpectedly` block in `run()`
- Check `self.opts.fallback_to_default`
- Call `self.switch_device_internal(None, None)` for fallback
- Log clearly so the application layer can observe the switch

### Step 5: Simplify RoomPublisherSync

- Add `switch_audio_device` method to `RoomPublisherSync`
- Deprecate `set_audio_backend`
- Update `set_audio` to no longer require full teardown/rebuild on device change

## Files

| File | Change |
|---|---|
| `moq-media/src/audio.rs` | `AudioDriverOpts`, stream tracking, `SwitchDevice` message, fallback logic |
| `moq-media/src/lib.rs` | Re-export `AudioDriverOpts` |
| `iroh-live/src/rooms/publisher.rs` | `switch_audio_device`, simplify `set_audio` |
| `iroh-live/examples/rooms.rs` | `AudioBackend::default()` |
| `iroh-live/examples/dev.rs` | `AudioBackend::default()` |
| `iroh-live/examples/watch.rs` | `AudioBackend::default()` |
| `iroh-live/examples/publish.rs` | `AudioBackend::default()` |
| `iroh-live/examples/common_egui/room_view.rs` | No change (receives `AudioBackend` from caller) |

## Testing

### Unit tests (moq-media)

- **Switch input device**: Create `AudioBackend`, get `InputStream`, switch input, verify `pop_samples` resumes returning data
- **Switch output device**: Create `AudioBackend`, get `OutputStream`, switch output, verify `push_samples` succeeds
- **Fallback disabled**: Set `fallback_to_default: false`, simulate disconnect, verify streams stay inactive
- **Multiple streams survive switch**: Create 3 output streams, switch device, verify all 3 resume

### Integration tests

- **Hot-swap headphones**: Start with built-in speakers, switch to headphones via `switch_output`, verify audio routes to new device
- **Mic switch mid-call**: Publishing audio, switch mic via `switch_input`, verify remote subscriber hears new mic without interruption

### Manual verification

1. `cargo run --example rooms` — in a call, change audio device via UI → audio continues seamlessly
2. Unplug headphones → logs show fallback to default, audio resumes on speakers
3. Plug headphones back → switch to headphones via API, audio moves to headphones
