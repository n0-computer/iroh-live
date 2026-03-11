# Zero-Copy Screen/Camera Capture

## Goal

Eliminate CPU round-trips in the capture→encode pipeline. Today every frame
goes through RGBA CPU buffers even when both capture and encode happen on the
GPU. The unified `VideoFrame` / `FrameData` / `NativeFrameHandle` types
(landed in the same branch) provide the plumbing; this plan covers the
capture-side changes needed to fill those types with GPU-native data.

## Current Capture Path

```
xcap (screen) ─► RGBA Vec<u8> ─► VideoFrame::new_rgba() ─► encoder
nokhwa (camera) ─► RGBA Vec<u8> ─► VideoFrame::new_rgba() ─► encoder
```

Every frame is CPU RGBA. The encoder then converts RGBA→YUV on the CPU
(software encoders) or uploads RGBA and converts on the GPU (VAAPI encoder).

## Target Architecture

### Linux — PipeWire (Screen + Camera, unified)

PipeWire is the standard capture API on modern Linux (Wayland). It handles
**both** screen capture (via ScreenCast portal) and camera capture (via Camera
portal). The stream-level code is 100% shared — only the portal negotiation
differs.

```
                       ┌─ ScreenCast portal (ashpd) ─┐
                       │                              ├─► pw_fd + node_id
                       └─ Camera portal (ashpd) ─────┘
                                     │
                          PipeWireCapture (shared)
                                     │
                    ┌────────────────┼────────────────┐
                    ▼                ▼                 ▼
              SPA_DATA_DmaBuf   SPA_DATA_MemFd   SPA_DATA_MemPtr
                    │                │                 │
           FrameData::Gpu      mmap + upload     direct upload
       NativeFrameHandle::DmaBuf    │                 │
                    │                └────────┬────────┘
                    ▼                         ▼
          VAAPI encoder import       FrameData::Packed (fallback)
           (true zero-copy)
```

**Portal layer** (`ashpd` crate):
- `ScreenCast`: `CreateSession` → `SelectSources` → `Start` → `OpenPipeWireRemote`
  (user picks source in compositor dialog; returns pw_fd + node_id)
- `Camera`: `AccessCamera` → `OpenPipeWireRemote`
  (simpler — no source selection; enumerate nodes in PipeWire graph)

**Stream layer** (`pipewire-rs` crate, shared for both):
1. `pw_context_connect_fd(fd)` to get a core
2. Create `pw_stream`, connect to node_id
3. Negotiate format: request NV12 preferred, BGRA fallback; advertise
   `SPA_FORMAT_VIDEO_modifier` with supported DRM modifiers
4. In `param_changed`: if modifiers present, set
   `SPA_PARAM_BUFFERS_dataType = 1 << SPA_DATA_DmaBuf`; else MemFd/MemPtr
5. In `process` callback: check `buf.datas[i].type`:
   - `DmaBuf`: extract fd + modifier + offset/stride → `DmaBufInfo` →
     `VideoFrame::new_gpu(PipeWireGpuFrame, w, h, ts)`
   - `MemFd`/`MemPtr`: mmap, wrap as `VideoFrame::new_packed(...)` (CPU fallback)

**`PipeWireGpuFrame`** holds an `Arc` to the PipeWire buffer; `Drop` returns it
to PipeWire's buffer pool so the compositor/camera can reuse it.

### Linux — V4L2 Camera (non-PipeWire fallback, Raspberry Pi)

For systems without PipeWire (headless Pi, embedded), V4L2 direct capture with
DMA-BUF export provides zero-copy without a session manager.

```
V4L2 camera ─► VIDIOC_EXPBUF ─► DMA-BUF fd (NV12/YUYV)
                                      │
                            VideoFrame::new_gpu(V4l2GpuFrame)
                            NativeFrameHandle::DmaBuf(..)
                                      │
                            VAAPI / V4L2 M2M encoder import
```

1. `VIDIOC_REQBUFS` with `V4L2_MEMORY_MMAP` to allocate N kernel buffers
2. `VIDIOC_EXPBUF` on each buffer index → DMA-BUF fds
3. QBUF/STREAMON/DQBUF loop; on each dequeued frame, wrap fd as `DmaBufInfo`
4. Encoder imports via `V4L2_MEMORY_DMABUF` (V4L2 M2M) or
   `VASurfaceAttribExternalBuffers` (VAAPI)
