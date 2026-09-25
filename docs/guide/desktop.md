# Desktop rendering

A player hands out decoded `moq_video::Frame` values through `VideoFrames`. On
the desktop they are drawn through wgpu.

## The wgpu renderer

`moq_video::render::Renderer::new(device, queue, config)` binds a renderer to a
wgpu device and queue. `render(&frame)` returns a `wgpu::Texture`. You can
present that texture, hand it to a UI toolkit, or copy it back. The renderer
has no windowing dependency and picks no surface format.

Backend selection, colour handling, and zero-copy import are documented in
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video). Two points matter
here.

The renderer fixes the wgpu version. `iroh_live_media::video::render::wgpu`
re-exports the wgpu it links, and a texture from another wgpu major version is
a different type. The workspace pins egui and eframe to versions on the same
wgpu major.

On Linux, request `wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF` when you
create the device. Without it, DMA-BUF frames from PipeWire screen capture are
uploaded through the CPU.

All of this needs the `render` feature. It is on by default in `iroh-live`, and
off in `iroh-live-media`.

## egui

`iroh-live-egui` draws frames in egui.

`VideoView` draws a `VideoFrames` stream, such as a player's `video()` or a
source's `frames()`:

```rust
use iroh_live_egui::VideoView;

let mut view = VideoView::new(ctx, "video", player.video(), render_state);
// in the update loop:
ui.add(view.render());
```

`render_state` is an `Option<&egui_wgpu::RenderState>`. The view wakes the
window when a frame arrives. `render()` draws the newest frame and returns an
`egui::Image`. `set_frames` switches to another stream and keeps the last
picture on screen until the new stream delivers one. `VideoView::new` must run
inside a Tokio runtime.

`FrameView` draws frames you hand it with `render_frame`. `irl publish
--preview` uses it for the camera frames of `VideoSource::frames()`. Create it
with `FrameView::new(ctx, name, render_state)`.

Without a render state, both views show a black placeholder.

`create_egui_wgpu_config()` returns the `egui_wgpu::WgpuConfiguration` to pass
to eframe. On Linux it selects Vulkan and enables
`VULKAN_EXTERNAL_MEMORY_DMA_BUF` when the adapter supports it. eframe does not
request that feature on its own.

`overlay::DebugOverlay` draws the stats panel described in [instrumentation and
tests](../architecture/devtools.md).

## Other toolkits

A toolkit that shares a wgpu device can draw the renderer's texture directly.
Otherwise, `moq_video::Surface::to_rgba` converts a frame's surface to RGBA8
pixels on the CPU, downloading it from the GPU where needed.

No library crate has a GLES renderer. The Pi demo has one in
`demos/pi-zero/src/gles.rs`, see [Raspberry Pi](raspberry-pi.md).
