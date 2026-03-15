# iroh-live

RULES FOR ALL AGENTS: if .agents folder exists, read it. ALWAYS KEEP IT IN MIND. if sth in there is marked as mandatory, treat it as such unless specifically told otherwise. if you deviate from anything, inform the user and justify why.

Rust workspace for real-time media over iroh (QUIC-based transport).

## Crates

| Crate | Description |
|-------|-------------|
| `iroh-live` | High-level API for live audio/video sessions and rooms |
| `iroh-moq` | Media-over-QUIC transport protocol |
| `moq-media` | Media capture, encoding, decoding, and processing pipelines |
| `rusty-codecs` | Video and audio codecs (H.264, AV1, Opus) with hardware acceleration |
| `rusty-capture` | Cross-platform screen and camera capture with DMA-BUF zero-copy |
| `moq-media-egui` | egui integration for video rendering |
| `moq-media-dioxus` | dioxus-native integration for video rendering |
| `web-transport-iroh` | WebTransport implementation over iroh/quinn |

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
- follow ./.agents/writing.md for all prose, including commit messages.
- When working through a multi-step prompt, add "(phase Xb of current prompt)" as the last line

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
