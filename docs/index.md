# iroh-live documentation

## Guide

| Page | Summary |
|------|---------|
| [Getting started](guide/index.md) | System dependencies, building the workspace, and running the examples for the first time. |
| [Desktop rendering](guide/desktop.md) | Choosing between wgpu, OpenGL, and egui or dioxus for desktop video rendering. |
| [Raspberry Pi](guide/raspberry-pi.md) | Cross-compiling for Pi Zero 2 W, hardware H.264 via V4L2, and e-paper QR display. |
| [Android](guide/android.md) | Building the JNI bridge with cargo-ndk, CameraX capture, and the demo app architecture. |
| [Browser relay](guide/browser-relay.md) | Running the relay server that bridges iroh P2P publishers to WebTransport browser viewers. |
| [Tickets](guide/tickets.md) | How LiveTicket URIs encode endpoint addresses, and how to share them via QR codes. |
| [Rooms](guide/rooms.md) | Multi-party rooms built on iroh-gossip for automatic peer discovery and broadcast sharing. |
| [MoQ protocol](guide/moq.md) | How moq-lite maps broadcasts, tracks, and groups onto QUIC streams. |

## Architecture

| Page | Summary |
|------|---------|
| [Overview](architecture/index.md) | Crate layering from iroh-live down to rusty-codecs, data flow through the publish and subscribe pipelines, and the design principles behind the split. |
| [Transport](architecture/transport.md) | How iroh-moq wraps moq-lite sessions over iroh QUIC connections, and how `PacketSource`/`PacketSink` decouple media from transport. |
| [Publish pipeline](architecture/publish.md) | `LocalBroadcast`, encoder pipelines, `VideoInput` variants, `SharedVideoSource` fan-out, and the `PacketSink` boundary. |
| [Subscribe pipeline](architecture/subscribe.md) | `RemoteBroadcast`, catalog watching, decoder pipelines, `VideoTrack`/`AudioTrack`, and viewport-driven scaling. |
| [Playout and sync](architecture/playout.md) | Shared playout clock (`Sync`), `FramePacer` fallback, `PlaybackPolicy`, and lag tracking. |
| [A/V sync](architecture/av-sync.md) | Sync design (ported from moq/js), `PlaybackPolicy` tuning knobs, overlay metrics, and diagnostic interpretation. |
| [Adaptive bitrate](architecture/adaptive.md) | Subscribe-side rendition switching driven by `NetworkSignals`, with bandwidth and loss thresholds, probe-based upgrades, and emergency downgrades. |
| [Capture](architecture/capture.md) | The rusty-capture abstraction over PipeWire, V4L2, X11, ScreenCaptureKit, and AVFoundation backends. |
| [Codecs](architecture/codecs.md) | `VideoEncoder`/`VideoDecoder` traits, software backends (openh264, rav1e/rav1d, Opus), and the `DynamicVideoDecoder` fallback chain. |
| [Rendering](architecture/rendering.md) | Three rendering backends (wgpu, GLES2, Android EGL) and how they integrate with egui and dioxus. |
| [Audio](architecture/audio.md) | The cpal-based audio backend, device enumeration, hot-switching, and the driver thread model. |
| [Echo cancellation](architecture/audio/aec.md) | Real-time AEC via sonora, the render/capture signal flow, and constraints on the audio callback. |
| [Performance](architecture/performance.md) | Per-frame allocation budgets on the encode and decode paths, and the strategy for hot-path optimization. |
| [P2P and relay](architecture/p2p-relay.md) | Direct P2P connectivity via iroh, relay fallback for NAT traversal, and the WebTransport bridge for browsers. |
| [Developer tools](architecture/devtools.md) | Debug overlays, `Metric`/`Label` stats infrastructure, `frame_dump` diagnostic, and patchbay network simulation tests. |

### API design

| Page | Summary |
|------|---------|
| [iroh-live API](architecture/iroh-live/api.md) | The `Live` builder, `Call` for 1:1 sessions, `Room` for multi-party, and the reactive state model via `n0_watcher`. |
| [moq-media API](architecture/moq-media/api.md) | `LocalBroadcast`/`RemoteBroadcast` publish-subscribe split, transport-agnostic design, and the codec trait hierarchy. |

### Platforms

| Page | Summary |
|------|---------|
| [Linux VAAPI](architecture/linux/vaapi.md) | Hardware H.264 encode and decode via cros-codecs, VA surface management, and the low-latency SPS VUI patch. |
| [Linux DMA-BUF](architecture/linux/dmabuf.md) | Zero-copy frame import into Vulkan via DMA-BUF, the VPP retiler for modifier conversion, and how the pipeline avoids GPU-to-CPU readback. |
| [macOS VideoToolbox](architecture/macos/videotoolbox.md) | VideoToolbox H.264 encode and decode, CVPixelBuffer handling, and current test status. |
| [Android MediaCodec](architecture/android/mediacodec.md) | NDK MediaCodec for H.264, AHardwareBuffer zero-copy rendering, and the JNI boundary design. |
| [Raspberry Pi V4L2](architecture/pi/v4l2.md) | V4L2 stateful M2M codec on bcm2835, quirks around DPB sizing and bitrate control, and cross-SoC portability notes. |
