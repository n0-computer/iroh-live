# moq-ext-egui

[egui](https://github.com/emilk/egui) video views for
[`moq-ext-player`](../moq-ext-player), plus a debug overlay.

## Video views

`VideoView` draws a `VideoFrames` stream, such as a `Player`'s video. It wakes
the window when a frame arrives. Call `render` in the update loop to draw the
newest frame and get an `egui::Image`:

```rust
use moq_ext_egui::VideoView;

let mut view = VideoView::new(&ctx, "remote", player.video_frames(), Some(&render_state));

ui.add(view.render());
```

`FrameView` is the lower level. It draws the frames you hand it with
`render_frame`, and shows a black placeholder before the first one.

Both need a `wgpu` render state. Without one, a view only shows the
placeholder.

## wgpu setup

`egui_wgpu_config()` returns an `egui_wgpu::WgpuConfiguration` to pass
to eframe. On Linux it builds a Vulkan device with DMA-BUF import when the
adapter supports it, so screen capture frames reach the GPU without a CPU
copy. On every platform the device requests only the adapter's own limits.
egui's defaults are too high for some drivers, such as the Raspberry Pi 4.

## Debug overlay

`overlay::DebugOverlay` draws a translucent bar along the bottom of a video,
with one clickable section per `StatCategory`. `show_player` shows a
`Player`'s stats and timeline, and `show_broadcast` shows a `LocalBroadcast`'s
stats. Clicking a section opens a detail panel with sparklines.

## wgpu

The crate has no direct `wgpu` dependency. Use the re-exported
`moq_ext_egui::wgpu` so your `wgpu` types match the renderer's.
