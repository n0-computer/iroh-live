# Code Map: `rusty-capture`

> Campaign: upstream | Kind: map | Read ../../0-overview.md first; index at ../0-index.md.

Scope: `rusty-capture/src/**` only. iroh-live's owned capture layer (camera + screen).
All citations are `file:line` against the repo at the time of writing.

`rusty-capture` produces raw video frames (and, on one path, pre-encoded H.264)
for the encode pipeline. It has **no MoQ/iroh dependency**; it depends only on
`rusty-codecs` (for the frame/format/trait vocabulary) plus platform crates.

---

## 1. Core abstraction

### The capture-source trait is not defined here - it is `rusty_codecs::traits::VideoSource`

`rusty-capture` does **not** define its own capture trait. Every backend
implements `VideoSource` (and one implements `PreEncodedVideoSource`), both from
`rusty-codecs`. `lib.rs` re-exports the trait so consumers see it as part of the
capture API (`rusty-capture/src/lib.rs:77-80`).

`rusty-codecs/src/traits.rs:229-241`:

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

**Delivery model = polling / pull.** There is no callback or async stream in the
trait. The consumer calls `pop_frame()` repeatedly. The contract is "the next
frame or `None` if none ready." Backends are driven from an OS thread owned by
the encode pipeline (`spawn_thread`), so a `pop_frame()` that blocks on a kernel
`dqbuf` / pipe read / channel recv is the intended usage
(`rusty-capture/src/platform/linux/v4l2.rs:18-24`,
`rusty-capture/src/platform/linux/x11.rs:9-13`,
`rusty-capture/src/platform/linux/libcamera.rs:122-127`).

Internally, backends split into two camps:
- **Blocking-read backends** (v4l2, x11, libcamera, nokhwa, xcap): `pop_frame`
  directly reads the device / pipe on the caller thread. No internal thread.
- **Channel-fed backends** (PipeWire, Apple ScreenCaptureKit): a callback thread
  owned by the OS API pushes frames into a **bounded** `mpsc::sync_channel(2)`;
  `pop_frame` recvs from it (`rusty-capture/src/platform/linux/pipewire.rs:1381`,
  `rusty-capture/src/platform/apple/screen.rs:331`). Bound = 2, back-pressures
  the callback and drops nothing silently in an unbounded buffer.

### The encoded-output trait: `PreEncodedVideoSource`

`rusty-codecs/src/traits.rs:268-287`:

```rust
/// Produces already-encoded video packets from an external encoder.
///
/// Unlike [`VideoSource`] (raw frames fed to an encoder pipeline), a
/// pre-encoded source yields [`EncodedFrame`]s directly. This is used when
/// the capture device or external tool performs hardware encoding internally
/// (e.g. `rpicam-vid --codec h264` on Raspberry Pi, hardware RTSP cameras,
/// or file demuxers).
pub trait PreEncodedVideoSource: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> VideoConfig;      // codec config for the MoQ catalog
    fn start(&mut self) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
    fn stop(&mut self) -> Result<()>;
}
```

Only `LibcameraH264Source` implements this in the capture crate.

### The frame data model (`rusty_codecs::format`)

`VideoFrame` is the unit `VideoSource::pop_frame` yields
(`rusty-codecs/src/format.rs:576-587`):

```rust
pub struct VideoFrame {
    pub dimensions: [u32; 2],
    pub data: FrameData,
    pub timestamp: Duration,   // ZERO for capture frames until encoder assigns PTS
    cached_rgba: OnceLock<RgbaImage>, // lazy RGBA cache for rendering
}
```

Backing storage is the `FrameData` enum (`rusty-codecs/src/format.rs:544-566`):

```rust
pub enum FrameData {
    Packed { pixel_format: PixelFormat, data: bytes::Bytes },  // CPU RGBA/BGRA
    I420 { y: Bytes, u: Bytes, v: Bytes },                     // CPU planar YUV420
    Nv12(Nv12Planes),                                          // CPU semi-planar
    Gpu(GpuFrame),                                             // GPU-resident, zero-copy
}
```

Constructors used by backends: `VideoFrame::new_rgba`, `new_packed`, `new_i420`,
`new_nv12`, and GPU frames via `GpuFrame::new(Arc<dyn GpuFrameInner>)`
(`rusty-codecs/src/format.rs:600-620`).

Two pixel-format vocabularies exist and must not be confused:
- `rusty_codecs::format::PixelFormat` is **only `Rgba | Bgra`**
  (`rusty-codecs/src/format.rs:49-57`) and is what `VideoFormat` advertises.
