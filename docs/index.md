# iroh-live documentation

## Guide

| Page | Summary |
|------|---------|
| [Getting started](guide/index.md) | Build, run examples, system dependencies |
| [Desktop rendering](guide/desktop.md) | wgpu, OpenGL, egui/dioxus integrations, framework options |
| [Raspberry Pi](guide/raspberry-pi.md) | Pi Zero 2 W setup, cross-compile, V4L2 HW codec, e-paper |
| [Android](guide/android.md) | Build with cargo-ndk, CameraX, MediaCodec, demo app |
| [Browser relay](guide/browser-relay.md) | Relay server for WebTransport, built-in web viewer |
| [Tickets](guide/tickets.md) | LiveTicket format, sharing, QR codes |
| [Rooms](guide/rooms.md) | Multi-party rooms with gossip coordination (experimental) |
| [MoQ protocol](guide/moq.md) | moq-lite wire protocol, broadcasts, tracks, groups |

## Architecture

| Page | Summary |
|------|---------|
| [Overview](architecture/index.md) | Crate layers, data flow, design principles |
| [Transport](architecture/transport.md) | MoQ over QUIC, iroh-moq, sessions, groups |
| [Publish pipeline](architecture/publish.md) | Encoder pipelines, VideoInput, SharedVideoSource |
| [Subscribe pipeline](architecture/subscribe.md) | Decoder pipelines, catalog, VideoTrack |
| [Playout and sync](architecture/playout.md) | PlayoutClock, jitter buffer, A/V sync, DPB bursts |
| [Adaptive bitrate](architecture/adaptive.md) | Rendition switching, NetworkSignals, thresholds |
| [Capture](architecture/capture.md) | rusty-capture, PipeWire, V4L2, ScreenCaptureKit |
| [Codecs](architecture/codecs.md) | Codec traits, software and hardware backends |
| [Rendering](architecture/rendering.md) | wgpu, GLES2, Android EGL, DMA-BUF zero-copy |
| [Audio](architecture/audio.md) | Audio backend, cpal, device switching |
| [Echo cancellation](architecture/audio/aec.md) | sonora AEC3, signal flow, real-time safety |
| [Performance](architecture/performance.md) | Allocation budgets, hot-path optimization |
| [P2P and relay](architecture/p2p-relay.md) | Direct connectivity, relay fallback, WebTransport bridge |
| [Developer tools](architecture/devtools.md) | Debug overlays, metrics, network simulation |

### API design

| Page | Summary |
|------|---------|
| [iroh-live API](architecture/iroh-live/api.md) | Live, Call, Room: design rationale and reactivity model |
| [moq-media API](architecture/moq-media/api.md) | LocalBroadcast, RemoteBroadcast, transport-agnostic design |

### Platforms

| Page | Summary |
|------|---------|
| [Linux VAAPI](architecture/linux/vaapi.md) | VAAPI encode/decode via cros-codecs |
| [Linux DMA-BUF](architecture/linux/dmabuf.md) | Zero-copy rendering, VPP retiler, modifier negotiation |
| [macOS VideoToolbox](architecture/macos/videotoolbox.md) | VideoToolbox H.264 encode/decode |
| [Android MediaCodec](architecture/android/mediacodec.md) | NDK codec, HardwareBuffer, EGL zero-copy |
| [Raspberry Pi V4L2](architecture/pi/v4l2.md) | V4L2 M2M, bcm2835-codec quirks, cross-SoC portability |
