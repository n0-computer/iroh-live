# iroh-live documentation

## Guide

| Page | Summary | Modified |
|------|---------|----------|
| [Getting started](guide/index.md) | Build, run examples, system dependencies | 2026-03-19 |
| [Desktop rendering](guide/desktop.md) | wgpu, OpenGL, egui/dioxus integrations, framework options | 2026-03-19 |
| [Raspberry Pi](guide/raspberry-pi.md) | Pi Zero 2 W setup, cross-compile, V4L2 HW codec, e-paper | 2026-03-19 |
| [Android](guide/android.md) | Build with cargo-ndk, CameraX, MediaCodec, demo app | 2026-03-19 |
| [Browser relay](guide/browser-relay.md) | Relay server for WebTransport, built-in web viewer | 2026-03-19 |
| [Tickets](guide/tickets.md) | LiveTicket format, sharing, QR codes | 2026-03-19 |
| [Rooms](guide/rooms.md) | Multi-party rooms with gossip coordination (experimental) | 2026-03-19 |
| [MoQ protocol](guide/moq.md) | moq-lite wire protocol, broadcasts, tracks, groups | 2026-03-19 |

## Architecture

| Page | Summary | Modified |
|------|---------|----------|
| [Overview](architecture/index.md) | Crate layers, data flow, design principles | 2026-03-19 |
| [Transport](architecture/transport.md) | MoQ over QUIC, iroh-moq, sessions, groups | 2026-03-19 |
| [Publish pipeline](architecture/publish.md) | Encoder pipelines, VideoInput, SharedVideoSource | 2026-03-19 |
| [Subscribe pipeline](architecture/subscribe.md) | Decoder pipelines, catalog, VideoTrack | 2026-03-19 |
| [Playout and sync](architecture/playout.md) | PlayoutClock, jitter buffer, A/V sync, DPB bursts | 2026-03-19 |
| [Adaptive bitrate](architecture/adaptive.md) | Rendition switching, NetworkSignals, thresholds | 2026-03-19 |
| [Capture](architecture/capture.md) | rusty-capture, PipeWire, V4L2, ScreenCaptureKit | 2026-03-19 |
| [Codecs](architecture/codecs.md) | Codec traits, software and hardware backends | 2026-03-19 |
| [Rendering](architecture/rendering.md) | wgpu, GLES2, Android EGL, DMA-BUF zero-copy | 2026-03-19 |
| [Audio](architecture/audio.md) | Audio backend, cpal, device switching | 2026-03-19 |
| [Echo cancellation](architecture/audio/aec.md) | sonora AEC3, signal flow, real-time safety | 2026-03-19 |
| [Performance](architecture/performance.md) | Allocation budgets, hot-path optimization | 2026-03-19 |
| [P2P and relay](architecture/p2p-relay.md) | Direct connectivity, relay fallback, WebTransport bridge | 2026-03-19 |
| [Developer tools](architecture/devtools.md) | Debug overlays, metrics, network simulation | 2026-03-19 |

### API design

| Page | Summary | Modified |
|------|---------|----------|
| [iroh-live API](architecture/iroh-live/api.md) | Live, Call, Room: design rationale and reactivity model | 2026-03-19 |
| [moq-media API](architecture/moq-media/api.md) | LocalBroadcast, RemoteBroadcast, transport-agnostic design | 2026-03-19 |

### Platforms

| Page | Summary | Modified |
|------|---------|----------|
| [Linux VAAPI](architecture/linux/vaapi.md) | VAAPI encode/decode via cros-codecs | 2026-03-19 |
| [Linux DMA-BUF](architecture/linux/dmabuf.md) | Zero-copy rendering, VPP retiler, modifier negotiation | 2026-03-19 |
| [macOS VideoToolbox](architecture/macos/videotoolbox.md) | VideoToolbox H.264 encode/decode | 2026-03-19 |
| [Android MediaCodec](architecture/android/mediacodec.md) | NDK codec, HardwareBuffer, EGL zero-copy | 2026-03-19 |
| [Raspberry Pi V4L2](architecture/pi/v4l2.md) | V4L2 M2M, bcm2835-codec quirks, cross-SoC portability | 2026-03-19 |