- `rusty_capture::types::CapturePixelFormat` is the richer camera-facing set:
  `Rgba, Bgra, Nv12, I420, Yuyv, Mjpeg, Rgb, Gray`
  (`rusty-capture/src/types.rs:265-284`), with V4L2 FourCC conversions
  (`from_v4l2_fourcc` / `to_v4l2_fourcc`, `types.rs:286-317`).

Note a modeling gap: `VideoFormat` can only say `Rgba/Bgra`, so YUV/NV12 backends
(libcamera, v4l2) report `PixelFormat::Rgba` from `format()` and rely on
downstream code inspecting the actual `FrameData` variant
(`rusty-capture/src/platform/linux/libcamera.rs:168-175`).

### Zero-copy GPU handle model

`FrameData::Gpu` wraps a `GpuFrame` whose `native_handle()` returns a
platform-gated `NativeFrameHandle` (`rusty-codecs/src/format.rs:73-87`):
`DmaBuf(DmaBufInfo)` on Linux, `HardwareBuffer` on Android, `CvPixelBuffer` on
macOS+`apple-gpu`. `DmaBufInfo` carries `fd: OwnedFd`, `modifier`, `drm_format`,
coded/display dims, and per-plane `offset`/`pitch`
(`rusty-codecs/src/format.rs:505-525`). `GpuPixelFormat` = `Nv12 | Bgrx | Bgra`
(`format.rs:528-536`).

### Capture crate's own public types (`rusty-capture/src/types.rs`)

- `CaptureBackend` - `#[non_exhaustive]` enum of the 8 backends: `PipeWire, V4l2,
  X11, ScreenCaptureKit, AVFoundation, Xcap, Nokhwa, Libcamera`
  (`types.rs:11-28`), with `Display`, `cli_name()` (`pw/v4l2/x11/sck/avf/xcap/
  nokhwa/libcamera`), and `FromStr` (`types.rs:30-76`).
- `MonitorInfo` (backend, id, name, position, dimensions, scale_factor,
  refresh_rate_hz, is_primary) - `types.rs:96-115`.
- `WindowInfo` (macOS window capture metadata) - `types.rs:117-134`.
- `CameraInfo` (backend, id, name, `supported_formats: Vec<CameraFormat>`) -
  `types.rs:136-147`.
- `CameraFormat` (dimensions, fps, `pixel_format: CapturePixelFormat`) -
  `types.rs:150-165`.
- `CameraSelector` - `HighestFramerate | HighestResolution(default) |
  TargetResolution(w,h)`, with the format-selection algorithm
  (`types.rs:167-259`).
- `CameraConfig` (selector, `preferred_format: Option<CapturePixelFormat>`,
  `zero_copy: bool` default true) - `types.rs:320-343`; `select_format()` filters
  by preferred format then applies the selector (`types.rs:345-368`).
- `ScreenConfig` (target_fps default 30, show_cursor default true,
  `pipewire_restore_token: Option<String>`) - `types.rs:370-394`.

### High-level façade capturers (`lib.rs`)

`CameraCapturer` (`lib.rs:362-529`) and `ScreenCapturer` (`lib.rs:535-685`) are
thin `Box<dyn VideoSource>` wrappers that auto-select a backend and forward all
five trait methods. Selection logic:
- `list_monitors` / `list_cameras` - preferred-backend **cascade**
  (`lib.rs:132-214`); `list_all_*` - union across compiled-in backends
  (`lib.rs:156-241`).
- Camera preference order: libcamera → v4l2 → PipeWire(if running) → nokhwa →
  AVFoundation (`lib.rs:182-214`). Screen: PipeWire(if running) → x11 → SCK →
  xcap (`lib.rs:132-152`).
- `create_camera_backend` / `create_screen_backend` dispatch on `info.backend`
  (`lib.rs:247-354`).
- Runtime PipeWire detection via `$XDG_RUNTIME_DIR/pipewire-0` socket or
  `pidof pipewire` (`lib.rs:85-99`); when live, screen/camera use portal
  **placeholder** `MonitorInfo`/`CameraInfo` (`lib.rs:101-123`) because the
  portal picks the actual source interactively.
- Ctor surface: `CameraCapturer::new / with_selector / with_index / with_format /
  open / with_backend / with_config`; `ScreenCapturer::new / open / with_backend /
  with_monitor / list_windows / with_window`.

---

## 2. Platform backends

