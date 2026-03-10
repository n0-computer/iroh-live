# moq-media-dioxus

[dioxus-native](https://github.com/DioxusLabs/blitz) integration for moq-media video rendering.

Provides `DioxusVideoRenderer`, a wrapper around `WgpuVideoRenderer` that implements
dioxus-native's `CustomPaintSource` trait for rendering decoded video frames in a
`<canvas>` element.

## Usage

```rust
use moq_media_dioxus::DioxusVideoRenderer;
use dioxus_native::use_wgpu;

let renderer = DioxusVideoRenderer::new();
let frame_tx = renderer.frame_sender();
let paint_source_id = use_wgpu(move || renderer);

// Send decoded frames from any thread:
frame_tx.send(decoded_frame).ok();

// In RSX:
rsx!(canvas { "src": paint_source_id })
```

## Running the viewer example

The viewer example captures from a camera or screen, optionally encodes and decodes
through a full pipeline, and renders via wgpu.

```sh
# Camera + screen capture (default)
cargo run -p moq-media-dioxus --example viewer --features capture-camera,capture-screen

# Camera only
cargo run -p moq-media-dioxus --example viewer --features capture-camera

# Screen only
cargo run -p moq-media-dioxus --example viewer --features capture-screen

# With DMA-BUF import (Linux, for zero-copy HW decode)
cargo run -p moq-media-dioxus --example viewer --features capture-camera,capture-screen,dmabuf-import
```

The viewer UI lets you switch between:

- **Source**: Camera or Screen (depending on enabled features)
- **Mode**: Direct (raw capture → decode → render) or Encode/Decode (capture → encode → decode → render)
- **Encoder/Decoder**: selectable when in Encode/Decode mode

## Features

| Feature | Description |
|---------|-------------|
| `dmabuf-import` | Zero-copy DMA-BUF texture import (Linux/Vulkan) |
| `capture-camera` | Camera capture via nokhwa |
| `capture-screen` | Screen capture via xcap |
