# moq-media-egui

An [egui](https://github.com/emilk/egui) video widget over
[`moq-media`](../moq-media), plus a debug overlay.

`moq_video::render::Renderer` hands back a `wgpu::Texture` per frame. This crate
registers that texture with egui and draws it.

## Two levels

`VideoTrackView` wraps a decoded `VideoTrack` and polls it. Call `render` in the
draw loop and it takes the newest frame, uploads it, requests a repaint if one
arrived, and returns an `egui::Image` plus the frame's timestamp.

```rust
use moq_media_egui::VideoTrackView;

let mut view = VideoTrackView::new_wgpu(&ctx, "remote", track, Some(render_state));

let (image, timestamp) = view.render(&ctx, available_size);
ui.add(image);
```

`FrameView` is the same upload machinery without a track, for frames that come
from somewhere else. `irl publish --preview` uses it to draw raw camera frames.

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
`Time`. Clicking opens a detail panel with values, threshold colours, and
sparklines. The `Time` category also draws a ten-second timeline of frame
arrivals, A/V offset, buffer depth, and round-trip time.

## Feature flags

`wgpu-render` is the only one, and it is on by default. `wgpu` is deliberately
not a direct dependency: every `wgpu` type this crate names comes from
`moq_media::video::render::wgpu`, the exact build the renderer links, so a
texture it hands back can never be a different `wgpu` major than the one this
crate draws with.