Module tree: `platform/mod.rs` gates `linux`, `apple`, `windows`, `android` by
`target_os` and `xcap_impl`/`nokhwa_impl` by feature (`platform/mod.rs:1-27`).
Modules are `pub(crate)`; concrete capturers are re-exported at the crate root
(`lib.rs:57-81`).

| Backend | Kind | Underlying API / dep | Frame output | Zero-copy | Source |
|---|---|---|---|---|---|
| PipeWire | screen + camera | `pipewire` 0.9 + `libspa` + `ashpd` (XDG portal) + `nix` + `tokio` | NV12 DMA-BUF → `FrameData::Gpu`; else CPU | **Yes (DMA-BUF, NV12 only)** | `platform/linux/pipewire.rs` |
| V4L2 | camera | `v4l2r` 0.0.7 + `nix` | NV12 / I420 / RGBA CPU (MMAP) | Documented EXPBUF, currently CPU/MMAP | `platform/linux/v4l2.rs` |
| X11 | screen | `x11rb` (MIT-SHM + RANDR) | RGBA CPU (SHM copy) | No | `platform/linux/x11.rs` |
| libcamera (YUV) | camera | `rpicam-vid --codec yuv420` subprocess | I420 CPU (pipe) | No | `platform/linux/libcamera.rs` |
| libcamera H264 | camera, **encoded** | `rpicam-vid --codec h264` subprocess | **`EncodedFrame` H.264 Annex-B** | n/a (on-device HW encode) | `platform/linux/libcamera_h264.rs` |
| ScreenCaptureKit | screen (+window) | `screencapturekit` 1.5 + objc2 (macOS 12.3+) | BGRA IOSurface `CVPixelBuffer` → `FrameData::Gpu` | **Yes (IOSurface/CVPixelBuffer)** | `platform/apple/screen.rs` |
| AVFoundation | camera | objc2-av-foundation | - | - (**stub, non-functional**) | `platform/apple/camera.rs` |
| nokhwa | camera | `nokhwa` 0.10 (v4l2/AVFoundation/MediaFoundation) | RGBA CPU | No | `platform/nokhwa_impl.rs` |
| xcap | screen | `xcap` 0.9 (X11/Wayland/mac/Win) | RGBA CPU, sleep-based fps limiter | No | `platform/xcap_impl.rs` |
| Windows | - | stub only (WGC/DXGI/MediaFoundation plan) | - | planned D3D11 | `platform/windows/mod.rs` |
| Android | - | stub only (MediaProjection/Camera2 plan) | - | planned AHardwareBuffer | `platform/android/mod.rs` |

Details:

**PipeWire** (`platform/linux/pipewire.rs`, 1655 lines) - the richest backend.
Both `PipeWireScreenCapturer` (`:1347`) and `PipeWireCameraCapturer` (`:1513`)
own a `mpsc::Receiver<VideoFrame>` fed from a PipeWire stream callback via
`mpsc::sync_channel(2)` (`:1381`, `:1570`). Screen capture negotiates a source
through the XDG ScreenCast portal (`ashpd`), supports a
`pipewire_restore_token` to skip the picker on reconnect
(`:1373-1441`, exposed via `pipewire_restore_token()` `:1436`). When PipeWire
delivers `SPA_DATA_DmaBuf` buffers, the FD is dup'd and wrapped as a
`PipeWireDmaBufFrame` implementing `GpuFrameInner`, exposing
`NativeFrameHandle::DmaBuf` for direct VAAPI import (`:14-16`, `:145-247`,
`:721-766`). Only NV12 DMA-BUFs take the zero-copy path (`:721`); others fall
back to CPU frames.

**V4L2** (`platform/linux/v4l2.rs`, 552 lines) - `V4l2CameraCapturer` (`:158`)
uses `v4l2r` MMAP streaming with `NUM_BUFFERS = 4` (`:146`, `:277`).
`pop_frame` (`:357`) blocks on `dqbuf` and converts by capture format to
`new_nv12` / `new_i420` / `new_rgba` / `new_packed` (`:423-476`). Despite the
module doc advertising `VIDIOC_EXPBUF` DMA-BUF export (`:8-16`, `:150-152`), the
current `pop_frame` path produces CPU frames; the DMA-BUF export field is marked
`dead_code … "future DMABUF format selection"` (`:161`). Camera enumeration scans
`/dev/video0..63`, filters to `VIDEO_CAPTURE` devices, enumerates FourCC formats
(`cameras()` `:47-95`).

