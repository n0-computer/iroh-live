# iroh-live

RULES FOR ALL AGENTS: if .agents folder exists, read it. ALWAYS KEEP IT IN MIND. if sth in there is marked as mandatory, treat it as such unless specifically told otherwise. if you deviate from anything, inform the user and justify why.

Rust workspace for real-time media over iroh (QUIC-based transport).

## Workspace crates

| Crate | Description |
|-------|-------------|
| `iroh-live` | High-level API: live sessions, rooms, tickets. Depends on `moq-media` + `iroh`. |
| `iroh-moq` | MoQ transport layer over iroh/quinn via `web-transport-iroh` (external crate). |
| `moq-media` | Media pipelines: capture, encode, decode, publish, subscribe, playout, adaptive bitrate. No iroh dependency. |
| `rusty-codecs` | Codec implementations (H.264 via openh264, AV1 via rav1e/rav1d, Opus) + VAAPI/V4L2/VideoToolbox HW accel + wgpu rendering. |
| `rusty-capture` | Cross-platform screen and camera capture: PipeWire, V4L2, X11, Apple ScreenCaptureKit/AVFoundation. DMA-BUF zero-copy. |
| `moq-media-egui` | egui integration for video rendering (wgpu texture upload, DMA-BUF import). |
| `moq-media-dioxus` | dioxus-native integration for video rendering. |

`gpu-debug` is an in-tree debug crate (not a workspace member).

## Source layout

### iroh-live
- `src/lib.rs` — re-exports
- `src/live.rs` — `Live` session manager
- `src/call.rs` — `Call` (1:1 session)
- `src/rooms.rs` — room management
- `src/rooms/publisher.rs` — room publisher
- `src/ticket.rs` — connection tickets
- `src/types.rs` — shared types
- `src/util.rs` — `StatsSmoother` and helpers
- `examples/` — `split.rs` (multi-source demo), `watch.rs` (egui viewer), `watch-wgpu.rs` (wgpu viewer), `rooms.rs`, `publish.rs`, `push.rs`, `room-publish-file.rs`, `viewer.rs` (in moq-media), `api_sketch.rs`
- `examples/common/` — shared example helpers (`mod.rs`, `import.rs`)
- `tests/e2e.rs` — end-to-end integration tests

### moq-media
- `src/lib.rs` — module re-exports
- `src/publish.rs` — `LocalBroadcast`, `VideoPublisher`, `AudioPublisher`, `VideoRenditions`, `AudioRenditions`
- `src/publish/controller.rs` — publish controller
- `src/subscribe.rs` — `RemoteBroadcast`, `VideoTrack`, `AudioTrack`, `MediaTracks`, `AdaptiveVideoTrack`, `CatalogSnapshot`
- `src/playout.rs` — `PlayoutClock`, `PlayoutBuffer`, `PlayoutMode`
- `src/adaptive.rs` — adaptive bitrate selection
- `src/transport.rs` — `MediaPacket`, `PacketSource`, `PacketSink`
- `src/pipeline.rs` — encode/decode pipeline orchestration
- `src/capture.rs` — capture source abstraction
- `src/net.rs` — network signal types
- `src/audio_backend.rs` — audio output (cpal/firewheel/sonora)
- `src/audio_backend/aec.rs` — acoustic echo cancellation
- `src/processing.rs`, `src/processing/mjpg.rs` — media processing
- `src/test_util.rs` — deterministic test sources (`test-util` feature)
- `tests/pipeline_integration.rs` — codec pipeline integration tests
- `tests/zero_copy_pipeline.rs` — zero-copy pipeline tests

