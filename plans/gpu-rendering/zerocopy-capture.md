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

### Linux (PipeWire + DMA-BUF)

PipeWire is the standard screen capture API on modern Linux (Wayland).
It can negotiate DMA-BUF buffers directly with the compositor.

```
PipeWire ─► DMA-BUF fd (NV12/BGRA) ─► VideoFrame { data: FrameData::Gpu(..) }
                                         native_handle: NativeFrameHandle::DmaBuf(..)
```

**Steps:**

1. Add `pipewire` feature to `rusty-codecs` (or `moq-media`).
2. Implement `PipeWireCapture` using `pipewire-rs` crate:
   - Request screen portal via `xdg-desktop-portal` (D-Bus).
   - Negotiate `SPA_DATA_DmaBuf` buffer type in the PipeWire stream.
   - On each frame callback, extract fd + modifier + offset/stride → `DmaBufInfo`.
   - Wrap in `VideoFrame::new_gpu(GpuFrame, w, h, ts)` with a `PipeWireGpuFrame`
     that implements `GpuFrameInner`.
3. `PipeWireGpuFrame` holds a ref to the PipeWire buffer; `Drop` releases it
   back to PipeWire so the compositor can reuse it.
4. The VAAPI encoder can import the DMA-BUF directly via `vaCreateSurfaces`
   with `VASurfaceAttribExternalBuffers` — no copy.

**Encoder zero-copy path (VAAPI):**

```rust
// In vaapi/encoder.rs push_frame():
match &frame.data {
    FrameData::Gpu(gpu) => {
        if let Some(NativeFrameHandle::DmaBuf(info)) = gpu.native_handle() {
            // Import DMA-BUF as VA surface, encode directly
            let surface = va_create_surface_from_dmabuf(info)?;
            self.encode_surface(surface)?;
            return Ok(());
        }
    }
    // ... existing fallback paths
}
```

### Linux (V4L2 Camera)

V4L2 cameras can export DMA-BUF fds via `VIDIOC_EXPBUF`.

1. Implement `V4l2Capture` using `v4l` or raw ioctls.
2. Request `V4L2_MEMORY_MMAP` + export via `VIDIOC_EXPBUF` → DMA-BUF fd.
3. Camera typically outputs NV12 or YUYV; wrap as `FrameData::Gpu` with
   `NativeFrameHandle::DmaBuf`.

### macOS (ScreenCaptureKit / AVFoundation)

ScreenCaptureKit (macOS 12.3+) provides `CMSampleBuffer` backed by
`CVPixelBuffer` which wraps an `IOSurface`.

```
ScreenCaptureKit ─► CVPixelBuffer (IOSurface-backed, NV12/BGRA)
                    ─► VideoFrame { data: FrameData::Gpu(..) }
                        native_handle: NativeFrameHandle::IoSurface(..)
```

**Steps:**

1. Add `NativeFrameHandle::IoSurface(IoSurfaceRef)` variant (behind `#[cfg(target_os = "macos")]`).
2. Implement `ScreenCaptureKitSource` using `screencapturekit-rs` or `objc2` bindings.
3. VideoToolbox encoder accepts `CVPixelBuffer` directly — true zero-copy.

### Camera on macOS

AVFoundation's `AVCaptureVideoDataOutput` already provides `CVPixelBuffer`.
Same `IoSurface` path applies.

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

## Implementation Order

| Phase | Work | Benefit |
|-------|------|---------|
| 1 | PipeWire screen capture with DMA-BUF negotiation | Zero-copy screen→encode on Linux Wayland |
| 2 | VAAPI encoder DMA-BUF import (skip CPU upload) | Completes the zero-copy loop |
| 3 | V4L2 camera with DMA-BUF export | Zero-copy camera→encode on Linux |
| 4 | macOS ScreenCaptureKit + IoSurface | Zero-copy screen→encode on macOS |
| 5 | macOS AVFoundation camera | Zero-copy camera→encode on macOS |

Phase 1+2 together give full zero-copy screen sharing on Linux:
`compositor → DMA-BUF → VAAPI encode → network`, no CPU pixel touching.

## Dependencies

- `pipewire-rs` — PipeWire client bindings (Rust)
- `ashpd` — xdg-desktop-portal D-Bus helpers (screen capture portal)
- `v4l` — V4L2 bindings (optional, phase 3)
- `screencapturekit-rs` or `objc2-screen-capture-kit` (phase 4)

## Risks

- **PipeWire modifier negotiation**: Compositor may not offer modifiers
  compatible with VAAPI encoder import. May need a DRM render-node allocator
  (`gbm`) as intermediary to control modifier selection.
- **Buffer lifetime**: PipeWire buffers must be returned promptly. If encode
  latency exceeds the buffer pool size, frames will be dropped. Need to
  measure and potentially increase the negotiated buffer count.
- **Portal permissions**: Screen capture requires user consent via the
  xdg-desktop-portal dialog on each session. No way to skip this on Wayland.