**X11** (`platform/linux/x11.rs`, 373 lines) - `X11ScreenCapturer` (`:134`),
CPU-only MIT-SHM. `monitors()` uses RANDR for multihead, falls back to root
screens (`:32-61`). `pop_frame` (`:311`) calls `shm::get_image` and builds an
RGBA frame (`:353`). Explicitly "No zero-copy path" (`:7`). Not in default
features.

**libcamera raw** (`platform/linux/libcamera.rs`, 268 lines) -
`LibcameraCapturer` (`:128`) spawns `rpicam-vid --codec yuv420`, reads
`w*h*3/2`-byte I420 frames via `read_exact` from stdout, slices into y/u/v and
emits `VideoFrame::new_i420` (`:213-252`). PTS is synthesized from a frame
counter and configured fps (`:235`). `cameras()` probes `rpicam-vid
--list-cameras` (`:35-95`).

**libcamera H264** (`platform/linux/libcamera_h264.rs`, 522 lines) - the **only
encoded backend** and the only `PreEncodedVideoSource`. `LibcameraH264Source`
(`:116`) spawns `rpicam-vid --codec h264 --inline --flush` (`:183-213`), reads
the Annex-B bytestream in 32 KB chunks, splits into access units by scanning
VCL NAL boundaries (`find_first_au_end` `:406-440`), detects IDR via
`contains_idr_nal` (`:376-398`), extracts SPS/PPS from the first keyframe to
build an avcC description (`:326-336`), and yields `EncodedFrame { is_keyframe,
timestamp, payload }` (`:351-355`). `config()` returns a `VideoConfig` with an
`H264` codec (baseline, profile 0x42, `inline: true`,
`optimize_for_latency: Some(true)`) (`:82-103`). `start()` has a retry loop with
exponential backoff for the Pi's single-camera exclusive-lock race (`:174-269`).
Rationale: on Pi Zero 2 this avoids the ~10 MB/s raw-YUV pipe and uses the
ISP→encoder DMABUF path internally (`:8-10`).

**Apple ScreenCaptureKit** (`platform/apple/screen.rs`, 394 lines) -
`MacScreenCapturer` (`:221`) supports display capture (`new` `:276`) and window
capture (`new_window` `:238`). An `SCStreamOutputTrait` callback (`:182`) wraps
each IOSurface `CVPixelBuffer` as an `AppleGpuFrame` (BGRA) →
`GpuFrame::new(Arc::new(...))` (`:207-209`) and pushes it into
`mpsc::sync_channel(2)` (`:331`); `pop_frame` recvs (`:387`). Zero-copy: pixel
data stays in GPU memory for VideoToolbox encode / Metal render (`:1-6`). Checks
`CGPreflightScreenCaptureAccess` and warns if permission missing (`:41-54`).
`windows()` (`:120`) and `monitors()` (`:95`) enumerate via
`SCShareableContent`.

**Apple AVFoundation camera** (`platform/apple/camera.rs`, 81 lines) -
**stub/non-functional**: `cameras()` returns empty, `new()` bails directing
callers to nokhwa; the objc2 sample-buffer delegate is unfinished (`:1-8`,
`:40-56`). This is why `list_cameras` places nokhwa *before* AVFoundation
(`lib.rs:198-210`).

**nokhwa** (`platform/nokhwa_impl.rs`, 246 lines) - cross-platform camera via
`nokhwa` 0.10 with `input-native` + `camera-sync-impl` (making `Camera: Send`,
so `pop_frame` runs on the caller thread, `:6-10`). CPU RGBA only. Maps nokhwa
`FrameFormat` → `CapturePixelFormat` (`:30-39`), enumerates via
`nokhwa::query(ApiBackend::Auto)` (`:42`).

**xcap** (`platform/xcap_impl.rs`, 175 lines) - cross-platform screen via `xcap`
0.9 (X11/Wayland-portal/mac/Win). `XcapScreenCapturer` (`:51`) captures
screenshots via `Monitor::capture_image`, converts to RGBA, and rate-limits with
a sleep-based limiter (`target_interval`, `:46-59`). CPU only.

**Windows / Android** - documentation-only stubs (`platform/windows/mod.rs`,
`platform/android/mod.rs`) describing intended WGC/DXGI + MediaFoundation and
MediaProjection + Camera2 designs with D3D11 / AHardwareBuffer zero-copy. No
code. `build.rs:1-11` sets a `capture_fallback` cfg on Windows so xcap/nokhwa are
used there.

---

## 3. Raw-frame vs. encoded-output split

Two distinct producer traits and two distinct output types:

