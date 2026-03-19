# moq-media

Media capture, encoding, decoding, and playout pipelines for real-time streaming. Works with any transport that implements the `PacketSource` and `PacketSink` traits, and has no dependency on iroh or any particular networking stack.

## What it handles

- **Capture** -- camera and screen input via [`rusty-capture`](../rusty-capture) (PipeWire, V4L2, AVFoundation, CameraX)
- **Encoding** -- video and audio encoder pipelines running on dedicated OS threads
- **Decoding** -- decoder pipelines with frame output channels
- **Playout** -- `PlayoutClock` maps presentation timestamps to wall-clock time, measures jitter, and supports frame skipping for live streams
- **Adaptive bitrate** -- `AdaptiveVideoTrack` switches between renditions based on network conditions (bandwidth, loss rate, RTT)
- **Audio** -- microphone input and speaker output via cpal/firewheel, with acoustic echo cancellation

## Publish side

`LocalBroadcast` manages the full encode-and-publish pipeline. You create video and audio publishers, set capture sources, and the broadcast handles encoding and packetization.

```rust
use moq_media::publish::LocalBroadcast;

let broadcast = LocalBroadcast::new();
let video = broadcast.video_publisher();
video.set(camera_source);
// broadcast.producer() feeds into the transport layer
```

`VideoRenditions` and `AudioRenditions` support simulcast, encoding multiple quality layers from a single source.

## Subscribe side

`RemoteBroadcast` wraps a transport consumer, watches the catalog for available tracks, and spawns decoder pipelines.

```rust
use moq_media::subscribe::RemoteBroadcast;

let remote = RemoteBroadcast::new("my-stream", consumer).await?;
let video = remote.video()?;
// video.current_frame() returns the latest decoded VideoFrame
```

`AdaptiveVideoTrack` wraps a `VideoTrack` and switches renditions automatically based on `NetworkSignals` (bandwidth, loss, RTT). The switching uses WebRTC-aligned thresholds: below 2% loss is good, above 10% triggers a downgrade, and above 20% drops to the lowest rendition.

## Capture architecture

Capture is handled by [`rusty-capture`](../rusty-capture), which provides platform-specific backends behind a common `VideoSource` trait. Each backend runs its capture loop on a dedicated OS thread and delivers frames through a channel. The `pop_frame()` interface always drains to the latest frame, so the encoder never falls behind.

On Linux, PipeWire captures deliver DMA-BUF handles that can be passed directly to a hardware encoder without any CPU-side copy. V4L2 camera capture uses kernel MMAP buffers. On macOS, ScreenCaptureKit and AVFoundation provide native capture with IOSurface backing. On Android, CameraX delivers NV12 frames through JNI.

## Rendering architecture

Rendering is handled by [`rusty-codecs`](../rusty-codecs), which provides GPU-accelerated and software rendering paths. The `WgpuVideoRenderer` converts decoded frames to display-ready textures via wgpu, which runs on Vulkan (Linux), Metal (macOS), or DX12 (Windows). A `GlesRenderer` is also available for OpenGL ES 2.0 contexts, and CPU-based RGBA conversion serves as a software fallback on all platforms.

Zero-copy rendering is available on most platforms. On Linux, decoded frames from VA-API can be imported as DMA-BUF textures without touching the CPU. On Android, `AHardwareBuffer` frames from MediaCodec are imported via EGL. On macOS, Metal texture import from VideoToolbox is supported.

## Playout

`PlayoutClock` synchronizes audio and video by mapping presentation timestamps to wall-clock time. Two modes are available:

- **`PlayoutMode::Live`** -- real-time with configurable buffer depth and maximum latency. Skips frames that fall behind.
- **`PlayoutMode::Reliable`** -- every frame in order, no latency target. Suitable for recordings and tests.

`PlayoutBuffer` sits after the decoder and gates frame release on the playout clock.

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `h264` | yes | H.264 via openh264 |
| `opus` | yes | Opus audio |
| `av1` | yes | AV1 via rav1e/rav1d |
| `capture` | yes | Camera + screen capture |
| `videotoolbox` | yes | macOS VideoToolbox hardware codecs |
| `metal-import` | yes | Metal texture import on macOS |
| `vaapi` | | Linux VA-API hardware codecs |
| `v4l2` | | V4L2 hardware codecs |
| `wgpu` | | GPU rendering |
| `dmabuf-import` | | Zero-copy DMA-BUF import (Linux/Vulkan) |
| `android` | | Android MediaCodec + CameraX |
| `test-util` | | Deterministic test sources for integration tests |