### rusty-codecs
- `src/lib.rs`, `src/codec.rs` — codec trait and registry
- `src/codec/h264.rs`, `h264/encoder.rs`, `h264/decoder.rs`, `h264/sps.rs`, `h264/annexb.rs` — H.264 (openh264)
- `src/codec/av1.rs`, `av1/encoder.rs`, `av1/decoder.rs`, `av1/rav1d_safe.rs` — AV1 (rav1e/rav1d)
- `src/codec/opus.rs`, `opus/encoder.rs`, `opus/decoder.rs` — Opus audio
- `src/codec/vaapi.rs`, `vaapi/encoder.rs`, `vaapi/decoder.rs` — VAAPI hardware codec
- `src/codec/v4l2.rs`, `v4l2/encoder.rs`, `v4l2/decoder.rs` — V4L2 hardware codec
- `src/codec/vtb.rs`, `vtb/encoder.rs`, `vtb/decoder.rs` — VideoToolbox (macOS)
- `src/codec/dynamic.rs` — dynamic codec selection
- `src/codec/test_util.rs` — test helpers
- `src/codec/tests/` — `harness.rs`, `patterns.rs`, `vectors.rs`, `metrics.rs`, `latency.rs`
- `src/render.rs` — wgpu video rendering
- `src/render/dmabuf_import.rs` — DMA-BUF zero-copy import + VPP retiler
- `src/processing.rs` — image processing
- `src/processing/scale.rs`, `convert.rs`, `resample.rs` — scale, colorspace, audio resample
- `src/format.rs` — frame/sample format types
- `src/config.rs` — codec configuration
- `src/traits.rs` — shared codec traits

### rusty-capture
- `src/lib.rs` — public API: `ScreenCapture`, `CameraCapture`, list functions
- `src/types.rs` — `MonitorInfo`, `CameraInfo`, `CapturedFrame`
- `src/platform/linux/pipewire.rs` — PipeWire screen + camera capture
- `src/platform/linux/v4l2.rs` — V4L2 camera capture
- `src/platform/linux/x11.rs` — X11 screen capture (SHM)
- `src/platform/apple/screen.rs` — ScreenCaptureKit
- `src/platform/apple/camera.rs` — AVFoundation camera
- `src/platform/windows/mod.rs`, `src/platform/android/mod.rs` — stubs
- `tests/camera.rs`, `tests/pipewire_reopen.rs` — integration tests

### iroh-moq
- `src/lib.rs` — `MoqSession`, connection management

### moq-media-egui
- `src/lib.rs` — egui video widget, wgpu texture upload

### moq-media-dioxus
- `src/lib.rs` — dioxus video component
- `examples/demo/main.rs` — demo app

## Build

```sh
cargo build --workspace
cargo build --workspace --all-features
```

## Test

```sh
cargo test --workspace
cargo test --workspace --all-features
```

Integration tests in `moq-media/tests/pipeline_integration.rs` use the
`test-util` feature for deterministic video/audio sources. They run with
default features (h264 + av1 + opus).

## Lint

```sh
cargo clippy --locked --workspace --all-targets --all-features
cargo fmt --check
```

## Shell

- Always use `rg` (ripgrep) instead of `grep` in shell commands

## Commits

- Small incremental commits, each leaving all crates compiling
- `cargo clippy --locked --workspace --all-targets --all-features` must be clean (no warnings)
- `cargo fmt --check` must pass
- Commit messages: start with *why* (motivation), then any decisions and their reasoning, then cover *what*, then points of note.
- Follow ./.agents/writing.md for all prose, including commit messages.
- When working through a multi-step prompt, add "(phase Xb of current prompt)" as the last line

## Complexity check

Before adding significant amounts of code, verify:
1. The approach is solid — not just the first thing that came to mind.
2. No simpler alternative achieves the same goal.
3. Compare to industry-standard tools (mpv, GStreamer, OBS, ffmpeg) if relevant.
4. Check if a good Rust crate already handles the task.

Complexity is fine when warranted — this is a genuinely complex project. The point is to be deliberate.

## Key types

### moq-media publish side