5. After encode completes, QBUF the buffer back to camera

**Raspberry Pi specifics:**
- Pi Camera Module v2/v3: libcamera driver (not legacy bcm2835-v4l2).
  libcamera outputs DMA-BUF natively — every `FrameBuffer::Plane` has a fd.
  PipeWire's `spa-libcamera` plugin exposes these as PipeWire nodes.
- Pi 4: has hardware H.264 encoder (`/dev/video11`, V4L2 M2M). Full zero-copy:
  camera DMA-BUF → encoder `V4L2_MEMORY_DMABUF` → encoded NALUs.
- Pi 5: **no hardware H.264 encoder**. Software encode only (openh264).
  DMA-BUF still useful to avoid one CPU copy (mmap + read vs RGBA conversion).
- Output formats: NV12 (preferred), YU12, YUYV, MJPEG (USB cameras).
- EXPBUF requires `videobuf2` driver with `vb2-dma-contig` or `vb2-dma-sg`.
  Most modern drivers (including Pi) support it. UVC (USB) cameras also support
  it but buffers are in system RAM, not CMA — limits downstream zero-copy.

### Linux — X11 Screen Capture (non-PipeWire fallback)

No zero-copy path exists for X11. MIT-SHM avoids the X socket transfer but the
X server always copies the framebuffer into the SHM region. BGRX format,
requires channel swap. Use as CPU-only fallback.

```
X11 (MIT-SHM) ─► mmap BGRX ─► VideoFrame::new_packed(Bgra) ─► encoder
```

### macOS — ScreenCaptureKit + AVFoundation (unified via CVPixelBuffer)

Both screen and camera capture produce IOSurface-backed `CVPixelBuffer`. The
downstream path is 100% shared — only the capture setup differs.

```
ScreenCaptureKit ─► CMSampleBuffer ─► CVPixelBuffer (IOSurface-backed)
AVFoundation cam ─► CMSampleBuffer ─► CVPixelBuffer (IOSurface-backed)
                                              │
                                    VideoFrame::new_gpu(AppleGpuFrame)
                                    NativeFrameHandle::IoSurface(..)
                                              │
                                    VTCompressionSessionEncodeFrame()
                                       (true zero-copy)
```

- `screencapturekit` crate (v1.5.0, 367k downloads): enumerate displays/windows
  via `SCShareableContent`, configure `SCStream`, receive frames in callback.
- `objc2-av-foundation` (v0.3.2): `AVCaptureSession` + `AVCaptureVideoDataOutput`
  for camera. Delegate callback delivers `CMSampleBuffer`.
- Both produce `CVPixelBuffer` → `CVPixelBufferGetIOSurface()` → pass directly
  to VideoToolbox encoder. No CPU readback at any point.

### Windows — DXGI/WGC + MediaFoundation

Screen capture is GPU-native; camera always starts on CPU (USB transport).

```
DXGI Dup / WGC ─► ID3D11Texture2D (GPU) ─► MF encoder (D3D11-aware)
MF Camera ─► CPU buffer ─► ID3D11Texture2D (upload) ─► MF encoder
```

- `windows-capture` crate (v1.5.0, 130k downloads): WGC-based, gives
  `ID3D11Texture2D` for screen capture. Supports window-level capture.
- Camera via MediaFoundation always produces CPU buffers (USB limitation).
  Set `MF_SOURCE_READER_D3D_MANAGER` to upload to GPU early, then keep on GPU.
- Both converge at `IMFSample` wrapping `ID3D11Texture2D` → MF H.264 encoder
  with `IMFDXGIDeviceManager`.

## `NativeFrameHandle` Variants (Roadmap)

```rust
#[derive(Debug)]
#[non_exhaustive]
pub enum NativeFrameHandle {
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBufInfo),          // ← landed

    // Future:
    // #[cfg(target_os = "macos")]
    // IoSurface(..),             // CVPixelBuffer / IOSurface ref
    // #[cfg(target_os = "windows")]
    // D3D11Texture(..),          // ID3D11Texture2D handle
}
```

