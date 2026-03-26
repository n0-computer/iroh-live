# Desktop Rendering

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | rusty-codecs, moq-media-egui, moq-media-dioxus |
| Platforms | Linux, macOS, Windows |

iroh-live is not coupled to any GUI framework. The subscribe side delivers decoded `VideoFrame` objects, and your application renders them however it wants. Three rendering paths are available, and two integration crates provide ready-made widgets for egui and dioxus.

## Rendering paths

### wgpu via `WgpuVideoRenderer`

`WgpuVideoRenderer` in `rusty-codecs` uploads decoded frames to GPU textures and converts YUV to RGB in a shader. It uses wgpu, which maps to Vulkan on Linux, Metal on macOS, and DX12 on Windows. This is the recommended path for desktop applications.

On Linux with VAAPI hardware decoding, the renderer can import DMA-BUF file descriptors directly into Vulkan textures without any CPU copy. This zero-copy path activates automatically when the `dmabuf-import` and `vaapi` features are both enabled and the GPU driver supports the frame's DRM modifier. On macOS, Metal texture import from VideoToolbox is available via the `metal-import` feature. See the [DMA-BUF architecture page](../architecture/linux/dmabuf.md) for details on the Linux zero-copy pipeline.

The `watch-wgpu` example and the `irl play` CLI command use this path.

### OpenGL ES 2.0 via `GlesRenderer`

`GlesRenderer` in `rusty-codecs` works anywhere EGL is available: desktop Linux, Raspberry Pi, and embedded ARM boards. It uploads RGBA to a GL texture and draws a fullscreen triangle. The `demos/opengl` crate demonstrates this with a minimal glutin/winit viewer that has no egui or wgpu dependency.

This path is primarily useful on constrained hardware where Vulkan drivers are immature or unavailable (Pi Zero, older embedded boards). On desktop systems with modern GPUs, the wgpu path is faster.

### CPU fallback via `frame.rgba_image()`

Every `VideoFrame` exposes an `rgba_image()` method that returns an `RgbaImage` (from the `image` crate). NV12 and I420 frames are converted to RGBA on the CPU. This is the slowest option but has no GPU dependencies. It serves as a fallback for headless pipelines, testing, or environments where neither wgpu nor OpenGL is practical.

## Integration crates

### moq-media-egui

`moq-media-egui` wraps `WgpuVideoRenderer` as an egui widget called `VideoTrackView`. It handles texture allocation, frame upload, and display within an egui panel. The `irl play` CLI command and the `split` example use this crate. It also provides a `DebugOverlay` for rendering network and codec stats.

### moq-media-dioxus

`moq-media-dioxus` provides a dioxus-native video component with a hook-based API. The `moq-media-dioxus/examples/demo` directory contains a working demo.

Both integration crates are prototype-quality. They work for demos and internal testing but have not been hardened for production use.

## Standalone rendering

For applications that use wgpu and winit directly, the `watch-wgpu` example demonstrates the full pattern: create a winit window, initialize a wgpu surface, construct a `WgpuVideoRenderer`, subscribe to a broadcast, and render frames in the event loop.

For OpenGL, the `demos/opengl` crate is a minimal GLES2 viewer using glutin and winit. It creates an EGL context, constructs a `GlesRenderer`, and renders incoming frames.

The `watch-wgpu` example works on Linux and macOS. Windows rendering via wgpu is expected to work but has not been tested.

## Other frameworks

The core API (`VideoFrame` with `rgba_image()` for CPU access, or `WgpuVideoRenderer` for GPU rendering) integrates with any framework that can display pixels or share a wgpu device. Frameworks like iced (which uses wgpu internally) could share the GPU device directly. Qt and GTK4 integrations are possible through their respective Rust bindings but do not exist today.
