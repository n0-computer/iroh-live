# moq-media-egui

[egui](https://github.com/emilk/egui) video rendering widget for moq-media.

Provides two levels of integration:

- **`FrameView`** is the low-level frame renderer. Call `render_frame(&video_frame)` to upload a frame, then call `image()` to get an `egui::Image` for your UI.
- **`VideoTrackView`** is the high-level track renderer. It wraps a decoded `VideoTrack`, handles frame polling and viewport sizing, and returns an `egui::Image` ready to display.

Both support CPU-only texture upload and optional wgpu-accelerated rendering with zero-copy DMA-BUF import on Linux.

## Usage

```rust
use moq_media_egui::VideoTrackView;

// Create a view from a decoded video track
let mut view = VideoTrackView::new(&ctx, "remote-video", track);

// In your egui draw loop
let (image, timestamp) = view.render(&ctx, available_size);
ui.add(image);
```

For wgpu acceleration (better performance, zero-copy on supported hardware):

```rust
let render_state = frame.wgpu_render_state().unwrap();
let mut view = VideoTrackView::new_wgpu(&ctx, "remote-video", track, render_state);
```

## Debug overlay

The `overlay` module provides `DebugOverlay`, a collapsible stats panel showing network, capture, render, and timing metrics as colored bars.

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `wgpu-render` | yes | GPU-accelerated rendering via wgpu |
| `metal-import` | yes | Metal texture import on macOS |
| `dmabuf-import` | | Zero-copy DMA-BUF texture import on Linux/Vulkan |

## Helper

`create_egui_wgpu_config()` returns a wgpu backend configuration optimized for video. On Linux with `dmabuf-import`, it requests the Vulkan instance extensions needed for DMA-BUF import.