- **Raw-frame capture** - implements `VideoSource`, yields `VideoFrame`
  (`FrameData` in CPU RGBA/I420/NV12 or GPU DMA-BUF/CVPixelBuffer). This is every
  backend except one: PipeWire, V4L2, X11, libcamera-YUV, ScreenCaptureKit,
  AVFoundation(stub), nokhwa, xcap. The downstream encoder does color conversion
  and PTS assignment.

- **On-device encoded capture** - implements `PreEncodedVideoSource`, yields
  `EncodedFrame` (compressed H.264 Annex-B AUs). **Only `LibcameraH264Source`**
  (`platform/linux/libcamera_h264.rs`). Here `rpicam-vid` runs the Pi's hardware
  H.264 encoder internally, so the crate parses/repackages the bitstream rather
  than raw pixels and supplies a `VideoConfig` (codec params + avcC) for the MoQ
  catalog directly - bypassing iroh-live's encoder entirely. The two libcamera
  backends are a deliberate fork: `libcamera.rs` for raw-to-external-encoder,
  `libcamera_h264.rs` for the pre-encoded fast path (`libcamera.rs:11-14`,
  `libcamera_h264.rs:6-14`). Both share the `libcamera` feature and are
  co-re-exported (`lib.rs:63-66`).

Other backends that *could* emit encoded output (V4L2 M2M on Pi 4, Windows MF,
Android MediaCodec) are only sketched in stubs; today the encoded path is
Pi-only.

---

## 4. Public API surface & Cargo

### `lib.rs` re-exports (`lib.rs:57-81`)

- From `rusty_codecs`: `PixelFormat`, `VideoFormat`, `VideoFrame`, `VideoSource`.
- From `types::*`: all the `Capture*`/`*Info`/`*Config`/`CameraSelector` types.
- Platform capturers, each cfg+feature gated: `AppleCameraCapturer`,
  `MacScreenCapturer`, `LibcameraCapturer`+`LibcameraConfig`,
  `LibcameraH264Source`+`LibcameraH264Config`, `PipeWireCameraCapturer`+
  `PipeWireScreenCapturer`, `V4l2CameraCapturer`, `X11ScreenCapturer`,
  `NokhwaCameraCapturer`, `XcapScreenCapturer`.
- The two façades `CameraCapturer` / `ScreenCapturer` (defined in `lib.rs`).
- Note: `PreEncodedVideoSource` is **not** re-exported by `rusty-capture`;
  consumers of `LibcameraH264Source` import the trait from `rusty-codecs`.

### Dependencies (`rusty-capture/Cargo.toml`)

Core (all targets): `anyhow`, `bytes`, `derive_more`, `tracing`, `image` (jpeg
only), `rusty-codecs` (workspace, **no codec features**), plus optional
cross-platform `xcap` 0.9 / `nokhwa` 0.10.

- Linux target deps: `libc`, `pipewire` 0.9, `libspa` 0.9, `ashpd` 0.11
  (tokio), `nix` 0.30, `tokio` (rt), `v4l2r` 0.0.7, `x11rb` 0.13.
- macOS/iOS: `objc2` + `objc2-foundation` + `objc2-av-foundation` +
  `objc2-core-media` + `objc2-core-video` + `block2` + `dispatch2`.
- macOS-only: `screencapturekit` 1.5, `objc2-app-kit`.

### Feature flags

```
default = ["camera", "screen"]
all     = ["camera","screen","xcap","nokhwa","x11","libcamera"]

camera  = ["camera-linux","camera-apple"]
screen  = ["screen-linux","screen-apple"]

camera-linux = ["pipewire","v4l2"]
camera-apple = [objc2 stack …]
screen-linux = ["pipewire"]                      # x11 NOT default
screen-apple = ["camera-apple", "screencapturekit", "objc2-app-kit", "rusty-codecs/apple-gpu"]

libcamera = ["rusty-codecs/h264"]                # pulls in H264 codec for avcC parsing
pipewire  = ["pipewire","libspa","ashpd","nix","tokio"]
v4l2      = ["v4l2r","nix"]
x11       = ["x11rb"]
xcap      = ["xcap"]
nokhwa    = ["nokhwa"]
```

Note the capability→platform-bundle→low-level layering. Activating a Linux bundle
on macOS is harmless because the deps sit under `[target.'cfg(target_os =
"linux")']` (`Cargo.toml` comments; `lib.rs:32-47`). `libcamera` uniquely enables
a codec feature on `rusty-codecs` (`rusty-codecs/h264`) so the H264 backend can
parse SPS/PPS into avcC. `screen-apple` enables `rusty-codecs/apple-gpu` for the
CVPixelBuffer path.

