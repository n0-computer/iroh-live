# Rendering: current state, problems, and streamlining proposal

## Current rendering backends

We have three distinct rendering paths, each grown to serve a specific
platform. They share no code and have incompatible APIs.

### 1. wgpu (`rusty-codecs::render::WgpuVideoRenderer`)

The primary desktop renderer. Renders `VideoFrame` to a wgpu RGBA
texture, consumed by egui (via `moq-media-egui`) or raw wgpu+winit
(via the `watch-wgpu` example). Supports three input paths:

- **CPU packed (RGBA/BGRA)**: `queue.write_texture()` — one copy.
- **CPU NV12**: upload Y and UV planes to separate textures, convert
  via a WGSL shader — two uploads, zero intermediate copies.
- **DMA-BUF GPU frames**: Vulkan import via `VK_EXT_image_drm_format_modifier`
  + VPP retiler for incompatible modifiers — true zero-copy on
  compatible hardware.

Used by: `watch.rs`, `watch-wgpu.rs`, `split.rs`, `viewer.rs`
(all desktop examples), `moq-media-dioxus`.

Feature gate: `rusty-codecs/wgpu`.

### 2. GLES2 (`rusty-codecs::render::gles::GlesRenderer`)

A minimal GLES2 renderer: uploads RGBA to a `GL_TEXTURE_2D`, draws a
fullscreen textured triangle. Uses `glow` for the GL abstraction.
Platform-agnostic — works wherever an EGL/GLES2 context is available.

Used by: `pi-zero-demo` (DRM/KMS direct-to-HDMI and glutin+winit
windowed modes).

Feature gate: `rusty-codecs/gles`.

### 3. Android EGL external texture (`moq-media-android::egl` + Kotlin)

Android-specific. Imports `AHardwareBuffer` as an EGL external OES
texture, rendered by a Kotlin GLES2 shader. The HW decoder produces
`AHardwareBuffer` frames directly — true zero-copy from decode to
display. CPU frames (direct mode, SW decoder) go through an
intermediate RGBA `AHardwareBuffer` allocation.

Used by: `demos/android`.

No feature gate — Android-only by `cfg(target_os = "android")`.

## Integration points

| Integration | Renderer | Notes |
|---|---|---|
| `moq-media-egui` | wgpu | Wraps `WgpuVideoRenderer`, manages egui texture handles |
| `moq-media-dioxus` | wgpu | Wraps `WgpuVideoRenderer`, bridges to vello |
| `watch-wgpu` example | wgpu | Direct wgpu+winit, no egui |
| pi-zero DRM | GLES2 | DRM/KMS + GBM + EGL, direct HDMI output |
| pi-zero windowed | GLES2 | glutin + winit |
| Android | EGL/OES | AHardwareBuffer import, Kotlin GL code |

## Problems

### No unified API

Each renderer has its own interface. Consumers must know which backend
they're targeting. Adding a new example or integration means choosing
a renderer and coupling to its API.

### GLES2 only renders RGBA

`GlesRenderer` calls `frame.rgba_image()`, which forces a CPU
NV12-to-RGBA conversion for every non-RGBA frame. On the Pi Zero this
is the bottleneck — a 720p NV12-to-RGBA conversion takes several
milliseconds on a Cortex-A53.

The wgpu renderer avoids this by uploading NV12 planes directly and
converting in a shader. The GLES2 renderer should do the same.

### Android renderer lives in Kotlin

The AHardwareBuffer-to-EGL import path is implemented in Kotlin
(`MainActivity.kt`) with thin JNI wrappers for the EGL extension
functions. This makes it impossible to reuse from Rust-only contexts
and tightly couples the rendering to the demo app.

### No shared frame upload abstraction

`VideoFrame` has multiple backing stores (`Packed`, `Nv12`, `I420`,
`Gpu`). Each renderer re-implements the logic for handling each
variant. A shared `FrameUploader` trait (or similar) would centralize
this.

## Proposal

### Phase 1: NV12 shader for GLES2

