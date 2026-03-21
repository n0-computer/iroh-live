# Capture Architecture

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | rusty-capture, moq-media |
| Platforms | Linux (PipeWire, V4L2, X11), macOS (ScreenCaptureKit, AVFoundation) |

rusty-capture provides platform-specific screen and camera capture behind
the `VideoSource` trait from rusty-codecs. Each backend runs on a
dedicated OS thread and produces `VideoFrame` values that encoder
pipelines consume.

## VideoSource trait

The trait is deliberately synchronous and polling-based:

```rust
pub trait VideoSource: Send + 'static {
    fn name(&self) -> &str;
    fn format(&self) -> VideoFormat;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}
```

`pop_frame()` returns the latest available frame, or `None` if no new
frame has arrived since the last call. The drain-to-latest semantics are
important: if multiple frames have accumulated (because the consumer was
slow), only the most recent is returned. This prevents latency buildup in
the capture-to-encode path.

## Platform backends

### PipeWire (Linux)

The primary Linux backend for both screen and camera capture. Session
negotiation goes through xdg-desktop-portal (the user sees a system
picker dialog for screen selection). Once granted, pipewire-rs opens a
stream and receives frames in a process callback.

Frames arrive as DMA-BUF file descriptors (typically NV12 from the
compositor or camera ISP). The callback mmaps the buffer, wraps it as a
`VideoFrame`, and parks it for `pop_frame()` to collect. DMA-BUF
zero-copy is supported through to the encoder when VAAPI is in use.

Feature flags: `pipewire` enables both screen and camera. The crate
requires `libpipewire-0.3-dev` at build time.

### V4L2 (Linux)

Camera capture via Video4Linux2 MMAP streaming. Targets USB webcams and
CSI-attached cameras (Raspberry Pi camera module, embedded systems). The
backend opens the device node, negotiates format and resolution, requests
MMAP buffers, and runs a dequeue/requeue loop on its thread.

V4L2 also supports DMA-BUF export via `VIDIOC_EXPBUF` for zero-copy
handoff to hardware encoders that accept DMA-BUF input.

Feature flag: `v4l2`.

### X11 (Linux)

Screen capture via MIT-SHM (shared memory). CPU-only; no GPU zero-copy.
Provided as a fallback for systems without PipeWire (e.g., X11-only
desktops or containers). Lower overhead than PipeWire for simple
screen-grab scenarios but no DMA-BUF support.

Feature flag: `x11`. Not included in defaults.

### ScreenCaptureKit (macOS)

Screen capture on macOS 12.3+ using Apple's ScreenCaptureKit framework.
Produces IOSurface-backed frames; zero-copy GPU import is planned but
not yet implemented.

Feature flag: `screen-apple`.

### AVFoundation (macOS, iOS)

Camera capture via AVFoundation's `AVCaptureSession`. Produces
CVPixelBuffer-backed frames.

Feature flag: `camera-apple`.

### Cross-platform fallbacks

`xcap` (screen) and `nokhwa` (camera) provide CPU-only capture on
platforms where native backends are unavailable. These are optional
feature flags, not enabled by default. The default `screen` and
`camera` features activate the native backends (PipeWire/X11/V4L2 on
Linux, ScreenCaptureKit/AVFoundation on macOS).

## Runtime backend selection

On Linux, the capture constructors prefer PipeWire when the PipeWire
daemon is available, falling back to X11 for screen and V4L2 for camera.
The selection happens at runtime based on D-Bus service availability, not
at compile time.

## Thread model

Each capture backend runs on a dedicated OS thread, not a tokio task.
Capture APIs often block (PipeWire's main loop, V4L2's `DQBUF` ioctl,
X11's `XShmGetImage`), and running them on the async runtime would stall
other tasks.

The shared source wrapper in moq-media (`SharedVideoSource`) runs
`pop_frame()` in a loop on its own OS thread and publishes the latest
frame via a `watch` channel. Encoder threads each hold a `watch::Receiver`
and see the latest frame without blocking the capture thread.

## Open areas

**PipeWire explicit sync.** PipeWire supports `SPA_META_SyncTimeline`
for GPU-explicit fence synchronization, but rusty-capture does not
implement it. On Intel hardware with implicit sync, this is not a problem
because buffer readiness is guaranteed by the time the mmap completes.
On NVIDIA or future kernels that drop implicit sync, the lack of explicit
fence handling may cause tearing or stale reads.

**Drain-all-keep-latest dequeue pattern.** Research into GNOME Remote
Desktop's PipeWire usage revealed a pattern where all pending buffers
are dequeued and only the latest is kept, releasing the rest back to the
pool immediately. rusty-capture currently processes one buffer per
callback invocation. Adopting the drain-all pattern would reduce pool
pressure under high frame rates and improve latency consistency, but has
not been implemented.