- `LocalBroadcast` — owns a `BroadcastProducer`, manages encoder pipelines
- `VideoPublisher` / `AudioPublisher` — slot handles for `set()`, `replace()`, `clear()`
- `VideoRenditions` / `AudioRenditions` — simulcast layer sets sharing a source
- `SharedVideoSource` — thread-safe wrapper that parks when no subscribers

### moq-media subscribe side

- `RemoteBroadcast` — wraps `BroadcastConsumer`, watches catalog, owns `PlayoutClock`
- `VideoTrack` / `AudioTrack` — decoded media tracks (frame channel + decoder handle)
- `MediaTracks` — convenience: broadcast + optional video + optional audio
- `CatalogSnapshot` — point-in-time catalog with rendition selection helpers
- `AdaptiveVideoTrack` — auto-switches renditions based on `NetworkSignals`

### Playout and sync

- `PlayoutClock` — PTS→wall-clock mapping, jitter measurement, buffer re-anchoring
- `PlayoutBuffer` — post-decoder frame buffer gated on playout time
- `PlayoutMode::Live { buffer, max_latency }` — real-time with frame skipping
- `PlayoutMode::Reliable` — every frame in order, no latency target (tests, recordings)

### Transport

- `MediaPacket` / `PacketSource` / `PacketSink` — codec-agnostic transport boundary
- `MoqPacketSource` / `MoqPacketSink` — moq-lite implementations
- `OrderedConsumer` — hang group ordering with max_latency skip

## Feature flags (key crates)

### rusty-codecs
`h264`, `opus`, `av1` (default), `vaapi`, `v4l2`, `videotoolbox`, `wgpu`, `dmabuf-import`, `hang`

### rusty-capture
`camera`, `screen` (default) → `pipewire`, `v4l2`, `x11`, `camera-apple`, `screen-apple`

### moq-media
`h264`, `opus`, `av1`, `capture` (default), `vaapi`, `v4l2`, `videotoolbox`, `wgpu`, `dmabuf-import`, `test-util`

### iroh-live
`av1` (default), `vaapi`, `wgpu`, `dmabuf-import`

## iroh Connection & Path Stats

Access chain: `MoqSession::conn()` → `&Connection<HandshakeCompleted>` (from `iroh::endpoint`).

### Connection-level

- `conn.stats()` → `ConnectionStats { udp_tx, udp_rx, frame_tx, frame_rx }`
- `conn.paths()` → `impl Watcher<Value = PathInfoList>` — `.get()` for snapshot

### Per-path stats

`conn.paths().get()` → `PathInfoList` → `.iter()` → `PathInfo`:

- `rtt()`, `is_selected()`, `remote_addr()`
- `stats()` → `PathStats { rtt, cwnd, lost_packets, lost_bytes, current_mtu, ... }`

### Loss rate

`loss_rate = lost_packets / (udp_tx.datagrams + lost_packets)` (selected path)

### StatsSmoother

`iroh_live::util::StatsSmoother` — smooths bandwidth/RTT over 1s intervals.

## Plans and docs

- `plans/PLANS.md` — master index of all plans with completion status
- `plans/future.md` — potential future features with use cases and effort estimates
- `plans/platforms.md` — platform support matrix (codecs, capture, GPU rendering)
- `plans/media-pipeline/00-main.md` — media pipeline master plan
- `plans/api/0-overview.md` — API redesign overview
- `REVIEW.md` — crate review findings and action items

## Platform testing

Currently tested only on Linux (Intel Meteor Lake, software codecs, VAAPI,
PipeWire capture, V4L2 capture). macOS, Windows, Android, and iOS have
varying levels of code but no confirmed test results. See `plans/platforms.md`
for the full matrix.

When testing a new platform, the workflow is:
1. Run `cargo check --all-features --workspace --tests --examples`
2. Run `cargo test -p moq-media --features test-util` for codec pipeline tests
3. Run platform-specific codec tests with appropriate feature flags
4. Update `plans/platforms.md` with results
