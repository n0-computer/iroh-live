# Desktop rendering

The subscribe side hands you `moq_video::Frame` values and does not care what you
do with them. There is one rendering path on the desktop, and it goes through
wgpu.

## The wgpu renderer

`moq_video::render::Renderer` takes a `wgpu::Device` and `Queue` and returns a
`wgpu::Texture` per frame. That texture is the whole integration point: present
it, hand it to a UI toolkit, or copy it back. The renderer carries no windowing
dependency and picks no surface format.

Backend selection, colour handling, and the zero-copy import matrix are
documented upstream in [the moq-video
page](https://doc.moq.dev/lib/rs/crate/moq-video). Two points matter when you
wire it up here.

The wgpu version is fixed by the renderer. `moq_video::render` re-exports the
exact build it links, reachable as `moq_media::video::render::wgpu`, and a
texture from a different wgpu major is a different type. This is why the
workspace pins egui and eframe to versions that sit on the same wgpu major.

On Linux, request `wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF` when you
create the device, or every DMA-BUF frame from PipeWire screen capture takes the
CPU upload path instead.

Enable the `render` feature to get any of this. It is on by default in
`iroh-live` and `iroh-live-cli`, and off in `moq-media`, since a build that never
draws should not pull a graphics stack.

## egui

`moq-media-egui` is the ready-made integration. Two types matter.

`VideoTrackView` wraps a `VideoTrack` and polls it. Call `render(ctx, size)` in
your draw loop and it takes the newest frame, uploads it, requests a repaint if
something arrived, and returns an `egui::Image` plus the frame's timestamp.

`FrameView` is the same upload machinery without the track, for an application
that gets frames from somewhere else. `irl publish --preview` uses it to draw the
local preview, which is raw camera frames rather than a decoded track.

Both need a wgpu render state. Construct them with `new_wgpu(ctx, name,
Some(render_state))`; a view built without one logs a warning and draws a
placeholder, because upstream only exposes pixels through the wgpu pipeline.

`create_egui_wgpu_config()` builds the `egui_wgpu::WgpuConfiguration` to hand
eframe. On Linux it selects the Vulkan backend and enables
`VULKAN_EXTERNAL_MEMORY_DMA_BUF` when the adapter advertises it, which eframe
would not otherwise request. Elsewhere it returns the default.

`overlay::DebugOverlay` draws the stats panel described in [instrumentation and
tests](../architecture/devtools.md).

## Other toolkits

Anything that can share a wgpu device can draw the renderer's texture directly.
Anything that cannot needs pixels, and `moq_video::Surface::into_rgba()` is the
exit: it downloads a native surface as needed, honours its colour metadata, and
returns an owned RGBA8 image.

There is no GLES renderer in a library crate. `demos/pi-zero/src/gles.rs` is one,
but it lives in the demo because it had exactly one caller and moq's `render`
module is wgpu-only. See [Raspberry Pi](raspberry-pi.md).

The dioxus integration crate was removed. It had no users and it wrapped a
renderer that no longer exists.