---

## 5. moq main capture (for comparison)

Cross-repo lookup against `/home/bit/Code/rust/moq` on branch **main**. moq keeps
capture inside its per-medium crates, not a standalone crate.

### `rs/moq-video/src/capture.rs` (256 lines) - webcam/screen via libavdevice

- **No public capture trait.** The public surface is a plain `Config` struct
  (`device: Option<String>`, `width`/`height`/`framerate: Option`,
  `#[non_exhaustive]`, `capture.rs:25-37`). The worker type `Camera`
  (`:44-50`) is **`pub(crate)`** deliberately, to keep `ffmpeg` types out of the
  public API until a bring-your-own-frames consumer needs them (`:39-43`).
- Backend selection is a compile-time `Backend { format_name }` mapping to a
  libavdevice input format: `avfoundation` (macOS), `v4l2` (Linux), `dshow`
  (Windows) (`:170-218`). Everything goes through **ffmpeg/libavdevice**
  (`ffmpeg_next`); there is no per-OS native backend, no PipeWire, no
  ScreenCaptureKit, no DMA-BUF/zero-copy path. Screen capture is described as the
  *same* pipeline with a different input format (`:6-9`).
- Frame model: `Camera::read()` returns `ffmpeg::frame::Video` in the source's
  native pixel format; the encoder converts to YUV420P (`:124-163`). Pull-based
  and blocking, like rusty-capture, but the frame type is an ffmpeg frame, not a
  purpose-built `VideoFrame` enum. Only one variant - no GPU-frame abstraction.

### `rs/moq-audio/src/capture.rs` (369 lines) - microphone via cpal

- Public `Microphone` type (this one *is* `pub`) plus a `Config`
  (`device/sample_rate/channels`, `#[non_exhaustive]`, `:27-34`). Backed by
  **`cpal`** (CoreAudio/WASAPI/ALSA), pure-Rust, no ffmpeg (`:1-6`).
- Delivery: cpal's realtime callback forwards converted interleaved-f32 buffers
  over an `std::sync::mpsc::channel`; `Microphone::read()` recvs a `Frame`
  (`timestamp_us`, `data: Bytes`) (`:40-166`). `!Send` stream, so it must live on
  one thread (`:36-39`). A `FIRST_BUFFER_TIMEOUT` (5s) turns a denied mic into an
  error rather than a silent hang (`:18-22`, `:105-117`).
- Beyond raw capture this file also owns a **publish orchestration** layer absent
  from rusty-capture: `publish_microphone` wires the mic to a MoQ broadcast, and
  a `Gate`/`monitor_demand` mechanism opens the device only while a subscriber is
  listening and releases it when idle, re-anchoring the PTS epoch on resume
  (`:180-329`). rusty-capture has no MoQ awareness - that lives in iroh-live.

### `rs/moq-audio/src/capture/permission.rs` (86 lines) - macOS TCC pre-check

- `ensure_microphone_access()` queries AVFoundation `AVCaptureDevice
  authorizationStatusForMediaType` and, when `NotDetermined`, triggers the system
  prompt via `requestAccessForMediaType_completionHandler` with a 30s
  `PROMPT_TIMEOUT` (`:13-74`). No-op on non-macOS (`:83-86`). Analogous to
  rusty-capture's `CGPreflightScreenCaptureAccess` warning, but moq *fails fast*
  where rusty-capture only warns.

### Comparison summary

moq's capture is **thinner and ffmpeg/cpal-centric**: a `Config`-struct + concrete
worker per medium (`Camera`, `Microphone`) with `read() -> native frame`, and no
shared `VideoSource`-style trait, no backend enum, no zero-copy/GPU frame model -
one libavdevice path covers camera+screen+all OSes. rusty-capture is **broader and
native-backend-centric**: a shared `VideoSource`/`PreEncodedVideoSource` trait
pair, a `CaptureBackend` enum with runtime selection cascades, and first-class
zero-copy (DMA-BUF, IOSurface, planned D3D11/AHardwareBuffer). Both use
pull-based blocking reads driven from a dedicated thread and channel-buffer the
callback-driven backends. Crucially, moq folds MoQ publish/demand-gating into the
capture files, whereas rusty-capture is transport-agnostic (publishing lives in
iroh-live). moq has no video pre-encoded/on-device-H264 equivalent to
`LibcameraH264Source`; its encoder always runs.
