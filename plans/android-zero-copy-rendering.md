# Android zero-copy rendering via ImageReader + HardwareBuffer

## 1. Current state analysis

### How `GpuFrame` works

`GpuFrame` (in `rusty-codecs/src/format.rs`) wraps an `Arc<dyn GpuFrameInner>` trait object. The trait has four operations:

- `download_rgba()` -- readback to CPU RGBA
- `download_nv12()` -- readback to CPU NV12 planes
- `native_handle()` -- returns a platform-specific handle for zero-copy import (currently only `NativeFrameHandle::DmaBuf` on Linux)
- `dimensions()` and `gpu_pixel_format()`

The only `GpuFrameInner` implementation today is `VaapiGpuFrame` in the VAAPI decoder. It holds a reference-counted VAAPI surface and exports DMA-BUF file descriptors on demand through `native_handle()`.

`VideoFrame` unifies all pixel backing stores through `FrameData`:

- `Packed { pixel_format, data }` -- CPU RGBA/BGRA
- `I420 { y, u, v }` -- CPU planar YUV
- `Nv12(Nv12Planes)` -- CPU semi-planar YUV
- `Gpu(GpuFrame)` -- opaque GPU-resident frame

Every consumer that needs pixels calls `VideoFrame::rgba_image()`, which dispatches to the appropriate readback path and caches the result.

### VAAPI-to-wgpu zero-copy path

The full pipeline:

1. VAAPI decoder produces `VaapiGpuFrame` containing the decoded surface.
2. `WgpuVideoRenderer::render()` receives a `VideoFrame` with `FrameData::Gpu`.
3. It calls `gpu.native_handle()` to get a `DmaBufInfo`.
4. `DmaBufImporter` imports the DMA-BUF FD into a Vulkan VkImage via `VK_EXT_image_drm_format_modifier` and `VK_KHR_external_memory_fd`.
5. If the VAAPI surface has a tiling modifier Vulkan cannot handle (e.g. Y_TILED on Intel), a VAAPI VPP blit re-tiles it first.
6. The imported NV12 planes are GPU-copied to R8/RG8 wgpu textures.
7. A fullscreen-triangle shader converts NV12 to RGBA in the output texture.

This path avoids any CPU readback. The key property: the decoded frame always passes through `VideoFrame`, and the renderer inspects `FrameData` to choose the cheapest path.

### Current Android rendering

The `AndroidDecoder` uses MediaCodec in ByteBuffer mode:

1. `dequeue_output_buffer` returns raw NV12 bytes.
2. `nv12_to_rgba_data()` converts to RGBA on the CPU.
3. The RGBA `VideoFrame` is sent to `VideoTrack`.
4. The JNI bridge calls `video.current_frame()`, then copies RGBA bytes into an `ANativeWindow` buffer row by row.

Two CPU-intensive steps: NV12-to-RGBA conversion and the ANativeWindow copy. Both are unnecessary with the ImageReader+HardwareBuffer approach.

## 2. Android zero-copy design: ImageReader + HardwareBuffer

### Approach: keep the `GpuFrame` abstraction

Instead of having MediaCodec render directly to the display Surface (which bypasses `VideoFrame` entirely), we use an intermediate `ImageReader`:

1. Create an `ImageReader` with `AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE`.
2. Pass the `ImageReader`'s Surface to `MediaCodec::configure()`.
3. MediaCodec decodes frames into the `ImageReader`'s buffer queue (GPU memory).
4. `acquire_latest_image()` yields an `Image` backed by a `HardwareBuffer`.
5. Wrap the `HardwareBuffer` in a `GpuFrame` via `NativeFrameHandle::HardwareBuffer`.
6. The `VideoFrame` flows through the normal pipeline: `PlayoutBuffer`, `VideoTrack`, etc.
7. On the JNI boundary, pass the raw `AHardwareBuffer*` pointer to Kotlin.
8. Kotlin renders it to a `SurfaceView` via OpenGL ES import or `SurfaceControl` (API 29+).