## Code Sharing Analysis

### Shared across all platforms

| Component | Shared? | Notes |
|-----------|---------|-------|
| `VideoFrame` / `FrameData` / `NativeFrameHandle` | 100% | Already landed |
| `GpuFrameInner` trait | 100% | Platform backends implement it |
| Encoder dispatch (`push_frame` match on `FrameData`) | 100% | Already landed |
| `WgpuVideoRenderer` | 100% | DMA-BUF import path already works |

### Per-platform code structure

```
capture/
├── mod.rs                    # VideoSource trait, re-exports
├── pipewire.rs               # Linux PipeWire (screen + camera, shared stream)
│   ├── portal_screen.rs      # ScreenCast portal setup (~50 lines)
│   └── portal_camera.rs      # Camera portal setup (~30 lines)
├── v4l2.rs                   # Linux V4L2 direct camera (~200 lines)
├── x11.rs                    # Linux X11 screen fallback (~100 lines)
├── apple_screen.rs           # macOS ScreenCaptureKit (~40 lines)
├── apple_camera.rs           # macOS AVFoundation camera (~60 lines)
├── apple_common.rs           # Shared CVPixelBuffer→GpuFrame (~50 lines)
├── win_screen.rs             # Windows WGC/DXGI screen (~120 lines)
├── win_camera.rs             # Windows MF camera (~150 lines)
└── win_common.rs             # Shared D3D11 texture→GpuFrame (~80 lines)
```

**Linux PipeWire**: portal setup is different (~80 lines divergent), but stream
consumption is identical (~250 lines shared). Camera portal is simpler than
ScreenCast (no session/source selection).

**macOS**: capture setup differs (ScreenCaptureKit ~40 lines vs AVFoundation
~60 lines), but CVPixelBuffer→GpuFrame is 100% shared (~50 lines).

**Windows**: screen capture is GPU-native, camera needs upload step. Shared
D3D11→GpuFrame wrapper (~80 lines), divergent capture setup (~270 lines total).

## Implementation Order

| Phase | Work | LoC | Benefit |
|-------|------|-----|---------|
| 1 | PipeWire screen+camera capture with DMA-BUF | ~400 | Zero-copy on Linux Wayland (screen + camera) |
| 2 | VAAPI encoder DMA-BUF import | ~150 | Completes zero-copy encode loop |
| 3 | V4L2 camera with DMA-BUF export | ~200 | Zero-copy camera on headless Linux / Pi |
| 4 | X11 screen capture fallback | ~100 | Non-Wayland Linux support |
| 5 | macOS ScreenCaptureKit + AVFoundation | ~150 | Zero-copy on macOS |
| 6 | Windows WGC + MF camera | ~350 | GPU-accelerated on Windows |

Phase 1+2 give full zero-copy screen sharing on Linux Wayland.
Phase 3 gives zero-copy camera on Raspberry Pi without PipeWire.
Phase 5 gives zero-copy everything on macOS.

## Dependencies

| Crate | Version | Downloads | Purpose | Phase |
|-------|---------|-----------|---------|-------|
| `pipewire` | 0.9.2 | 47k/mo | PipeWire stream handling | 1 |
| `ashpd` | 0.13.0 | 240k/mo | xdg-desktop-portal (ScreenCast + Camera) | 1 |
| `v4l2r` | latest | — | V4L2 with DMABUF support | 3 |
| `screencapturekit` | 1.5.0 | 367k total | macOS screen capture | 5 |
| `objc2-av-foundation` | 0.3.2 | 184k total | macOS camera | 5 |
| `windows-capture` | 1.5.0 | 130k total | Windows WGC screen | 6 |
| `windows` | latest | — | Windows MF camera + D3D11 | 6 |

Note: `v4l` crate (simpler) does NOT support DMABUF. Use `v4l2r` for
zero-copy, or raw ioctls via `nix`.

## Risks

- **PipeWire modifier negotiation**: Compositor may not offer modifiers
  compatible with VAAPI encoder import. May need a DRM render-node allocator
  (`gbm`) as intermediary to control modifier selection.
