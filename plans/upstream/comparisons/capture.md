# Capture Comparison: rusty-capture vs moq capture

> Campaign: upstream | Kind: comparison | Read ../0-overview.md first; index at 0-index.md.

Compares iroh-live's `rusty-capture` (plus the capture-adjacent pieces of
`moq-media`) against moq's capture layer at main HEAD `3a3e0ea8`. moq is a
single branch since the dev line merged into main on 2026-07-21, so the full
native capture stack (PipeWire, V4L2, ScreenCaptureKit, AVFoundation, Media
Foundation, Desktop Duplication, and macOS system audio) is moq main. moq keeps
video capture inside `rs/moq-video/src/capture/` and audio capture inside
`rs/moq-audio/src/capture*`; there is no standalone capture crate. iroh-live
citations are `file:line` against the working tree; moq citations are against
`git show 3a3e0ea8:<path>`. The capture source files are byte-identical to the
pre-merge analysis SHA `261c2048`, so the line citations are exact against main.
Trait and API-shape questions are analyzed in
[traits-api.md](traits-api.md); concrete moq-side change
proposals live in [moq-changes.md](moq-changes.md).

---

## 1. Abstractions side by side

### Ours: a public trait, a backend enum, and facade capturers

The capture contract is not defined in `rusty-capture` at all. Every backend
implements `rusty_codecs::traits::VideoSource`, which `rusty-capture` re-exports
(`rusty-capture/src/lib.rs:77-80`). Verbatim
(`rusty-codecs/src/traits.rs:229-241`):

```rust
/// Provides raw video frames from a capture device or synthetic source.
pub trait VideoSource: Send + 'static {
    /// Returns the source's display name.
    fn name(&self) -> &str;
    /// Returns the video format produced by this source.
    fn format(&self) -> VideoFormat;
    /// Pops the next captured frame, or `None` if no frame is ready.
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    /// Starts frame capture.
    fn start(&mut self) -> Result<()>;
    /// Stops frame capture.
    fn stop(&mut self) -> Result<()>;
}
```

A second trait covers sources that encode on the device
(`rusty-codecs/src/traits.rs:268-287`): `PreEncodedVideoSource` with
`config() -> VideoConfig`, `pop_packet() -> Result<Option<EncodedFrame>>`, and
the same `start`/`stop`. Only `LibcameraH264Source` implements it.

On top of the trait sit two `Box<dyn VideoSource>` facades, `CameraCapturer`
(`rusty-capture/src/lib.rs:363-529`) and `ScreenCapturer` (`lib.rs:536-685`),
which auto-select a backend through a priority cascade: cameras try libcamera,
then V4L2, then PipeWire if the daemon is running, then nokhwa, then
AVFoundation (`lib.rs:182-214`); screens try PipeWire, then X11, then
ScreenCaptureKit, then xcap (`lib.rs:132-152`). Dispatch runs over the public
`CaptureBackend` enum of eight backends (`rusty-capture/src/types.rs:11-28`),
and `list_all_monitors`/`list_all_cameras` union enumeration across every
compiled-in backend (`lib.rs:156-241`). PipeWire liveness is probed at runtime
via the `$XDG_RUNTIME_DIR/pipewire-0` socket or `pidof` (`lib.rs:86-99`).

Delivery is pull. The consumer (moq-media's encode pipeline, on an OS thread it
owns) calls `pop_frame()` in a loop. Blocking-read backends (V4L2, X11,
libcamera, nokhwa, xcap) block on the device syscall inside `pop_frame`;
callback-driven backends (PipeWire, ScreenCaptureKit) push into a bounded
`mpsc::sync_channel(2)` from the OS callback thread and `pop_frame` receives
from it (`rusty-capture/src/platform/linux/pipewire.rs:1381`,
`rusty-capture/src/platform/apple/screen.rs:331`).