### Why this fits `GpuFrame`

The `HardwareBuffer` is the Android equivalent of a DMA-BUF fd — a shareable, GPU-resident buffer handle. It supports all `GpuFrameInner` operations:

- `download_rgba()`: lock the HardwareBuffer for CPU read, convert NV12 to RGBA
- `download_nv12()`: lock and return NV12 planes directly
- `dimensions()`: from ImageReader/MediaCodec format
- `native_handle()`: return `NativeFrameHandle::HardwareBuffer` with the raw pointer

This is the same pattern as VAAPI+DMA-BUF. The decoder produces an opaque GPU frame, the consumer inspects `FrameData::Gpu` and either imports zero-copy or falls back to CPU readback.

### Advantages over direct Surface output mode

- **Preserves `VideoFrame`/`GpuFrame` abstraction.** No special pipeline path.
- **`PlayoutClock`/`PlayoutBuffer` work normally.** Frames are real objects with timestamps.
- **Frame inspection works.** Screenshots, recording, adaptive switching, preview — all possible.
- **Kotlin gets compositing control.** Position video in UI, add overlays, animate.
- **Consistent architecture across Linux (DMA-BUF) and Android (HardwareBuffer).**

### Trade-offs

- **ImageReader adds a buffer queue** (typically 2-3 frames). Adds ~1-2 frames of latency vs direct Surface output. Negligible for video calls.
- **Buffer lifecycle management.** The `HardwareBuffer` must stay alive while Kotlin holds it. Use `AHardwareBuffer_acquire()`/`release()` ref counting across JNI.
- **OpenGL ES import on Kotlin side** requires `EGL_ANDROID_get_native_client_buffer` + `eglCreateImageKHR`. This is standard and widely supported (API 26+).

### Industry comparison

- **FFmpeg**: Always produces `AVFrame` with opaque GPU handles. Never bypasses the frame abstraction.
- **WebRTC Android**: Uses `SurfaceTextureHelper` to get decoded frames as GL textures, then renders via GL to a SurfaceView. Same pattern: frame object with GPU handle.
- **ExoPlayer**: Uses direct Surface mode for dumb playback, but switches to `ImageReader` output when the app needs frame access (effects processing, HDR tone mapping).

Our approach matches FFmpeg/WebRTC: frame-based pipeline with GPU-native handles.

## 3. Implementation: `NativeFrameHandle::HardwareBuffer`

### New types in `rusty-codecs/src/format.rs`

```rust
/// NativeFrameHandle gains an Android variant:
#[non_exhaustive]
pub enum NativeFrameHandle {
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBufInfo),
    #[cfg(target_os = "android")]
    HardwareBuffer(HardwareBufferInfo),
}

#[cfg(target_os = "android")]
pub struct HardwareBufferInfo {
    /// The AHardwareBuffer, reference-counted.
    pub buffer: ndk::hardware_buffer::HardwareBuffer,
    pub width: u32,
    pub height: u32,
    /// NV12 plane layout within the buffer.
    pub y_stride: u32,
    pub uv_offset: u32,
    pub uv_stride: u32,
}
```

### New decoder: `AndroidHwDecoder` in `rusty-codecs/src/codec/android/hw_decoder.rs`

```rust
pub struct AndroidHwDecoder {
    codec: MediaCodec,
    image_reader: ImageReader,
    nal_format: NalFormat,
    decoded_width: u32,
    decoded_height: u32,
    pending_frames: VecDeque<VideoFrame>,
}
```

Key differences from `AndroidDecoder`:
- `MediaCodec::configure()` receives the `ImageReader`'s surface (not `None`)
- `drain_output()` calls `release_output_buffer(buf, true)` (render=true) instead of reading bytes
- After release, `image_reader.acquire_latest_image()` yields the `HardwareBuffer`
- Wraps it in `GpuFrame` → `FrameData::Gpu` → `VideoFrame`
- No NV12-to-RGBA conversion. No CPU pixel access at all.