- **Buffer lifetime**: PipeWire buffers must be returned promptly. If encode
  latency exceeds the buffer pool size, frames will be dropped. Need to
  measure and potentially increase the negotiated buffer count.
- **Portal permissions**: Screen capture requires user consent via the
  xdg-desktop-portal dialog on each session. No way to skip this on Wayland.
  Camera portal also requires a one-time permission dialog.
- **Pi 5 no HW encoder**: DMA-BUF still avoids one copy (no RGBA conversion)
  but the encode itself is CPU. Acceptable — Pi 5 has enough CPU for 720p
  openh264.
- **USB camera DMA-BUF**: UVC cameras export DMA-BUF via EXPBUF but buffers
  are in system RAM (not CMA). Downstream zero-copy to VAAPI encoder may
  fail — need runtime fallback to mmap path.
- **`objc2-av-foundation` maturity**: Pre-1.0, some rough edges with ObjC
  delegate patterns. May need manual `objc2` blocks for callbacks.

---

## Research: xcap and nokhwa Zero-Copy Feasibility

### xcap (v0.9.0) — Screen Capture

**Platforms**: Linux (X11 + Wayland), macOS, Windows.

**Underlying APIs**:
| Platform | Screenshot | Recording |
|----------|-----------|-----------|
| Linux X11 | XCB (`XGetImage`) | Frame-by-frame XCB polling |
| Linux Wayland | `libwayshot-xcap` | PipeWire via ScreenCast portal |
| macOS | CoreGraphics `CGWindowListCreateImage` | AVFoundation `AVCaptureSession` |
| Windows | GDI `BitBlt` | DXGI Desktop Duplication (or WGC with feature flag) |

**Zero-copy support: None.**

xcap always copies to CPU RGBA `Vec<u8>`. The `Frame` type is:
```rust
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub raw: Vec<u8>,  // Always RGBA, always CPU
}
```

**Even on Wayland where PipeWire is used internally**, xcap:
1. Connects with `StreamFlags::MAP_BUFFERS` — requests CPU-mapped buffers
2. Negotiates only RGB/RGBA/RGBx/BGRx (no NV12, no YUV)
3. Never sets `SPA_DATA_DmaBuf` in buffer type negotiation
4. Copies every frame via `frame_data.to_vec()` in the process callback
5. Does BGRX→RGBA channel swap for non-RGBA formats (second allocation)

The PipeWire buffer is returned to the pool immediately after copy. No way to
hold a reference or access the underlying DMA-BUF fd.

**On Windows**, DXGI internally acquires `ID3D11Texture2D` on GPU, but
`texture_to_frame()` immediately creates a staging texture with
`D3D11_CPU_ACCESS_READ`, maps it, and copies to `Vec<u8>`.

**On macOS**, AVFoundation delivers `CVPixelBuffer` (IOSurface-backed) but xcap
immediately reads pixel data to `Vec<u8>` with a BGRA→RGBA swap.

**Extensibility**: Cannot extend without forking. `Frame` is opaque `Vec<u8>`,
platform backends are private modules, no traits to implement custom backends.

### nokhwa (v0.10.10) — Camera Capture

**Platforms**: Linux (V4L2), macOS (AVFoundation), Windows (MediaFoundation).

**Zero-copy support: None.**

nokhwa's `Buffer` type is:
```rust
pub struct Buffer {
    resolution: Resolution,
    buffer: Bytes,                 // CPU-side, always
    source_frame_format: FrameFormat,
}
```

- On Linux: V4L2 via `v4l` crate (MMAP only, no DMABUF export)
- On macOS: AVFoundation wraps `CVPixelBuffer` but copies out immediately
- On Windows: MediaFoundation produces CPU buffers

Supported formats: MJPEG, YUYV, NV12, GRAY, RAWRGB, RAWBGR — but the public
API only exposes decoded RGBA via `frame()`. Raw bytes available via
`frame_raw()` but still CPU memory.

Has an `output-wgpu` feature that copies a **decoded** RGBA frame into a wgpu
texture — upload, not zero-copy.

**Extensibility**: Cannot extend without forking. `Buffer` is fixed to `Bytes`,
`CaptureBackendTrait` is sealed, platform backends are private.

