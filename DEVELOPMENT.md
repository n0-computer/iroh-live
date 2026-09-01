# Development guide

Working notes for contributors. [README.md](README.md) has the project overview
and the quick start, and [docs/](docs/index.md) has the architecture and the
guides.

## Workspace

| Crate | Role |
|---|---|
| `iroh-live` | `Live`, `Call`, `Subscription`, tickets. Depends on `moq-media` and `iroh-moq` |
| `iroh-moq` | MoQ transport over iroh: the node origin, sessions, ALPN negotiation |
| `iroh-rooms` | Gossip rooms. No media dependency |
| `moq-media` | Publish and subscribe plumbing over moq-video and moq-audio. No iroh dependency |
| `moq-media-egui` | egui widget and debug overlay |
| `moq-media-android` | Camera2 push bridge and EGL renderer |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

Demos live in `demos/`: `android`, `pi-zero`, `pi-zero-minimal`.

Codecs, capture, decoding, and the wgpu renderer are upstream in `moq-video` and
`moq-audio`. Nothing here implements one. See
[docs/architecture/media-stack.md](docs/architecture/media-stack.md).

## The patch block

`Cargo.toml` carries a `[patch.crates-io]` block pointing the moq crates at a
local `../moq` checkout, for five changes that have not reached a release. Every
pinned version matches what `moq-dev/moq@main` publishes, so deleting the block
is the whole revert once they land. CI cannot pass until then.

## Build and test

```sh
cargo build --workspace                 # default features
cargo build --workspace --all-features  # everything

cargo make check-all   # check and clippy across three feature sets, then fmt
cargo make test        # cargo nextest across the workspace
cargo make test-e2e    # Playwright browser suite, building the relay and CLI first
cargo make test-full   # all three
```

Run `cargo make check-all` before committing code. It covers default features,
`--all-features`, and `--no-default-features`, which is where feature-gated
mistakes show up. Markdown-only changes can skip it.

Cross-compiling for aarch64 is `cargo make cross-sysroot-aarch64` once, then
`cargo make cross-build-aarch64 -- <cargo args>`. See
[cross/README.md](cross/README.md).

## Commits

Conventional prefixes: `feat:`, `fix:`, `test:`, `refactor:`, `perf:`, `ci:`,
`docs:`, `chore:`. Lead with why, then the reasoning, then what changed. Keep
commits small enough that each one leaves the workspace compiling. New behaviour
needs a test.

## Key types

Publishing, in `moq_media::publish`:

- `LocalBroadcast` owns a `moq_net::broadcast::Producer` and the catalog.
- `VideoPublisher::set_renditions(source, renditions)` opens the source once and
  fans frames out to one encoder per rendition.
- `VideoSource` is `Capture`, `Frames`, or `AnnexB`; `AudioSource` is `Device` or
  `Frames`.
- `LocalBroadcast::preview()` taps the raw frames on their way to the encoders.

Subscribing, in `moq_media::subscribe`:

- `RemoteBroadcast` watches the catalog and hands out tracks.
- `VideoTrack::take()` polls the latest-wins frame slot; `recv()` awaits.
- `VideoTrack::set_rendition` and `enable_adaptation` drive the same request
  channel.
- `AudioTrack` writes into the process-wide `moq_media::playback` engine.

Transport, in `iroh_moq`: `Moq::publish(path)` returns a producer synchronously
and announces it node-wide. `MoqSession::subscribe(path)` waits for the peer's
announce. `MoqSession::conn()` is the iroh `Connection` behind it.

## Threading

Codecs run on their own threads inside `moq_video::encode::Sink` and
`moq_video::decode::Sink`, so this repository spawns almost none. The audio file
reader is the exception: symphonia decoding is blocking, so it runs on a named OS
thread and feeds a bounded channel. `iroh_live::util::spawn_thread` is the helper
for that pattern and currently has no callers.

`moq_video::decode::Consumer::read` is not cancel-safe. Never poll it from a
`select!` arm. The video decode path gives each decoder a task that reads it in a
plain loop and forwards over a bounded channel, and the supervisor selects only
on cancel-safe things. See
[docs/architecture/subscribe.md](docs/architecture/subscribe.md).

Networking, adaptation, and the room actor are ordinary tokio tasks. Audio output
is a cpal callback on a real-time thread owned by `moq_audio::playback::Engine`.

## Conventions

- `n0_watcher::Watchable` and `Direct<T>` for continuous state, not `tokio::watch`.
- `CancellationToken` for cooperative shutdown, `AbortOnDropHandle` to tie a task
  to a handle.
- Bounded channels only. Frames to a renderer go through the single-slot
  latest-wins `frame_channel`, not a queue.
- `tracing_subscriber::fmt::init()` for setup: it respects `RUST_LOG` with no
  `EnvFilter` boilerplate. Use `throttled-tracing` for anything per-frame, and
  structured fields rather than string interpolation.
- Rust doc comments follow RFC 1574: third-person declarative sentences starting
  with a verb, no headings in item docs, types linked with `[`Type`]`.
- Prose follows the house style: full sentences, no em dashes, no emoji.

## Known gaps

`TimingStats`, `Timeline`, and `render.decode_ms` have no producer, so the
overlay's timing panel and timeline read zero. `render.fps` records a constant
rather than a measured rate.

The upgrade half of adaptive switching commits rather than probes: `probe_duration`,
`probe_cooldown`, and `loss_probe_abort` are inert. See
[docs/architecture/adaptive.md](docs/architecture/adaptive.md).

`SyncMode::Unmanaged` does no pacing at all, despite what its doc comment says.

`iroh-moq` has no tests.

## Where testing happens

Linux on Intel Meteor Lake is the day-to-day platform. macOS builds in CI and has
been run by hand. Android and the Raspberry Pi have been tested on device.
Windows and iOS have never been built here. See
[docs/platforms.md](docs/platforms.md).
