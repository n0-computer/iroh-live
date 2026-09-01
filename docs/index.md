# iroh-live documentation

## Guide

| Page | Summary |
|---|---|
| [Getting started](guide/index.md) | System dependencies, building, and a first stream in the CLI and the library |
| [CLI reference](cli.md) | Every flag of `irl devices`, `irl publish`, and `irl watch` |
| [Desktop rendering](guide/desktop.md) | Drawing decoded frames with wgpu and egui |
| [Tickets](guide/tickets.md) | How connection information is encoded and shared |
| [Rooms](guide/rooms.md) | Multi-party rooms over gossip, and why the crate is a holding pattern |
| [MoQ, as it appears here](guide/moq.md) | Broadcasts, tracks, groups, and the catalog, in the vocabulary these docs use |
| [Raspberry Pi](guide/raspberry-pi.md) | The two Pi demos, `rpicam-vid` capture, GLES2 rendering, and cross-compiling |
| [Android](guide/android.md) | The demo app, the JNI bridge, and where MediaCodec lives now |
| [Browser relay](guide/browser-relay.md) | Bridging iroh publishers to browsers over WebTransport |
| [Platform support](platforms.md) | What runs where, and what is untested |

## Architecture

| Page | Summary |
|---|---|
| [Overview](architecture/index.md) | The crates, what `moq-media` adds over upstream, and the conventions |
| [The media stack](architecture/media-stack.md) | What we use from moq-video and moq-audio, what we contributed back, and what was lost |
| [Transport](architecture/transport.md) | `iroh-moq`: the node origin, session lifetime, and ALPN negotiation |
| [Publishing](architecture/publish.md) | `LocalBroadcast`, sources, the simulcast ladder, and demand-gated encoders |
| [Subscribing](architecture/subscribe.md) | `RemoteBroadcast`, the decode supervisor, and the rendition swap |
| [Adaptive rendition switching](architecture/adaptive.md) | The selection algorithm, its thresholds, and what is not wired up |
| [Playout and A/V sync](architecture/playout.md) | The shared playout clock and the playback policy |
| [Peer-to-peer and the relay](architecture/p2p-relay.md) | Direct connectivity, and the relay that bridges to browsers |
| [Instrumentation and tests](architecture/devtools.md) | Metrics, the debug overlay, and the test suites |
