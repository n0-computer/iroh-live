# Desktop Rendering

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | draft |
| Applies to | rusty-codecs, moq-media-egui, moq-media-dioxus |
| Platforms | Linux, macOS, Windows |

## Framework independence

iroh-live is not coupled to any GUI framework. The subscribe side delivers decoded `VideoFrame` objects, and your application renders them however it wants. Three rendering paths are available, ranging from GPU-accelerated to pure CPU.

## Rendering paths

### wgpu via `WgpuVideoRenderer`

`WgpuVideoRenderer` in `rusty-codecs` uploads decoded frames to GPU textures and renders them with an NV12 shader. It uses wgpu, which maps to Vulkan on Linux, Metal on macOS, and DX12 on Windows.

On Linux with VAAPI hardware decoding, the renderer can import DMA-BUF file descriptors directly into Vulkan textures, avoiding any CPU-side copy. This zero-copy path activates automatically when the `dmabuf-import` and `vaapi` features are both enabled and the GPU driver supports the frame's DRM modifier. When the modifier is incompatible (common with Intel tiled formats), a VAAPI VPP blit re-tiles the surface before import.

This is the highest-performance path and the one used by the `watch-wgpu` and `watch` examples.

### OpenGL ES 2.0 via `GlesRenderer`

`GlesRenderer` in `rusty-codecs` works anywhere EGL is available: desktop Linux, Raspberry Pi, and embedded ARM boards. It converts frames to RGBA on the CPU and uploads via `glTexImage2D`. The `demos/opengl` crate demonstrates this path with a minimal glutin/winit viewer that has no egui or wgpu dependency.

OpenGL ES 2.0 is a good choice when wgpu is unavailable or when targeting constrained hardware where Vulkan drivers are immature.

### CPU fallback via `frame.rgba_image()`

Every `VideoFrame` exposes an `rgba_image()` method that returns an `RgbaImage` (from the `image` crate). This works regardless of the original pixel format: NV12 and I420 frames are converted to RGBA on the CPU. Any framework that can display RGBA pixel data can use this path.

The CPU conversion is the slowest option, but it has no GPU dependencies and integrates trivially with frameworks like iced, slint, or custom software renderers.

## Integration crates

### moq-media-egui

`moq-media-egui` wraps `WgpuVideoRenderer` as an egui widget called `VideoTrackView`. It handles texture allocation, frame upload, and display within an egui panel. The `watch` example uses this crate.

### moq-media-dioxus

`moq-media-dioxus` provides a dioxus-native video component with a hook-based API. The `moq-media-dioxus/examples/demo` directory contains a working demo app.

Both integration crates are prototype-quality. They work for demos and internal testing but have not been hardened for production use.

## Standalone wgpu + winit

For applications that use wgpu and winit directly without a UI framework, the `watch-wgpu` example in `iroh-live/examples/` demonstrates the full pattern: create a winit window, initialize a wgpu surface, construct a `WgpuVideoRenderer`, subscribe to a broadcast, and render frames in the event loop.

## Standalone OpenGL + glutin

The `demos/opengl` crate is a minimal GLES2 viewer using glutin and winit. It creates an EGL context, constructs a `GlesRenderer`, and renders incoming frames. This serves as a reference for OpenGL-based integrations.

## Future possibilities

The core API (`VideoFrame` with `rgba_image()`) works with any framework that can display pixels. Some integration paths that would be straightforward to build:

- Qt bindings via cxx or qmetaobject, feeding frames to a QQuickPaintedItem or QML VideoOutput.
- GTK4 via gtk4-rs with a custom `gdk::Paintable` backed by `WgpuVideoRenderer` or `rgba_image()`.
- iced, which already uses wgpu internally and could share the GPU device with `WgpuVideoRenderer`.

None of these exist today. Contributions are welcome.
