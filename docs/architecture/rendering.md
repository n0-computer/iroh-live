# Rendering Architecture

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | rusty-codecs, moq-media-egui, moq-media-dioxus |
| Platforms | Linux, macOS, Android, Raspberry Pi |

## Three backends

Three rendering backends exist, each serving different platforms. They share no code and have incompatible APIs. A `FrameRenderer` trait for unification is planned but not yet implemented.

### wgpu (feature: `wgpu`)

`WgpuVideoRenderer` in `rusty-codecs::render` converts a `VideoFrame` to a wgpu RGBA texture. Three input paths handle the different frame backing stores:

- **CPU RGBA**: `queue.write_texture()` uploads packed RGBA data directly. One copy, simplest path.
- **CPU NV12**: uploads Y plane to an R8 texture and UV plane to an RG8 texture, then a WGSL fragment shader converts NV12 to RGBA on the GPU. Two uploads, no intermediate CPU conversion.
- **DMA-BUF zero-copy** (feature: `dmabuf-import`, Linux only): imports the DMA-BUF fd as a Vulkan image via `VK_EXT_image_drm_format_modifier`, GPU-copies NV12 planes to R8/RG8 textures, then runs the same NV12-to-RGBA shader. No CPU involvement after decode. See [linux/dmabuf.md](linux/dmabuf.md) for the full pipeline.
- **Metal zero-copy** (feature: `metal-import`, macOS only): imports CVPixelBuffer via CVMetalTextureCache for zero-copy decode-to-render.

`RenderPath` tracks which path was used for the last frame, exposed for debug overlays.

Used by: `moq-media-egui` (egui texture handle wrapper), `moq-media-dioxus` (vello bridge), `watch-wgpu` example (direct wgpu+winit).

### GLES2 (feature: `gles`)

`GlesRenderer` in `rusty-codecs::render::gles` is a minimal GLES2 renderer using the `glow` abstraction. It supports two upload paths:

- **RGBA**: uploads packed pixels to a `GL_TEXTURE_2D`, draws a fullscreen textured triangle.
- **NV12**: uploads Y plane as `LUMINANCE` and UV plane as `LUMINANCE_ALPHA`, converts to RGBA in a fragment shader (BT.601 limited-range matrix).

Works wherever an EGL/GLES2 context is available. Used by the `pi-zero-demo` crate in both DRM/KMS direct-to-HDMI mode and glutin+winit windowed mode.

### Android EGL external texture

The Android demo imports `AHardwareBuffer` as a `GL_TEXTURE_EXTERNAL_OES` texture via `EGL_ANDROID_image_hardwarebuffer`. The hardware decoder produces `HardwareBuffer` frames directly, so the path from decode to display is true zero-copy.

The EGL import and rendering logic currently lives in Kotlin (`MainActivity.kt`), with thin JNI wrappers for the EGL extension function pointers resolved at runtime via `dlopen`. CPU frames (from the software decoder fallback) go through an intermediate RGBA `AHardwareBuffer` allocation.

## Integration points

| Integration | Renderer | How |
|---|---|---|
| `moq-media-egui` | wgpu | Wraps `WgpuVideoRenderer`, manages egui texture handles |
| `moq-media-dioxus` | wgpu | Wraps `WgpuVideoRenderer`, bridges to vello |
| `watch-wgpu` example | wgpu | Direct wgpu+winit, no UI framework |
| Pi Zero DRM | GLES2 | DRM/KMS + GBM + EGL, direct HDMI output |
| Pi Zero windowed | GLES2 | glutin + winit |
| Android demo | EGL/OES | AHardwareBuffer import, Kotlin GL shader |

## Industry comparison

Every production Vulkan video renderer uses `VK_EXT_image_drm_format_modifier` for DMA-BUF import. The EGL path (`eglCreateImage` with `EGL_LINUX_DMA_BUF_EXT`) is the older alternative, used by GStreamer, Chromium, and SDL.

| Project | DMA-BUF import method | NV12 conversion |
|---------|----------------------|-----------------|
| mpv / libplacebo | Vulkan DRM modifier | Separate R8/RG8 textures + shader |
| GStreamer VA plugin | EGL DMA-BUF | `vkCmdBlitImage` or EGL |
| Chromium | EGL per-plane import | Compositor handles conversion |
| FFmpeg | DRM PRIME descriptors | Downstream consumer decides |
| iroh-live | Vulkan DRM modifier | Separate R8/RG8 textures + WGSL shader |

Our approach matches libplacebo, the most sophisticated open-source Vulkan video renderer. The separate-texture-plus-shader approach is preferred over `VK_KHR_sampler_ycbcr_conversion` because the latter imposes usage constraints and has poor driver support for DMA-BUF-imported images.

## Open areas

**`FrameRenderer` trait**: a shared trait for all three backends would let consumers stay renderer-agnostic. Planned API splits upload and draw into separate steps for timing control.

**Android EGL in Rust**: moving the `AHardwareBuffer` import chain from Kotlin into `GlesRenderer` would decouple rendering from the demo app and enable reuse in Rust-only contexts.

**`SurfaceControl` rendering on Android**: bypasses EGL entirely by handing the `HardwareBuffer` to the system compositor. Eliminates one composition pass and reduces latency.
