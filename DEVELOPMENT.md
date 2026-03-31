# Development guide

Working notes for contributors and AI agents. See [README.md](README.md)
for the project overview and quick start.

## Workspace structure

| Crate | Role |
|-------|------|
| `iroh-live` | High-level API: sessions, rooms, tickets. Depends on moq-media + iroh. |
| `iroh-live-cli` | CLI tool (`irl`): publish, play, call, room, record, run. |
| `iroh-live-relay` | Relay server for browser WebTransport bridging. |
| `iroh-moq` | MoQ transport over iroh/quinn. |
| `moq-media` | Media pipelines: capture, encode, decode, publish, subscribe, playout. No iroh dependency. |
| `rusty-codecs` | Codec implementations + GPU rendering. H.264, AV1, Opus, PCM, VAAPI, V4L2, VTB. |
| `rusty-capture` | Cross-platform capture: PipeWire, V4L2, X11, libcamera, nokhwa, xcap, Apple SCK. |
| `moq-media-egui` | egui video widget (wgpu texture upload, DMA-BUF import). |
| `moq-media-dioxus` | dioxus-native video component. |

Demos live in `demos/`: `pi-zero`, `pi-zero-minimal`, `headless`, `opengl`.

## Build and test

```sh
cargo build --workspace                         # default features
cargo build --workspace --all-features          # everything
cargo make check-all                            # full check: all feature combos + clippy + fmt
cargo test -p moq-media --features test-util    # codec pipeline tests
cargo test -p iroh-live                         # e2e, room, patchbay tests
```

Always run `cargo make check-all` before committing code changes.
Markdown-only changes can skip it.

## Commits

- Conventional prefixes: `feat:`, `fix:`, `test:`, `refactor:`, `perf:`, `ci:`, `docs:`, `chore:`
- Start with *why*, then decisions and reasoning, then *what*.
- Small incremental commits, each leaving all crates compiling.
- `cargo make check-all` must pass. New features need tests.

## Key types

### Publish side (moq-media)

- `LocalBroadcast` — owns a BroadcastProducer, manages encoder pipelines
- `VideoPublisher` / `AudioPublisher` — slot handles for set/replace/clear
- `LocalBroadcast::preview()` — returns a VideoTrack with raw decoded frames (no encode-decode roundtrip)

### Subscribe side (moq-media)

- `RemoteBroadcast` — wraps BroadcastConsumer, watches catalog
- `VideoTrack` / `AudioTrack` — decoded media tracks
- `VideoTrack::try_recv()` — non-blocking frame poll for game loops and ECS
- `VideoTrack::enable_adaptation()` — automatic rendition switching via channel-swap
- `AudioTrack::set_volume(f32)` — per-stream volume control

### Transport

- `MediaPacket` / `PacketSource` / `PacketSink` — codec-agnostic boundary
- `OrderedConsumer` — group ordering with max_latency skip

### Source specs (moq-media)

`VideoSourceSpec` and `AudioSourceSpec` in `moq_media::source_spec` parse
source strings like `cam:pw:0`, `screen`, `test`, `file:path.mp4`,
`preenc:libcamera`. Single canonical identifier per variant.

## Architecture notes

### Threading model

- Encoder/decoder pipelines run on OS threads (not tokio tasks) via `spawn_thread()`
- Audio output uses cpal callbacks on real-time OS threads
- Networking (MoQ sessions, gossip, room actor) runs on tokio
- Frame delivery between threads uses `FrameChannel` (bounded single-slot with AtomicU64 sender count)

### Shutdown

- `CancellationToken` for cooperative shutdown
- `AbortOnDropHandle` for tokio task cleanup
- `SyncInner` implements Drop to close the playout clock and unblock decode threads

### Capture backends

rusty-capture tries backends in priority order: libcamera, PipeWire, V4L2, nokhwa, xcap.
On macOS, nokhwa is the working camera backend (native AVFoundation is stubbed out).
Screen capture works via ScreenCaptureKit on macOS, PipeWire/X11 on Linux.

### GPU rendering

`WgpuVideoRenderer` handles NV12-to-RGBA conversion on the GPU. For DMA-BUF
sources (VAAPI decode), the VPP retiler converts tiled formats to ones Vulkan
can import. `render_cached()` provides a persistent output texture that avoids
per-frame allocation.

### Audio

Audio backend uses cpal with optional JACK support (`jack` feature).
`AudioBackend::available_hosts()` lists available hosts (ALSA, PipeWire, JACK).
Per-stream volume via `AudioTrack::set_volume(f32)`.
Audio file import uses symphonia (pure Rust: WAV, MP3, FLAC at 48kHz).

### Rooms

Rooms use iroh-gossip for peer discovery and iroh-smol-kv for state.
PeerState is serialized with postcard (positional binary format: do NOT use
`skip_serializing_if` on fields, it breaks deserialization).

## Tracing

- `error!` for breakage, `warn!` for degraded-but-recoverable, `info!` for lifecycle, `debug!` for operational detail, `trace!` for per-frame
- Use `throttled-tracing` for high-frequency logs
- Prefer structured fields over string interpolation
- Use `tracing_subscriber::fmt::init()` — respects RUST_LOG by default, no EnvFilter boilerplate

## Cross-compilation

See the cross-compilation section in [README.md](README.md) for building
aarch64 binaries for Raspberry Pi.

## Platform notes

Primary test platform: Linux (Intel Meteor Lake, VAAPI, PipeWire, V4L2).
macOS: software codecs + nokhwa camera + ScreenCaptureKit.
Windows, Android, iOS: code exists but testing is limited.

### Raspberry Pi

`demos/pi-zero/` has the full demo (camera, e-paper, watch mode).
`demos/pi-zero-minimal/` is an 80-line camera+mic publisher.
libcamera capture is in rusty-capture under the `libcamera` feature.
Cross-compile: `cargo make cross-build-aarch64 -- -p pi-zero-demo --release`.

## Conventions

- `n0_watcher::Watchable/Watcher` for reactive state (not tokio watch)
- Decoder threads are OS threads via `spawn_thread()`
- `CancellationToken` for shutdown, `AbortOnDropHandle` for task cleanup
- Bounded channels only (no unbounded, prevents memory leaks)
- `tracing_subscriber::fmt::init()` for logging setup (no EnvFilter boilerplate)
