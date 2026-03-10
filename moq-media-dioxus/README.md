# moq-media-dioxus

[dioxus-native](https://github.com/DioxusLabs/blitz) integration for moq-media video rendering.

Provides `use_video_renderer`, a dioxus hook that creates a `DioxusVideoRenderer`
and returns a `VideoTrackHandle` for setting the active video track.

## Usage

```rust
use moq_media_dioxus::use_video_renderer;

let (handle, paint_source_id) = use_video_renderer();

// Set or swap the video track at any time:
handle.set(watch_track);

// In RSX:
rsx!(canvas { "src": paint_source_id })
```

## Running the demo

The demo captures from a camera or screen, optionally encodes and decodes
through a full pipeline, and renders via wgpu.

```sh
# Camera + screen capture
cargo run -p moq-media-dioxus --example demo --features capture-camera,capture-screen

# Camera only
cargo run -p moq-media-dioxus --example demo --features capture-camera

# Screen only
cargo run -p moq-media-dioxus --example demo --features capture-screen

# With DMA-BUF import (Linux, for zero-copy HW decode)
cargo run -p moq-media-dioxus --example demo --features capture-camera,capture-screen,dmabuf-import
```

The UI lets you switch between:

- **Source**: Camera or Screen (depending on enabled features)
- **Mode**: Direct (raw capture → render) or Encode/Decode (capture → encode → decode → render)
- **Encoder/Decoder**: selectable when in Encode/Decode mode

## Features

| Feature | Description |
|---------|-------------|
| `dmabuf-import` | Zero-copy DMA-BUF texture import (Linux/Vulkan) |
| `capture-camera` | Camera capture via nokhwa |
| `capture-screen` | Screen capture via xcap |