Add an NV12-to-RGBA fragment shader to `GlesRenderer`, matching the
approach in the wgpu renderer. Upload Y plane to an R8 texture and UV
plane to an RG8 texture, convert in the shader. This eliminates the
CPU conversion bottleneck on low-power hardware.

The RGBA upload path remains as a fallback for packed frames.

Effort: small. Shader is ~10 lines of GLSL; upload logic mirrors the
wgpu NV12 path.

### Phase 2: `FrameRenderer` trait

Define a trait that all three backends implement:

```rust
pub trait FrameRenderer {
    /// Uploads a frame to the GPU. Returns true if the frame was new.
    ///
    /// # Safety
    /// The GPU context must be current on the calling thread.
    unsafe fn upload(&mut self, frame: &VideoFrame);

    /// Draws the uploaded frame to the current viewport.
    ///
    /// # Safety
    /// The GPU context must be current on the calling thread.
    unsafe fn draw(&self, viewport_w: u32, viewport_h: u32);
}
```

`GlesRenderer` and `WgpuVideoRenderer` both implement it.
`WgpuVideoRenderer` already has `render_frame(&VideoFrame)` which
combines upload + draw; splitting into two steps lets callers control
timing (e.g., upload on one thread, draw on another).

The Android path is harder to fit because the EGL import happens in
Kotlin. Phase 2 defers Android integration — the trait exists in Rust,
and Android continues using its current Kotlin path.

Effort: small. Trait definition + impl wrappers on existing methods.

### Phase 3: move Android EGL import to Rust

Move the `AHardwareBuffer → EGLImage → GL_TEXTURE_EXTERNAL_OES`
import chain from Kotlin into `GlesRenderer` (or a new
`GlesExternalRenderer`). The EGL extension functions are already
resolved in `moq_media_android::egl`; the rendering logic is
straightforward GLES2.

This would let the Android demo's render loop call a single Rust
function instead of orchestrating GL calls from Kotlin. It also opens
the door to reusing the Android renderer in other Rust contexts
(e.g., a headless test harness).

Effort: medium. Need to handle the EGL context lifecycle from Rust
(currently managed by Kotlin) and ensure thread-safety of the GL
context.

### Phase 4: OpenGL as a viewer/watch mode (assess feasibility)

Adding a `--renderer gles` flag to the `watch-wgpu` example is
feasible — the example already uses winit, and glutin provides the
EGL/GLES2 context. The pi-zero demo's windowed mode does exactly this.

For the egui-based examples (`watch.rs`, `split.rs`, `viewer.rs`),
GLES2 is not a direct fit because egui expects either a software
painter or wgpu. egui does have a `glow` backend
(`egui_glow`), which could work but adds another integration surface.

Recommendation: add GLES2 as a mode in `watch-wgpu` only. The egui
examples continue using wgpu — it's the better fit for their UI needs.

Effort: small for watch-wgpu (glutin context + GlesRenderer,
following the pi-zero windowed pattern). Medium for egui integration
if we pursue it later.

## When is GLES2 faster than wgpu?

On desktop (x86, recent Mesa), wgpu/Vulkan is faster — the driver
overhead is lower and the DMA-BUF import path avoids copies entirely.
GLES2 adds no benefit there.

On constrained hardware — Raspberry Pi (vc4/v3d), older Android
devices, embedded Linux with Mali/PowerVR — GLES2 drivers are more
mature and lower-overhead than Vulkan. The Pi Zero's vc4 driver has
no Vulkan support at all; GLES2 is the only GPU path.

On Android, the EGL external texture path (OES) is the standard
zero-copy decode-to-display path. It uses GLES2 under the hood.
wgpu on Android goes through Vulkan, which can import
`AHardwareBuffer` via `VK_ANDROID_external_memory_android_hardware_buffer`
but adds driver overhead compared to the native EGL path.

Rule of thumb: use wgpu on desktop, GLES2 on embedded/Pi, and EGL/OES
on Android. The `FrameRenderer` trait (Phase 2) lets consumers stay
agnostic.
