# iroh-live documentation

## Guide

| Page | Summary |
|---|---|
| [Getting started](guide/index.md) | System dependencies, building, and a first stream from the CLI and the library |
| [CLI reference](cli.md) | The `irl` commands and their flags |
| [Desktop rendering](guide/desktop.md) | Drawing decoded frames with wgpu and egui |
| [Tickets](guide/tickets.md) | How a broadcast or a room is named and shared |
| [Rooms](guide/rooms.md) | Rooms with several participants, over gossip |
| [MoQ, as it appears here](guide/moq.md) | Broadcasts, tracks, groups, and the catalog |
| [Raspberry Pi](guide/raspberry-pi.md) | `rpicam-vid` capture, the V4L2 codecs, the Pi demos, and cross-compiling |
| [Android](guide/android.md) | The demo app and its JNI bridge |
| [Browser relay](guide/browser-relay.md) | Serving iroh broadcasts to browsers over WebTransport |
| [Platform support](platforms.md) | What runs where, and what is tested |

## Architecture

| Page | Summary |
|---|---|
| [Overview](architecture/index.md) | The crates, what `iroh-live-media` adds to moq-video and moq-audio, and the conventions |
| [The media stack](architecture/media-stack.md) | What we use from moq-video and moq-audio, and the feature flags |
| [Transport](architecture/transport.md) | `iroh-moq`: paths, the route table, publications and audiences, sessions, relay links, and ALPN negotiation |
| [Publishing](architecture/publish.md) | `LocalBroadcast`, sources, the simulcast ladder, and encoders that run only on demand |
| [Subscribing](architecture/subscribe.md) | `RemoteBroadcast`, video decoding, and rendition switches |
| [Adaptive rendition switching](architecture/adaptive.md) | How a player picks a rendition from the link |
| [Playout and A/V sync](architecture/playout.md) | The playout clock and latency |
| [Peer-to-peer and the relay](architecture/p2p-relay.md) | Direct connections, and the relay for browsers |
| [Instrumentation and tests](architecture/devtools.md) | Metrics, the debug overlay, and the test suites |
