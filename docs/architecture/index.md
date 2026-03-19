# Architecture

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | iroh-live, moq-media, iroh-moq, rusty-codecs, rusty-capture |

This section documents how iroh-live works internally: the crate boundaries and their responsibilities, the pipeline from capture to rendering, the design principles behind the public API, and platform-specific codec and GPU details.

## Crate layers

The workspace is organized into three layers. Each layer depends only on the layers below it, and each can be used independently.

**iroh-live** is the entry point for applications that want video calls, rooms, or live streaming with peer-to-peer connectivity. It provides `Live` (session manager), `Call` (1:1 sessions), and `Room` (multi-party via gossip). It re-exports moq-media and adds signaling, identity, and connection management over iroh's QUIC transport. See [iroh-live API design](iroh-live/api.md).

**moq-media** owns the media pipeline: broadcast management (`LocalBroadcast`, `RemoteBroadcast`), codec orchestration, playout timing, adaptive bitrate, and the audio backend. It has no dependency on iroh, so non-RTC use cases (recording pipelines, studio links, camera dashboards) can use it directly. See [moq-media API design](moq-media/api.md).

**iroh-moq** connects iroh's QUIC endpoint to moq-lite broadcast primitives. It manages sessions and provides the publish/subscribe interface. See [transport](transport.md).

Below these sit **rusty-codecs** (codec implementations, hardware acceleration, GPU rendering) and **rusty-capture** (platform-specific screen and camera capture). These define the `VideoEncoder`, `VideoDecoder`, and `VideoSource` traits that moq-media builds on.

## Design principles

**`&self` everywhere.** All public types use interior mutability, making them safe to share across async tasks and threads without wrapper gymnastics.

**Drop-based cleanup.** Dropping a `Call` closes it. Dropping a `LocalBroadcast` tears down encoder pipelines. Dropping a `VideoTrack` stops its decoder thread.

**Watcher for continuous state, stream for discrete events.** Connection quality, active rendition, and catalog contents are continuous values exposed via `n0_watcher::Direct<T>`. Participant joins and session arrivals are discrete events exposed as `impl Stream`.

**Declarative intent, not mechanism.** `VideoTarget { max_pixels: 1280*720 }` tells the system what quality the subscriber needs. The catalog selects the best matching rendition automatically.

**moq-media is standalone.** A recording pipeline can use `LocalBroadcast` and `RemoteBroadcast` directly, without importing iroh-live. The transport boundary is the `PacketSink`/`PacketSource` trait pair.

## Data flow

A capture-to-render pipeline crosses all three layers:

```
capture source (rusty-capture, VideoSource trait)
    |
    v
encoder pipeline (moq-media, dedicated OS thread)
    |
    v  EncodedFrame
PacketSink (MoqPacketSink starts new MoQ group on each keyframe)
    |
    v  MoQ transport (iroh-moq, QUIC streams)
PacketSource (MoqPacketSource reads ordered frames)
    |
    v  MediaPacket
decoder pipeline (moq-media, dedicated OS thread)
    |
    v  VideoFrame
PlayoutBuffer (smooths bursty decoder output)
    |
    v
renderer (wgpu texture upload or egui widget)
```

Encoder and decoder pipelines run on dedicated OS threads, not tokio tasks, so slow codec operations never block the async runtime. The `forward_packets` async task bridges the network-side `PacketSource` into an mpsc channel that the decoder thread reads synchronously.

## Platform coverage

Linux (Intel Meteor Lake) is the primary development and test platform, with full hardware acceleration (VAAPI encode/decode, DMA-BUF zero-copy rendering) and PipeWire capture. macOS has VideoToolbox codec support and ScreenCaptureKit/AVFoundation capture with some manual testing. Android has been tested end-to-end with bidirectional video calls (MediaCodec HW, CameraX, EGL rendering). Raspberry Pi has V4L2 hardware codecs tested up to 1080p. Windows support (Media Foundation codecs, DXGI capture) is planned but not yet implemented.

See [platforms.md](../../plans/platforms.md) for the full support matrix.