Demand-driven open and close exists on our side, but two layers up. The trait
itself is manual `start`/`stop`; `moq-media/src/publish.rs` implements the
gating: `run_dynamic` watches `dynamic.requested_track()` and spawns a task per
track that awaits `track.unused()` and stops it
(`moq-media/src/publish.rs:333-360`), and `SharedVideoSource`'s capture thread
parks while no subscriber is present, calling `source.stop()` on park and
`source.start()` on resume with the PTS anchor reset
(`moq-media/src/publish.rs:1119-1146`). The seam is leaky: a comment at
`publish.rs:1109-1113` records that PipeWire capturers cannot survive a `stop()`
because it permanently kills the capture thread, so the gating has to track
`ever_started` and avoid stopping before the first start.

### Theirs: no trait, a crate-private stream, and demand wired into publish

moq has no capture trait and no backend enum. The public surface is a `Source`
selector, a `Config`, and enumeration types; the capture machinery is
`pub(crate)` and reachable only through `encode::publish_capture`. Verbatim
(`rs/moq-video/src/capture/mod.rs:65-93` region):

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// A camera / webcam. `None` opens the default camera.
	Camera(Option<String>),
	/// A whole display. `None` opens the main display.
	Display(Option<String>),
	/// A single window, by the id [`windows`] reports. macOS only.
	Window(String),
	/// Every window belonging to one application, by the id [`apps`] reports
	/// (a bundle identifier). Windows that open later are included. macOS only.
	App(String),
}
```

Enumeration (#2293) provides `cameras()`, `displays()`, `windows()`, and
`apps()` returning `Camera { id, name }`, `Display { id, name, width, height }`,
`Window { id, title, app, width, height }`, and `App { id, name }`, each with a
`.source()` builder. All four functions are macOS-only today; every other
platform returns `Error::Unsupported`, with the stated rationale that on Linux
the xdg portal picker owns display selection
(`rs/moq-video/src/capture/mod.rs:365-412`).

The internal stream every backend feeds (`capture/mod.rs:230-292`):

```rust
pub(crate) struct FrameStream {
	chan: Arc<FrameChannel>,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	device: String,
	/// First frame captured during [`open`] (some backends learn their geometry
	/// only from a frame); returned by the first [`read`](Self::read).
	pending: Option<Frame>,
	/// Keeps the backend alive and releases it on drop. Type-erased because it
	/// differs per platform (objc session + delegate, or pump-thread guard).
	_backend: Keepalive,
}
```

Delivery is push into a channel, consumed async. `FrameChannel` is a hand-rolled
bounded queue (`DEPTH = 4`) over `Mutex<VecDeque<Frame>>` plus `tokio::Notify`;
`push` from the foreign producer thread drops the oldest frame when full,
explicitly favoring latency over completeness, and `recv` uses the
register-before-check Notify pattern so it is cancel-safe
(`rs/moq-video/src/capture/channel.rs:18-83`). Blocking pull-style devices
(V4L2, the Media Foundation source reader) are bridged by `pump::spawn`, which
builds the device on a dedicated thread (so `!Send` handles like
`IMFSourceReader` are fine), reports negotiated `Geometry` over a oneshot, loops
`read` until a stop flag, and joins the thread in `PumpGuard::drop` so the
device fd is closed before any reopen races EBUSY
(`rs/moq-video/src/capture/pump.rs:24-120`). Dropping the `FrameStream` is the
release path on every platform.

Device lifecycle is owned by moq track demand end to end. `publish_capture`
advertises the track up front, captures until the catalog rendition exists, then
releases the device whenever the last viewer leaves (`demand.unused()` in a
biased select) and reopens on `demand.used()`, forcing an IDR on each reopen
(`rs/moq-video/src/encode/producer.rs:183-215, 344-379`). The capture layer was
built assuming teardown and re-open cycles, so the PipeWire restore token is a
process-wide static replayed on the next `open`
(`rs/moq-video/src/capture/pipewire.rs:51-55`); there is no equivalent of our
"cannot stop before start" caveat because there is no long-lived capturer object
at all.

### Comparison

- **Pull vs push.** Ours is pull (`pop_frame` on the consumer's thread); theirs
  is push into a shared bounded channel with async `read().await`. Both bound
  the buffer; ours backpressures the producer at depth 2, theirs drops the
  oldest at depth 4. Their model is cancel-safe by construction (drop the
  future, the device closes); ours relies on the pipeline thread noticing a
  `CancellationToken`.
- **Who owns the device lifecycle.** Ours: the capturer object, opened eagerly
  at construction and toggled by `start`/`stop`, with demand-gating bolted on in
  `moq-media/src/publish.rs` and at least one backend (PipeWire) that cannot
  round-trip a stop. Theirs: the `FrameStream`, opened per demand cycle and
  released by drop, with the transport's `Demand` as the single driver. Theirs
  is the cleaner contract; ours carries the state machine in two places.
- **Format negotiation.** Ours negotiates richly: `CameraSelector`
  (HighestFramerate, HighestResolution, TargetResolution) plus
  `preferred_format: Option<CapturePixelFormat>` and a `zero_copy` flag
  (`rusty-capture/src/types.rs:167-259, 320-368`). Theirs treats
  `width`/`height`/`framerate` as hints and lets the backend pick the closest
  mode (`capture/mod.rs:197-220`); there is no pixel-format preference because
  every backend converges on I420 or a platform surface internally.
- **Enumeration.** Ours enumerates on Linux (V4L2 `/dev/video0..63` with FourCC
  format lists, `rusty-capture/src/platform/linux/v4l2.rs:47-95`; libcamera via
  `rpicam-vid --list-cameras`, `libcamera.rs:35-95`; X11 RANDR monitors,
  `x11.rs:32-61`) and macOS. Theirs enumerates only on macOS, but covers
  windows and apps there, which we list (windows) but cannot select by app.

---

## 2. Per-backend comparison

| Backend | Ours | Theirs (moq main) | Zero-copy handle (ours / theirs) | Verdict |
|---|---|---|---|---|
| Linux screen (PipeWire) | `pipewire.rs`, 1,655 lines, portal + DMA-BUF GPU frames, camera too | `pipewire.rs` (#2238), 581 lines, portal, CPU I420 only | DMA-BUF fd / none | Ours wins |
| Linux camera (V4L2) | `v4l2.rs`, 552 lines, v4l2r MMAP, NV12/I420/YUYV/MJPEG/RGBA, enumeration | `v4l2.rs`, 204 lines, v4l MMAP, YUYV + MJPEG to I420 | none / none | Ours slightly, both CPU |
| Linux camera (libcamera raw) | `libcamera.rs`, 268 lines, rpicam-vid I420 pipe | absent | n/a | Ours only |
| Linux camera (libcamera H.264) | `libcamera_h264.rs`, 522 lines, on-device HW encode | absent | on-device ISP-to-encoder DMABUF | Ours only, unique |
| Linux screen (X11) | `x11.rs`, 373 lines, MIT-SHM CPU | absent (no portal, no capture) | none / n/a | Ours only |
| macOS screen (SCK) | `apple/screen.rs`, 394 lines, display + window, BGRA IOSurface | `screencapture.rs`, 434 lines, display + window + app, NV12 IOSurface | CVPixelBuffer / CVPixelBuffer | Theirs, narrowly |
| macOS camera | `apple/camera.rs`, 81 lines, stub; nokhwa CPU fallback | `avfoundation.rs`, 244 lines, working, zero-copy | none (stub) / CVPixelBuffer | Theirs, outright |
| Windows camera | stub (`platform/windows/mod.rs`); nokhwa CPU fallback | `mediafoundation.rs`, 403 lines, D3D11 NV12 texture | none / ID3D11Texture2D | Theirs, outright |
| Windows screen | stub; xcap CPU fallback | `desktopduplication.rs`, 351 lines, DXGI DD, CPU I420 | none / none | Theirs, outright |
| Cross-platform fallbacks | `nokhwa_impl.rs` 246 lines, `xcap_impl.rs` 175 lines, CPU RGBA | none (nokhwa deliberately replaced) | none | Ours only, by design divergence |
| Android | stub (`platform/android/mod.rs`, MediaProjection + Camera2 plan) | absent, no plan | planned AHardwareBuffer / n/a | Ours only (planning) |

### Linux screen: PipeWire

Ours is the deepest backend on either side. `PipeWireScreenCapturer`
(`rusty-capture/src/platform/linux/pipewire.rs:1347`) negotiates the source
through the XDG ScreenCast portal via `ashpd`, accepts a
`pipewire_restore_token` in `ScreenConfig` to skip the picker on reconnect, and
exposes the fresh token back to the caller
(`pipewire.rs:1378-1381, 1434-1436`). When the compositor delivers
`SPA_DATA_DmaBuf` buffers, the fd is duplicated and wrapped as a
`PipeWireDmaBufFrame` implementing `GpuFrameInner`, exposing
`NativeFrameHandle::DmaBuf` with DRM fourcc, modifier, and per-plane layout
(`pipewire.rs:145-247, 743-766`). The DRM mapping covers NV12, BGRA, BGRx,
RGBA, RGBx, and YUYV (`pipewire.rs:114-133`); anything unmapped falls back to
mmap plus copy. The same file also implements `PipeWireCameraCapturer`
(`pipewire.rs:1513`) for portal cameras, which theirs does not have at all.

Theirs (#2238) is deliberately CPU-only: the format offer carries no dmabuf
modifiers, so buffers stay in shared memory and a dedicated PipeWire loop thread
converts BGRx/BGRA to CPU I420 per frame
(`rs/moq-video/src/capture/pipewire.rs:383, 424`). It is also feature-gated off
by default because libpipewire is a build-time link dependency. Where theirs is
ahead: the restore token is a process-wide static replayed automatically across
demand-driven reopens, forgotten when the compositor ends the stream so a
revoked grant re-prompts (`pipewire.rs:51-55` and module doc `:9-18`); a
damage-driven compositor is re-paced by re-emitting the last frame each frame
interval so a static screen does not starve the encoder (`:18-20`); and `open`
fails fast on a 10 s format timeout or 5 s first-frame timeout rather than
handing the encoder a dead stream (`:41-49`). Ours threads the token through
the caller instead, and, as noted in section 1, our capturer object cannot
survive `stop()`, which is exactly the operation demand-gating wants.

Verdict: keep ours for the zero-copy path and the camera support, and port
three of their behaviors: automatic token replay across reopens, static-screen
re-pacing, and teardown-per-cycle instead of `stop()`.

### Linux camera: V4L2

Ours (`rusty-capture/src/platform/linux/v4l2.rs`) streams MMAP buffers through
`v4l2r` with four buffers (`v4l2.rs:146, 278`), blocks on `dqbuf` in
`pop_frame` (`v4l2.rs:357`), and converts by negotiated format to NV12, I420,
RGBA, or passthrough packed frames, decoding MJPEG through the `image` crate to
RGBA (`v4l2.rs:462-463, 540-542`). Enumeration scans `/dev/video0..63`, filters
to capture-capable devices, and reports per-device FourCC format lists
(`v4l2.rs:47-95`), feeding the `CameraSelector` negotiation. The module doc
advertises `VIDIOC_EXPBUF` DMA-BUF export, but the field is `dead_code` and the
delivered frames are CPU (`v4l2.rs:14, 161`).

Theirs (`rs/moq-video/src/capture/v4l2.rs`) is a quarter the size: the `v4l`
crate on the pump thread, YUYV resampled directly to I420 and MJPEG decoded
with pure-Rust `zune-jpeg` then converted, with the header stating plainly that
this is the CPU path feeding NVENC, VAAPI, and openh264 and there is no GPU
surface (`v4l2.rs:1-7`). Device selection is by index or path string; there is
no format enumeration API.

Verdict: both are CPU paths in practice, so the honest comparison is breadth
against economy. Ours keeps richer enumeration, format negotiation, and NV12
passthrough (useful because our VAAPI encoder wants NV12); theirs decodes MJPEG
to I420 in one hop where ours goes MJPEG to RGBA and reconverts downstream.
Keep ours; steal the zune-jpeg-to-I420 shortcut. The EXPBUF zero-copy claim in
our module doc should either be implemented or deleted.

### macOS screen: ScreenCaptureKit

Ours (`rusty-capture/src/platform/apple/screen.rs`) captures displays
(`screen.rs:276`) and single windows (`new_window`, `screen.rs:238`),
enumerating both via `SCShareableContent` (`screen.rs:95-133`). The
`SCStreamOutputTrait` callback wraps each IOSurface-backed `CVPixelBuffer` as an
`AppleGpuFrame` in BGRA (`screen.rs:207-209`) and pushes it into
`sync_channel(2)` (`screen.rs:331`). Missing Screen Recording permission is
detected with `CGPreflightScreenCaptureAccess` but only warned about
(`screen.rs:41-54`); capture then silently produces nothing.

Theirs (`rs/moq-video/src/capture/screencapture.rs`) covers display, window,
and whole-application capture, differing only in the `SCContentFilter` each
builds (#2293), requests NV12 output
(`kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`, `screencapture.rs:26`),
filters window listings to layer 0 so the dock and menu bar do not pollute the
list (`screencapture.rs:41-45`), and fails `open` if no first frame arrives
within 5 s, which converts a missing TCC grant into an error instead of a hang
(`screencapture.rs:36-38`). The `CMSampleBuffer` to surface extraction is
shared with the camera backend in a 20-line `surface.rs`.

Verdict: theirs, narrowly. Feature-wise the gap is app capture and fail-fast
permission handling. Format-wise NV12 matters: their surface enters
VideoToolbox in the encoder's native layout, while our BGRA surface makes VT
do the color conversion internally. Both are genuinely zero-copy deliveries.

### macOS camera

Ours is a stub. `apple/camera.rs` is 81 lines whose `new()` bails with
"AVFoundation camera capture is not yet implemented; enable the `nokhwa`
feature" (`camera.rs:41-47`), and the facade cascade deliberately orders nokhwa
before AVFoundation because of it (`rusty-capture/src/lib.rs:198-203`). The
working path on macOS is therefore nokhwa: CPU RGBA, no surface, no zero-copy.

Theirs is complete: `avfoundation.rs` runs `AVCaptureVideoDataOutput` with a
delegate that wraps each IOSurface-backed `CVPixelBuffer` as `Frame::Surface`,
so "frames reach VideoToolbox with no copy and no color conversion"
(`avfoundation.rs:1-6`). It gates on TCC authorization with a 60 s prompt
timeout, applies a 5 s first-frame timeout, and enumerates cameras by
AVFoundation `uniqueID` without needing the grant (`avfoundation.rs:29-56`).

Verdict: theirs, outright. This is the single largest quality gap in their
favor alongside Windows.

### Windows

Ours has documentation-only stubs describing an intended WGC/DXGI plus Media
Foundation design (`rusty-capture/src/platform/windows/mod.rs`), and `build.rs`
sets a `capture_fallback` cfg so xcap and nokhwa serve as CPU fallbacks. Theirs
has real backends. `mediafoundation.rs` drives an `IMFSourceReader` with a
D3D11 DXGI device manager and the advanced video processor so each sample
arrives as a GPU-resident NV12 texture (`Frame::Texture`) that the hardware
encoder MFT consumes zero-copy on the same device, with a software-processor
CPU I420 fallback for GPU-less hosts (`mediafoundation.rs:1-10`); device
selection uses `MFEnumDeviceSources` friendly names or index.
`desktopduplication.rs` duplicates a monitor via DXGI Desktop Duplication,
copies the BGRA desktop texture through a staging texture to CPU I420,
whole-monitor only, with paced re-emission for static screens and the pump
thread hosting the `!Send` duplication handle (`desktopduplication.rs:1-15`).

Verdict: theirs, outright, on both camera and screen. Note their screen path is
not zero-copy either (BGRA to CPU I420); only their camera path keeps frames on
the GPU.

### Backends only we have

- **libcamera raw** (`platform/linux/libcamera.rs`): spawns
  `rpicam-vid --codec yuv420` and reads exact-size I420 frames from the pipe
  (`libcamera.rs:213-252`). CPU, Pi-focused, with `rpicam-vid --list-cameras`
  enumeration.
- **libcamera H.264** (`platform/linux/libcamera_h264.rs`): the only encoded
  backend on either side. `rpicam-vid --codec h264 --inline --flush` runs the
  Pi's hardware encoder using the ISP-to-encoder DMABUF path internally
  (`libcamera_h264.rs:8-10`); the source splits the Annex-B stream into access
  units, detects IDR NALs, extracts SPS/PPS into an avcC description for the
  catalog, and retries the Pi's exclusive-camera lock with backoff
  (`libcamera_h264.rs:174-213`). moq has nothing like this; their encoder
  always runs. On a Pi Zero 2 this is the difference between working and not.
- **X11** (`platform/linux/x11.rs`): MIT-SHM CPU capture with RANDR multihead
  enumeration, explicitly documented as having no zero-copy path
  (`x11.rs:7`). moq's Linux screen story without the `pipewire` feature is
  `Error::Unsupported` (`capture/mod.rs`, the `Source::Display` arm), so we
  cover X11-only and portal-less systems they cannot.
- **nokhwa and xcap** (`platform/nokhwa_impl.rs`, `platform/xcap_impl.rs`):
  CPU cross-platform fallbacks. moq removed nokhwa on purpose ("replacing
  nokhwa", their `v4l2.rs:1`); for us they are the only working path on macOS
  camera and all of Windows, which is a symptom, not a feature.
- **Android**: our stub documents a MediaProjection plus Camera2 plan with
  AHardwareBuffer zero-copy, and `rusty-codecs` already carries the
  `NativeFrameHandle::HardwareBuffer` variant and MediaCodec backends. moq has
  no Android capture surface at all.

### Backends only they have

Windows Desktop Duplication and Windows Media Foundation (covered above), and
system-audio capture (section 4).

---

## 3. Zero-copy capture delivery

The question that matters is not "does a GPU handle exist" but "does the frame
reach the encoder without a download".

### Ours

The vocabulary is `FrameData::Gpu(GpuFrame)` whose `native_handle()` yields a
platform-gated `NativeFrameHandle`: `DmaBuf(DmaBufInfo)` on Linux with fd,
modifier, DRM format, and per-plane offset/pitch,
`CvPixelBuffer` on macOS, and `HardwareBuffer` on Android
(`rusty-codecs/src/format.rs:68-87, 505-525`).

- **Linux screen (PipeWire)**: delivers `NativeFrameHandle::DmaBuf`. The VAAPI
  encoder imports it directly as a VA surface through a
  `VADRMPRIMESurfaceDescriptor`, no CPU mapping permitted on that variant
  (`rusty-codecs/src/codec/vaapi/encoder.rs:40-51, 87-91, 194`). Capture to
  encode is zero-copy today when the compositor hands out DMA-BUFs and the
  format maps; SHM streams fall back to CPU.
- **macOS screen (SCK)**: delivers a retained BGRA `CVPixelBuffer`. The VTB
  encoder detects `FrameData::Gpu` and feeds the captured buffer straight into
  `VTCompressionSession` (`rusty-codecs/src/codec/vtb/encoder.rs:245-253`).
  Zero-copy, with VT performing BGRA to 4:2:0 conversion internally.
- **Linux camera (V4L2)**: CPU MMAP today; EXPBUF documented, not implemented.
- **macOS camera**: nokhwa, CPU. No surface.
- **Pi camera (libcamera_h264)**: the zero-copy happens inside the device
  (ISP to encoder DMABUF within rpicam-vid); what crosses the process boundary
  is already compressed H.264, which is arguably the strongest form of
  "no download" available.
- **Windows, Android**: nothing (stubs).

The same handles also feed our renderers (DMA-BUF into Vulkan/wgpu and EGL,
CVPixelBuffer into Metal, `rusty-codecs/src/render/`), a consumer moq does not
have at all: their crate has no decode-to-render GPU handoff.

### Theirs

The vocabulary is the crate-private `Frame` enum
(`rs/moq-video/src/frame.rs:23-36`): `Surface` (macOS `CVPixelBuffer`),
`Texture` (Windows D3D11 NV12), `Cuda` (Linux NVDEC output only), and CPU
`I420`.

- **macOS camera and screen**: `Frame::Surface`, cloned as a retain, consumed
  by the VideoToolbox backend as `surface.buffer.clone()` into `encode_frame`
  (`encode/backend/videotoolbox.rs:162-166`). Zero-copy on both sources, in
  NV12, and pinned by a hardware test.
- **Windows camera**: `Frame::Texture` on the source reader's D3D11 device; the
  encoder MFT binds the same device through the DXGI manager and consumes the
  NV12 texture in place (`encode/backend/mediafoundation.rs:1-14`). Zero-copy.
- **Windows screen**: CPU (BGRA staging download to I420). Not zero-copy.
- **Linux screen and camera**: CPU I420 always. Their only Linux GPU frames are
  `Frame::Cuda` from NVDEC, which is the transcode path, not capture.

### Scorecard

Capture-to-encode zero-copy today: ours on Linux screen (DMA-BUF to VAAPI) and
macOS screen (CVPixelBuffer to VT), plus the Pi on-device encode; theirs on
macOS screen, macOS camera, and Windows camera (surface or texture straight
into the platform encoder). Neither side has Linux camera zero-copy. The two
inventories are complementary rather than comparable: they own the
Apple-and-Windows column, we own the Linux column and the only
capture-to-render GPU path.

---

## 4. Audio capture

### Ours: moq-media audio_backend

`moq-media/src/audio_backend.rs` (2,445 lines) plus `audio_backend/aec.rs`
(392 lines) is a full duplex audio engine, not just capture: cpal input and
output streams connected to fixed-resample ring buffers, with AEC and peak
metering applied inline in the real-time callbacks, everything internally at
48 kHz stereo, and a dedicated driver thread handling stream lifecycle, device
switching, and error recovery (`audio_backend.rs:1-23`). The AEC is real
acoustic echo cancellation via `sonora` (a webrtc-audio-processing lineage),
with a render-reference ring buffer written from the output mix and drained in
the capture path (`audio_backend/aec.rs:1-8`, `audio_backend.rs:1059-1060,
1156-1160`), runtime-toggleable (`set_aec_enabled`,
`audio_backend.rs:178-189`). Device enumeration exists on both directions
(`list_inputs`/`list_outputs`, `audio_backend.rs:157-162, 706-718`). There is
no system-audio capture and no explicit OS permission preflight; a denied macOS
mic would surface as a cpal stream error rather than a guided prompt.

### Theirs: moq-audio capture

`rs/moq-audio/src/capture.rs` (326 lines) plus `capture/permission.rs` and
`capture/screencapture.rs` (379 lines). The public surface is
`Source::{Microphone(Option<String>), System}` with `devices()` enumeration
returning `Device { id, name, default }` (`capture.rs:26-48, 236-262`). The mic
path is cpal with the realtime callback forwarding converted f32 buffers over a
channel, a 5 s first-buffer timeout so a TCC-denied mic errors instead of
hanging, and an explicit AVFoundation TCC pre-check that triggers the system
prompt and awaits it via oneshot with a 30 s timeout
(`capture.rs:50-54, 151-215`, `capture/permission.rs:14-80`). `System` captures
desktop audio through ScreenCaptureKit (macOS has no loopback device), pinning
the mandatory video side to a 2x2 frame at 1 fps and excluding the process's
own audio to avoid feedback (`capture/screencapture.rs:1-13, 38-41, 81-98`).
`format(&Config)` reports the capture format without opening the device so the
catalog can be registered up front, and `encode::publish_capture` opens the
device only while `track.used()` and releases it on `track.unused()`,
re-anchoring the PTS epoch on resume (`capture.rs:99-116`,
`encode/capture.rs:13-100`).

Verified absence: `git grep -iE "aec|echo"` over `rs/moq-audio` at main HEAD `3a3e0ea8`
returns nothing but a changelog line and a doc comment about echoing
timestamps. They have no echo cancellation, no noise suppression, and no
output-side engine at all in the capture layer.

Verdict: not the same problem being solved. Ours is a conferencing audio engine
(duplex, AEC, metering, device switching); theirs is a clean capture-to-publish
pipe with better OS integration (system audio, TCC prompting, demand-gating,
format-without-open). A migration that adopted their capture shape would need
our AEC engine layered between capture and encode, and our engine would benefit
from their permission preflight and their system-audio source.

---

## 5. Verdict

Per backend:

- **Linux screen (PipeWire)**: keep ours. Theirs is CPU-only; ours delivers
  DMA-BUF into VAAPI. Port their restore-token replay, static-screen
  re-pacing, and open-per-demand-cycle lifecycle. Upstream candidate: the
  DMA-BUF path itself would be a major contribution to their #2238 backend.
- **Linux camera (V4L2)**: keep ours (enumeration, format negotiation, NV12
  passthrough); adopt their zune-jpeg MJPEG-to-I420 decode; implement or delete
  our EXPBUF claim.
- **libcamera raw and libcamera H.264**: keep, ours only. The pre-encoded
  `rpicam-vid` H.264 source has no upstream equivalent and no home in their
  architecture short of adding a pre-encoded source concept to
  `publish_capture`; it is the strongest upstream candidate we have, and also
  the one most worth protecting in any migration.
- **X11, nokhwa, xcap**: keep as fallbacks; they cover portal-less Linux and
  are currently the only working path for macOS camera and Windows, which the next two
  items should eliminate.
- **macOS screen**: adopt theirs (app capture, NV12 surfaces, fail-fast
  permissions); ours is functional but strictly a subset plus BGRA.
- **macOS camera**: adopt theirs. Ours is a stub; theirs is a complete
  zero-copy backend with TCC handling.
- **Windows camera and screen**: adopt theirs. We have documentation stubs;
  they have working Media Foundation and Desktop Duplication backends, with the
  camera path GPU-resident into the encoder.
- **Android**: keep our plan and frame vocabulary (`HardwareBuffer` handle,
  MediaCodec backends in rusty-codecs); they have nothing here.
- **Audio**: keep our AEC engine, adopt their capture surface (system audio,
  `devices()`, TCC prompt, demand-gated open, format-without-open). Neither
  side subsumes the other.
- **Abstraction**: their demand-driven, drop-to-release `FrameStream` lifecycle
  is the better contract and would have prevented our PipeWire
  cannot-stop-before-start wart (`moq-media/src/publish.rs:1109-1113`). Our
  trait-plus-facade model earns its keep only through backend breadth and the
  GPU frame vocabulary; if the breadth migrates, the trait can shrink.

One migration freebie to record: our `VideoFormat::pixel_format` can only say
`Rgba` or `Bgra` (`rusty-codecs/src/format.rs:49-66`), so every YUV-producing
backend misreports its format and leans on downstream code inspecting the
`FrameData` variant, documented in the source itself
(`rusty-capture/src/platform/linux/libcamera.rs:168-172`). Their model has no
such gap because capture output is normalized to exactly one CPU format (I420)
or a typed platform surface (`rs/moq-video/src/frame.rs:23-36`); adopting that
frame model fixes the misreport by construction.

Upstream candidates, in order of value to them: PipeWire DMA-BUF capture
delivery, the libcamera_h264 pre-encoded source concept, and Linux device
enumeration (V4L2 FourCC scanning) to fill out their macOS-only `cameras()`.