Implements `VideoDecoder` trait normally — same `push_packet()` / `pop_frame()` contract.

### `AndroidGpuFrame` (GpuFrameInner impl)

```rust
struct AndroidGpuFrame {
    image: Image,  // holds AHardwareBuffer ref
    width: u32,
    height: u32,
}

impl GpuFrameInner for AndroidGpuFrame {
    fn download_rgba(&self) -> Result<RgbaImage> {
        // Lock HardwareBuffer for CPU read, convert NV12 to RGBA
        // (fallback path, not used in the normal zero-copy flow)
    }
    fn gpu_pixel_format(&self) -> GpuPixelFormat { GpuPixelFormat::Nv12 }
    fn dimensions(&self) -> (u32, u32) { (self.width, self.height) }
    fn native_handle(&self) -> Option<NativeFrameHandle> {
        Some(NativeFrameHandle::HardwareBuffer(..))
    }
}
```

### JNI bridge changes (`android-demo/rust/src/lib.rs`)

Replace `renderToSurface` (CPU RGBA row copy) with `nextHardwareBuffer`:

```rust
/// Returns a raw AHardwareBuffer pointer for the latest decoded frame.
/// Kotlin must call releaseHardwareBuffer when done.
/// Returns 0 if no frame is available.
fn Java_..._nextHardwareBuffer(handle: jlong) -> jlong {
    // current_frame() → VideoFrame with FrameData::Gpu
    // Extract NativeFrameHandle::HardwareBuffer
    // AHardwareBuffer_acquire() to prevent premature release
    // Return raw pointer as jlong
}

fn Java_..._releaseHardwareBuffer(buffer_ptr: jlong) {
    // AHardwareBuffer_release()
}
```

Remove: `renderToSurface`, `nextFrame` (ByteBuffer path).

### Kotlin rendering

Two options (in order of preference):

**Option A: OpenGL ES (API 26+)**
1. Create an EGL context + surface attached to the SurfaceView.
2. Import HardwareBuffer as EGLImage: `eglGetNativeClientBufferANDROID` → `eglCreateImageKHR`.
3. Bind as `GL_TEXTURE_EXTERNAL_OES` texture.
4. Render a fullscreen quad with a simple shader.
5. Call `releaseHardwareBuffer` after `eglDestroyImageKHR`.

**Option B: SurfaceControl (API 29+)**
1. `SurfaceControl.Transaction().setBuffer(sc, hardwareBuffer).apply()`
2. Zero-copy compositing by SurfaceFlinger.
3. Simplest code path but higher API requirement.

For Phase 1, use Option A (broadest device support).

## 4. Migration path

### Phase 1: AndroidHwDecoder + JNI bridge + Kotlin GL renderer

**Scope**: Full zero-copy from MediaCodec to display. No CPU pixel handling.

1. **`rusty-codecs/src/format.rs`**: Add `NativeFrameHandle::HardwareBuffer` variant and `HardwareBufferInfo` struct.

2. **`rusty-codecs/src/codec/android/hw_decoder.rs`**: New `AndroidHwDecoder` implementing `VideoDecoder`. Uses `ImageReader` surface for output, produces `FrameData::Gpu(GpuFrame)` frames.

3. **`rusty-codecs/src/codec/android/gpu_frame.rs`**: New `AndroidGpuFrame` implementing `GpuFrameInner`. Wraps `ndk::media::image_reader::Image` (which holds the `HardwareBuffer` ref).

4. **`rusty-codecs/src/codec/dynamic.rs`**: Update `DynamicVideoDecoder` to prefer `AndroidHwDecoder` over `AndroidDecoder` when the `android` feature is enabled.

5. **`android-demo/rust/src/lib.rs`**:
   - Remove `renderToSurface` and `nextFrame` JNI exports.
   - Add `nextHardwareBuffer() -> jlong` and `releaseHardwareBuffer(jlong)`.
   - `connect_impl` uses `broadcast.video()` which picks `AndroidHwDecoder` via dynamic dispatch.
   - `VideoTrack::current_frame()` returns `VideoFrame` with `FrameData::Gpu`.

