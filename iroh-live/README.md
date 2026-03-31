# iroh-live

High-level API for live audio/video sessions over [iroh](https://github.com/n0-computer/iroh).

`iroh-live` ties together the MoQ transport, media pipelines, and connection management into three progressively higher-level abstractions:

- **`Live`** is the entry point. It creates an iroh endpoint, manages sessions, and publishes and subscribes to broadcasts.
- **`Call`** provides 1:1 call convenience, handling the handshake of publishing your own media and subscribing to the remote peer's.
- **`rooms::Room`** coordinates multi-party rooms with gossip-based participant discovery. Each participant publishes into the room and subscribes to everyone else.

## Usage

Publish a camera broadcast:

```rust
use iroh::Endpoint;
use iroh_live::Live;
use iroh_live::media::publish::LocalBroadcast;

let endpoint = Endpoint::builder().discovery_n0().bind().await?;
let live = Live::builder(endpoint).spawn_with_router();

let broadcast = LocalBroadcast::new();
// configure video/audio sources on the broadcast...
live.publish("my-stream", &broadcast).await?;
```

Subscribe to a remote broadcast:

```rust
let (_session, remote) = live.subscribe(remote_addr, "my-stream").await?;
let video = remote.video()?;
// video.try_recv() returns the latest decoded VideoFrame
```

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `h264` | yes | H.264 codec via openh264 |
| `opus` | yes | Opus audio codec |
| `av1` | yes | AV1 codec via rav1e/rav1d |
| `capture` | yes | Camera and screen capture |
| `vaapi` | | Linux VA-API hardware codecs |
| `v4l2` | | V4L2 hardware codecs (Raspberry Pi) |
| `videotoolbox` | | macOS VideoToolbox hardware codecs |
| `wgpu` | | GPU-accelerated rendering |
| `dmabuf-import` | | Zero-copy DMA-BUF texture import (Linux/Vulkan) |
| `pcm` | | Raw PCM audio (no encoding) |
| `jack` | | JACK audio backend via cpal |
| `android` | | Android MediaCodec + CameraX support |

## Examples

See the [examples directory](examples/) and the [main README](../README.md) for a walkthrough.
