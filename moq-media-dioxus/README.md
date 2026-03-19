# moq-media-dioxus

[dioxus-native](https://github.com/DioxusLabs/blitz) integration for moq-media video rendering.

Provides `use_video_renderer`, a dioxus hook that creates a video renderer and returns a `VideoTrackHandle` for setting the active video track.

## Usage

```rust
use moq_media_dioxus::use_video_renderer;

let (handle, paint_source_id) = use_video_renderer();

// Set or swap the video track at any time
handle.set(video_track);

// In RSX
rsx!(canvas { "src": paint_source_id })
```

## Running the demo

The included demo captures from a camera or screen, optionally routes through a full encode/decode pipeline, and renders via wgpu.

```sh
# Camera + screen capture
cargo run -p moq-media-dioxus --example demo --features capture-camera,capture-screen

# Camera only
cargo run -p moq-media-dioxus --example demo --features capture-camera

# With DMA-BUF import (Linux, zero-copy HW decode)
cargo run -p moq-media-dioxus --example demo --features capture-camera,dmabuf-import
```

The UI lets you switch between capture sources and toggle between direct rendering (raw capture frames) and encode/decode mode (full codec round-trip).

## Feature flags

No features are enabled by default.

| Feature | Description |
|---------|-------------|
| `dmabuf-import` | Zero-copy DMA-BUF texture import (Linux/Vulkan) |
| `capture-camera` | Camera capture via rusty-capture |
| `capture-screen` | Screen capture via rusty-capture |