6. **`android-demo/app/src/main/java/.../IrohBridge.kt`**: Update native method declarations.

7. **`android-demo/app/src/main/java/.../MainActivity.kt`**:
   - Replace render loop: call `nextHardwareBuffer`, import as GL texture, render, release.
   - Add `GLSurfaceView` (or manual EGL setup on `SurfaceView`).
   - Remove ByteBuffer/Bitmap rendering code.

**What does NOT change**: `VideoDecoder` trait, `VideoTrack`, `PlayoutBuffer`, `PlayoutClock`, `GpuFrame` trait, `WgpuVideoRenderer`, any desktop code.

**Estimated effort**: 2-3 days, mostly on-device testing and Kotlin GL boilerplate.

### Phase 2 (future): SurfaceControl rendering

Replace GL import with `SurfaceControl.setBuffer()` on API 29+ for true zero-copy compositing. Fewer moving parts, no GL context needed. Fall back to Phase 1 GL path on older devices.

### Phase 3 (future): Cross-platform NativeFrameHandle

- **macOS IOSurface**: `NativeFrameHandle::IoSurface` for VideoToolbox decoder → wgpu Metal import.
- **Windows D3D11**: `NativeFrameHandle::D3D11Texture` for DXVA2 → wgpu D3D11 import.

Each follows the same pattern: decoder produces `GpuFrame`, renderer imports via platform API.

## Summary of design decisions

**ImageReader + HardwareBuffer preserves the `GpuFrame` abstraction.** The Android zero-copy path follows the same pattern as VAAPI+DMA-BUF on Linux. No pipeline bifurcation, no special VideoTrack variants.

**`NativeFrameHandle::HardwareBuffer` is the Android equivalent of `DmaBuf`.** GPU-resident, shareable, refcounted. Fits `GpuFrameInner` cleanly.

**Direct Surface mode is NOT used.** It bypasses `VideoFrame` entirely, breaking frame inspection, playout timing, and the subscription API. The 1-2 frame latency cost of ImageReader is acceptable for video calls.

**Kotlin renders via OpenGL ES import (Phase 1).** Broadest device support (API 26+). SurfaceControl (API 29+) is a future optimization.

**The existing `AndroidDecoder` (ByteBuffer mode) remains available** as a fallback for devices where ImageReader output doesn't work, or for cases that need CPU pixel access.

---

## TODO: Android MediaCodec encoder Surface input mode

The encoder currently uses ByteBuffer mode: camera NV12 frames are copied into
MediaCodec input buffers on CPU. When the camera resolution differs from the
encoder target (common — CameraX picks the closest sensor crop, e.g. 1280×960
for a 720p request), the frame goes through NV12→RGBA→CPU scale→RGBA→NV12
before reaching the codec. This works but wastes CPU and memory bandwidth.

**Surface input mode** is the standard Android encode path (used by CameraX
VideoCapture, WebRTC, OBS, etc.):

1. `MediaCodec.createInputSurface()` returns a `Surface` backed by a
   `BufferQueue`.
2. Camera (or any producer) writes directly to that Surface — GPU handles
   color conversion and scaling, zero CPU copies.
3. The encoder consumes buffers from the queue with no `dequeueInputBuffer` /
   `queueInputBuffer` calls.

Switching to Surface mode eliminates the per-frame CPU round-trip and lets the
hardware scaler handle resolution mismatches natively. It also unblocks
higher-resolution encode (4K) without proportional CPU cost.

**Blocked on**: the NDK `AMediaCodec` C API does expose `createInputSurface`
(via `AMediaCodec_createInputSurface`), but the `ndk` Rust crate (0.9) does
not wrap it yet. Options: (a) add raw FFI bindings ourselves, (b) upstream to
the `ndk` crate, (c) use JNI to call the Java `MediaCodec` API for Surface
creation while keeping the rest in NDK.

See also: REVIEW.md item RC15.
