# Architecture Overview

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | iroh-live, moq-media, iroh-moq, rusty-codecs, rusty-capture |

## Crate layers

The workspace is organized into three layers. Each layer depends only on
the layers below it, and each can be used independently for its scope of
concern.

**iroh-live** is the product layer. It provides `Live` (session manager),
`Call` (1:1 sessions), and rooms with participant models. It re-exports
types from moq-media and adds signaling, identity, and connection
management over iroh's QUIC transport. This is the entry point for
applications that want video calls, rooms, or live streaming with
peer-to-peer connectivity.

**moq-media** is the media pipeline layer. It owns broadcast management
(`LocalBroadcast`, `RemoteBroadcast`), codec pipeline orchestration
(encode/decode on OS threads), playout timing, adaptive rendition
switching, and the `PacketSink`/`PacketSource` transport boundary. It has
no dependency on iroh. Non-RTC use cases (studio links, recording
pipelines, file-based playback) use moq-media directly without iroh-live.

**iroh-moq** is the transport layer. It connects iroh's QUIC endpoint to
moq-lite broadcast primitives. `Moq` manages the endpoint integration and
connection lifecycle; `MoqSession` represents a single peer connection with
publish/subscribe operations. See [transport.md](transport.md) for details.

**rusty-codecs** provides codec implementations (H.264 via openh264, AV1
via rav1e/rav1d, Opus), hardware acceleration (VAAPI, V4L2, VideoToolbox),
image processing, and wgpu-based GPU rendering. It defines the core traits
(`VideoEncoder`, `VideoDecoder`, `VideoSource`) that the rest of the stack
builds on.

**rusty-capture** provides platform-specific screen and camera capture
behind the `VideoSource` trait. PipeWire, V4L2, X11, ScreenCaptureKit,
and AVFoundation backends run on dedicated OS threads. See
[capture.md](capture.md) for the full picture.

## Design principles

These principles govern the public API across moq-media and iroh-live.

**`&self` everywhere.** All public types use interior mutability. Callers
never need `&mut self`, which makes types safe to share across async tasks
and threads without wrapper gymnastics.

**Drop-based cleanup.** Dropping a `Call` closes it. Dropping a
`LocalBroadcast` tears down encoder pipelines. Dropping a `VideoTrack`
stops its decoder thread. No explicit shutdown calls are required.

**Watcher for continuous state, stream for discrete events.** Connection
quality, active rendition, and catalog contents are continuous values that
always have a current snapshot, exposed via `n0_watcher::Direct<T>` (call
`.get()` for the snapshot, `.updated().await` to wait for changes).
Participant joins, track publications, and session arrivals are discrete
events exposed as `impl Stream`.

**Declarative intent, not mechanism.** `VideoTarget { max_pixels: 1280*720 }`
tells the system what quality the subscriber needs. The catalog selects
the best matching rendition. Callers do not manually pick rendition
strings unless they want to.

**moq-media is standalone.** A recording pipeline or a camera dashboard
can use `LocalBroadcast` and `RemoteBroadcast` directly, without importing
iroh-live or running iroh networking. The transport boundary is the
`PacketSink`/`PacketSource` trait pair.

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

Encoder and decoder pipelines run on dedicated OS threads, not tokio
tasks. This keeps slow codec operations off the async runtime entirely.
The `forward_packets` async task bridges the network-side `PacketSource`
into an `mpsc` channel that the decoder thread reads synchronously.

## Pages in this section

### Pipeline

- [transport.md](transport.md) -- MoQ transport layer (iroh-moq)
- [publish.md](publish.md) -- Publish pipeline and encoder management
- [subscribe.md](subscribe.md) -- Subscribe pipeline and decoder management
- [playout.md](playout.md) -- Playout timing, buffering, and A/V sync
- [adaptive.md](adaptive.md) -- Adaptive rendition switching
- [capture.md](capture.md) -- Platform capture backends

### Cross-cutting

- [codecs.md](codecs.md) -- Codec architecture (traits, auto-selection, software and hardware backends)
- [rendering.md](rendering.md) -- Rendering backends (wgpu, GLES2, Android EGL)
- [audio.md](audio.md) -- Audio backend (cpal, resampling, device management)
- [audio/aec.md](audio/aec.md) -- Acoustic echo cancellation (sonora AEC3)
- [performance.md](performance.md) -- Per-frame allocation budget and optimization status
- [p2p-relay.md](p2p-relay.md) -- P2P connectivity and browser relay
- [devtools.md](devtools.md) -- Debug overlay, metrics, network simulation

### API design

- [iroh-live/api.md](iroh-live/api.md) -- iroh-live API (Live, Call, Room)
- [moq-media/api.md](moq-media/api.md) -- moq-media API (LocalBroadcast, RemoteBroadcast, transport boundary)

### Platform-specific

- [linux/vaapi.md](linux/vaapi.md) -- VAAPI hardware codec on Linux
- [linux/dmabuf.md](linux/dmabuf.md) -- DMA-BUF zero-copy rendering on Linux
- [macos/videotoolbox.md](macos/videotoolbox.md) -- VideoToolbox on macOS
- [android/mediacodec.md](android/mediacodec.md) -- MediaCodec on Android
- [pi/v4l2.md](pi/v4l2.md) -- V4L2 M2M codec on Raspberry Pi