### Effort to Add Zero-Copy to xcap/nokhwa

**xcap fork**: Would need to change the PipeWire path to negotiate DMA-BUF
instead of MAP_BUFFERS, add modifier negotiation, expose buffer lifetime
management, change `Frame` to an enum with a GPU variant. Touches ~300 lines
in wayland_video_recorder.rs, plus new public API surface. Similar effort for
DXGI (expose texture instead of staging copy) and macOS (expose CVPixelBuffer).
**Estimate: 500–800 lines of changes across 3 platform backends, plus API
redesign that breaks all downstream users.**

**nokhwa fork**: Would need to add DMABUF to V4L2 path (nokhwa uses the `v4l`
crate which lacks DMABUF — would need to switch to `v4l2r` or raw ioctls),
expose CVPixelBuffer on macOS, add D3D manager on Windows. Even more invasive
than xcap because the core `Buffer` type needs to become an enum.
**Estimate: 800–1200 lines of changes, near-total rewrite of buffer handling.**

### Effort to Build Capture From Scratch (Without xcap/nokhwa)

| Component | Lines | Deps | Notes |
|-----------|-------|------|-------|
| **PipeWire stream** (shared screen+camera) | ~250 | `pipewire`, `libspa` | Format negotiation, DMA-BUF extraction, buffer lifecycle |
| PipeWire ScreenCast portal | ~50 | `ashpd` | D-Bus session setup |
| PipeWire Camera portal | ~30 | `ashpd` | D-Bus access + enumerate |
| **V4L2 DMA-BUF camera** | ~200 | `v4l2r` or `nix` | REQBUFS + EXPBUF + DQBUF loop |
| X11 screen capture (MIT-SHM) | ~100 | `x11rb` | CPU-only fallback |
| **macOS ScreenCaptureKit** | ~40 | `screencapturekit` | Display enum + stream |
| **macOS AVFoundation camera** | ~60 | `objc2-av-foundation` | Session + delegate |
| macOS CVPixelBuffer→GpuFrame | ~50 | `objc2-core-video` | Shared IOSurface extraction |
| **Windows WGC screen** | ~120 | `windows-capture` | Frame pool + texture |
| **Windows MF camera** | ~150 | `windows` | Source reader + D3D manager |
| Windows D3D11→GpuFrame | ~80 | `windows` | Shared texture wrapper |
| **Total** | **~1130** | | |

### Verdict: Build From Scratch

**Forking xcap/nokhwa is not worth it.** Reasons:

1. **Neither crate was designed for zero-copy.** The CPU-copy-to-Vec architecture
   is fundamental to their API contract. Adding GPU paths means redesigning their
   public types, which breaks all other users and creates a maintenance burden.

2. **The from-scratch effort is comparable.** ~1130 lines for all platforms with
   zero-copy vs ~1300–2000 lines to fork+modify two crates. And the from-scratch
   code is purpose-built for our `VideoFrame`/`FrameData`/`NativeFrameHandle` types.

3. **PipeWire unifies screen+camera on Linux.** xcap uses PipeWire internally but
   only for screen capture. nokhwa uses V4L2 directly. Building our own PipeWire
   layer means one code path handles both, with shared DMA-BUF negotiation.

4. **We already have the downstream plumbing.** `FrameData::Gpu`, `GpuFrameInner`,
   `NativeFrameHandle::DmaBuf`, VAAPI DMA-BUF import in the renderer — all landed.
   We just need capture backends that produce the right types.

5. **Fewer dependencies.** xcap pulls in `pipewire` + `xcb` + `zbus` + `image` +
   platform crates. nokhwa pulls in `v4l` + `nokhwa-bindings-*` + `image`. Our
   from-scratch approach uses the same underlying crates (`pipewire`, `ashpd`) but
   skips the intermediate abstractions.

**Recommendation**: Drop xcap and nokhwa. Implement capture backends directly
using platform crates. Start with PipeWire (phase 1) since it covers both
screen and camera on Linux with zero-copy, then V4L2 direct (phase 3) for
headless Pi, then macOS (phase 5) and Windows (phase 6).

Keep xcap/nokhwa as optional fallback features during transition, remove once
native backends are stable.
