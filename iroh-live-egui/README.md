# iroh-live-egui

An [egui](https://github.com/emilk/egui) video widget over
[`iroh-live-media`](../iroh-live-media), plus a debug overlay.

`moq_video::render::Renderer` hands back a `wgpu::Texture` per frame. This crate
registers that texture with egui and draws it.

## Two levels

`VideoView` reads a `VideoFrames` stream, the one frame type every source of
pictures hands out: a `Player`'s decoded video, a `VideoSource`'s own frames for
a local preview, or a scanner's camera. Call `render` in the draw loop and it
takes the newest frame, uploads it, and returns an `egui::Image` plus the
frame's timestamp. The stream wakes the window when a frame arrives, so nothing
has to poll.

```rust
use iroh_live_egui::VideoView;

let mut view = VideoView::new(&ctx, "remote", player.video(), Some(&render_state));

let (image, timestamp) = view.render(available_size);
ui.add(image);
```

`FrameView` is the same upload machinery without a stream, for a caller that
has a frame in hand. `irl publish --preview` uses it to draw the camera's own
frames.

Both need a wgpu render state. A view built without one logs a warning and draws
a placeholder, because upstream exposes pixels only through the wgpu pipeline.

## Handing eframe a device

`create_egui_wgpu_config()` builds the `egui_wgpu::WgpuConfiguration` to pass
eframe. On Linux it selects the Vulkan backend and requests
`VULKAN_EXTERNAL_MEMORY_DMA_BUF` when the adapter advertises it, which turns on
zero-copy DMA-BUF import for PipeWire screen capture. eframe would not otherwise
ask for it. Elsewhere it returns the default.

## Debug overlay

`overlay::DebugOverlay` draws a translucent bar along the bottom of a video tile
with one clickable section per `StatCategory`: `Net`, `Capture`, `Render`, and
`Audio`. `show_playback` draws a `Player`'s `PlaybackStats` and `PlayerStatus`,
and `show_publish` a `LocalBroadcast`'s `PublishStats` and `PublishStatus`.
Clicking a section opens a detail panel with values and sparklines, whose
history the overlay keeps itself, bounded to the last few seconds.

## Feature flags

`wgpu-render` is the only one, and it is on by default. `wgpu` is deliberately
not a direct dependency: every `wgpu` type this crate names comes from
`iroh_live_media::video::render::wgpu`, the exact build the renderer links, so a
texture it hands back can never be a different `wgpu` major than the one this
crate draws with.
