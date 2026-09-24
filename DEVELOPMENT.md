# Development guide

Working notes for contributors. [README.md](README.md) has the project overview
and the quick start, and [docs/](docs/index.md) has the architecture and the
guides.

## Workspace

| Crate | Role |
|---|---|
| `iroh-live` | `Live`, `Call`, `Subscription`. Depends on `iroh-live-media` and `iroh-moq` |
| `iroh-moq` | MoQ transport over iroh: the node origin, sessions, ALPN negotiation, tickets, endpoint setup |
| `iroh-rooms` | Gossip rooms. No media dependency |
| `iroh-live-media` | Sources, broadcasts, and players over moq-video and moq-audio. No iroh dependency |
| `iroh-live-egui` | egui widget and debug overlay |
| `iroh-live-media-android` | Camera2 push bridge and EGL renderer |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

Demos live in `demos/`: `android` and `pi-zero`. The shortest Pi publisher is
an example instead, `iroh-live/examples/publish-pi.rs`.

Codecs, capture, decoding, and the wgpu renderer are upstream in `moq-video` and
`moq-audio`. Nothing here implements one. See
[docs/architecture/media-stack.md](docs/architecture/media-stack.md).

## The patch block

`Cargo.toml` carries a `[patch.crates-io]` block pointing the moq crates at
`Frando/moq@iroh-live`, a branch of five changes that have not reached a
release. Every pinned version matches what `moq-dev/moq@main` publishes, so
deleting the block is the whole revert once they land. `Cargo.lock` pins the
revision, so a clean clone and CI build the same tree; to work against a local
checkout instead, point the block at `../moq/rs/<crate>` and leave it
uncommitted.

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

Everything below is exported from the root of `iroh_live_media`.

Sources are values that are already open:

- `VideoSource::capture(config).await` opens a camera or screen and returns once
  it produced a frame; `test_pattern`, `push`, `spawn`, and `rpicam` cover
  generated, application-made, thread-bound, and Raspberry Pi frames.
  `EncodedVideoSource::annex_b` and `rpicam` carry pre-encoded H.264.
- `AudioSource::microphone`, `file`, `tone`, `test_pattern`, and `push` do the
  same for sound. `MicrophoneConfig::with_echo_cancellation(&output)` attaches
  the canceller for one `AudioOutput`.
- `VideoSource::frames()` is the local preview: a `VideoFrames` handle onto the
  captured pictures that costs no encode.

Publishing:

- `LocalBroadcast` owns a `moq_net::broadcast::Producer`, the catalog, and a
  media clock, with one video slot and one audio slot.
- `set_video(source, VideoEncoding::ladder([...]))` encodes one source into
  every rendition of a ladder, each rendition encoding only while somebody
  watches it. `set_encoded_video` and `set_audio` fill the other slots, and
  `status()` and `stats()` report per rendition.

Subscribing:

- `RemoteBroadcast` reads the catalog and holds the subscription;
  `with_network` attaches the link signals adaptation reads.
- `RemoteBroadcast::play(PlayerConfig)` returns a `Player`, which owns its
  decoders, its playout clock, its rendition choice, and its stats. Two players
  of one broadcast cannot interfere.
- `Player::video()` returns a `VideoFrames` handle: `next().await` waits for a
  newer picture and `try_next()` polls without blocking. Every handle keeps its
  own cursor.
- `Player::set_rendition(RenditionMode)` switches between `Auto`, `Pinned`, and
  `Off`. Audio plays through the `AudioOutput` the config names, and a config
  without one does not subscribe to audio at all.

Transport, in `iroh_moq`: `Moq::publish(path)` returns a producer synchronously
and announces it node-wide. `MoqSession::subscribe(path)` waits for the peer's
announce. `MoqSession::conn()` is the iroh `Connection` behind it.

## Threading

Codecs run on their own threads inside `moq_video::encode::Sink` and
`moq_video::decode::Sink`. Sources are the threads this repository spawns: every
source runs on a named thread of its own for its whole life, since some
platform capture objects are not `Send`. A capture device gets a thread with a
current-thread runtime (`local_task`), the test pattern and tones draw on plain
threads, the audio file reader decodes with symphonia on one, and
`VideoSource::spawn` hands the same arrangement to application code. Only
frames cross, into a latest-wins slot for video and a bounded fan-out for PCM.

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
- Bounded channels only. Frames to a renderer go through `VideoFrames`, a
  single-slot latest-wins stream with a cursor per handle, not a queue.
- `tracing_subscriber::fmt::init()` for setup: it respects `RUST_LOG` with no
  `EnvFilter` boilerplate. Use `throttled-tracing` for anything per-frame, and
  structured fields rather than string interpolation.
- Rust doc comments follow RFC 1574: third-person declarative sentences starting
  with a verb, no headings in item docs, types linked with `[`Type`]`.
- Prose follows the house style: full sentences, no em dashes, no emoji.

## Known gaps

Adaptation compares the publisher's delivery estimate against each rung's
*advertised* bitrate, which is a ceiling handed to the encoder rather than what
it sends. The fit ratio of one half absorbs that and the overshoot of a BBR
estimate together, but it is a figure measured against openh264 and one
patchbay lab, not against a hardware encoder or a real mobile link. See
[docs/architecture/adaptive.md](docs/architecture/adaptive.md).

Loss on a subscriber is measured over its own packets, which are mostly
acknowledgements, so the loss thresholds see the media direction only as far
as both directions are impaired alike.

## Where testing happens

Linux on Intel Meteor Lake is the day-to-day platform. macOS builds in CI and has
been run by hand. Android and the Raspberry Pi have been tested on device.
Windows and iOS have never been built here. See
[docs/platforms.md](docs/platforms.md).
