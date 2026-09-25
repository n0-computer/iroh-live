# Development guide

[README.md](README.md) lists the crates and has the quick start.
[docs/](docs/index.md) has the architecture and the guides.

Codecs, capture, decoding, and the wgpu renderer live upstream in `moq-video`
and `moq-audio`. See
[docs/architecture/media-stack.md](docs/architecture/media-stack.md).

## Build and test

```sh
cargo build --workspace                 # default features
cargo build --workspace --all-features  # everything
```

The [cargo-make](https://crates.io/crates/cargo-make) tasks in
[Makefile.toml](Makefile.toml) match CI:

| Task | What it runs |
|---|---|
| `cargo make check-all` | `cargo check` and `cargo clippy -D warnings` with all features, no default features, and default features, then `format-check` |
| `cargo make check` | Only the three `cargo check` runs |
| `cargo make clippy` | Only the three clippy runs |
| `cargo make format` | `cargo fmt` with the import grouping the project uses |
| `cargo make test` | `cargo nextest run --workspace` |
| `cargo make test-patchbay` | The patchbay network simulation tests in `iroh-live`, ignored ones included. Linux only |
| `cargo make test-e2e` | The Playwright browser suite in `tests/e2e-browser`, after building the relay, the CLI, and the `subscribe_test` example |
| `cargo make test-full` | `check-all`, `test`, and `test-e2e` |

Run `cargo make check-all` before you commit code. The `--no-default-features`
run is where feature-gated mistakes show up. The browser suite needs
`npm ci` and `npx playwright install chromium` in `tests/e2e-browser` once.

A test that has to see a rendition switch within its timeout shortens the
timers in `PlayerConfig::adaptation`, as the patchbay suite does.

Cross-compiling for aarch64 is `cargo make cross-sysroot-aarch64` once, then
`cargo make cross-build-aarch64 -- <cargo args>`. See
[cross/README.md](cross/README.md).

## The patch block

`Cargo.toml` has a `[patch.crates-io]` block that points every moq crate the
build uses at `Frando/moq@iroh-live-5`. That branch is the commit the released
versions (`moq-video` 0.0.26, `moq-net` 0.3.0, and their siblings) were cut
from, plus one fix: `moq-video` 0.0.26 does not compile for Windows with
`capture`. All crates of the moq workspace are patched together, so the build
never mixes a git copy of one with a crates.io copy of another. Delete the block
once a `moq-video` release has the fix.

`Cargo.lock` pins the revision. To work against a local moq checkout, point the
block at `../moq/rs/<crate>` and do not commit that change.

## Commits

Use conventional prefixes: `feat:`, `fix:`, `test:`, `refactor:`, `perf:`,
`ci:`, `docs:`, `chore:`. The message says why first, then what changed. Keep
each commit small, and leave the workspace compiling after every one. New
behaviour needs a test.

## Key types

In `iroh_live_media`:

- `VideoSource` is an open source of pictures: `capture`, `test_pattern`,
  `push`, `spawn` (application code on a thread of its own), and `rpicam`.
  `VideoSource::frames()` is a local preview that costs no encode.
  `EncodedVideoSource` (`annex_b`, `rpicam`) carries H.264 that is already
  encoded.
- `AudioSource` is the same for sound: `microphone`, `file`, `tone`,
  `test_pattern`, and `push`. `microphone` only checks that the device exists.
  The device opens when a broadcast first has a listener, and a failure then
  shows in `LocalBroadcast::status()`. `MicrophoneConfig::echo_reference`
  names the `AudioOutput` to cancel from the microphone.
- `LocalBroadcast` has a catalog, a media clock, one video slot, and one audio
  slot. `set_video(source, VideoEncoding::ladder([...]))` encodes one source
  into every rendition of a ladder, and a rendition only encodes while someone
  watches it. `set_encoded_video` and `set_audio` fill the other slots.
  `status()` and `stats()` report per rendition.
- `RemoteBroadcast` reads the catalog of a subscribed broadcast. `from_origin`
  and `from_resolved` build one that follows a path in a route table, and
  `with_network` attaches the `NetworkSignals` that adaptation reads.
  `closed()` resolves about three seconds after the publisher ends the
  broadcast, once the route table has no route left.
- `RemoteBroadcast::play(PlayerConfig)` returns a `Player`. Each player has its
  own decoders, playout clock, rendition choice, and stats.
  `Player::set_rendition` takes a `RenditionMode`: `Auto`, `Pinned`, or `Off`.
  Audio plays through `PlayerConfig::audio`. With `None`, the player does not
  subscribe to audio.
- `Player::video()` returns `VideoFrames`: `next().await` waits for a newer
  picture, and `try_next()` polls. Every handle has its own cursor.

In `iroh_moq`: `Moq::publish(path, broadcast, audience)` publishes any
`Consume<broadcast::Consumer>` at a full path. `Moq::subscribe(path, reach)`
resolves a path in the route table and returns a `Subscription`.
`Session::subscribe(path)` resolves over one session only.
`MoqConfig::grant` gives each peer a `Grant`, which says what it may subscribe
to and publish. `Session::link()` and `RelayLink::link()` return the latest
`LinkSample` of the link's monitor, and `Subscription::link()` returns the
serving link with its sample.

In `iroh_live`: `Live::publish(name, broadcast)` publishes at
`live/<endpoint id>/<name>` to everyone, and `Live::ticket(name)` is the
`BroadcastTicket` for that path. `Live::subscribe(&ticket)` resolves the
ticket and returns a `RemoteBroadcast` without waiting for the catalog.
`Live::remote_broadcast` wraps a `Subscription` from a room or from
`Moq::subscribe` the same way. `iroh_live::grant` is the grant a live node
gives its peers. `iroh_live::Error` is `Transport(iroh_moq::Error)` or
`Media(iroh_live_media::Error)`.

## Threading

Codecs run on their own threads inside `moq_video::encode::Sink` and
`moq_video::decode::Sink`. Every source runs on a named thread of its own,
since some platform capture objects are not `Send`. A capture device gets a
thread with a current-thread runtime (`local_task`), and `VideoSource::spawn`
gives application code the same. Only frames cross threads: video through a
latest-wins slot, PCM through a bounded fan-out.

`moq_video::decode::Consumer::read` is not cancel safe. Never poll it from a
`select!` arm. Each video decoder has a task that reads it in a plain loop and
forwards over a bounded channel. See
[docs/architecture/subscribe.md](docs/architecture/subscribe.md).

Networking, adaptation, and rooms run as tokio tasks. Audio output is a cpal
callback on a real-time thread that `moq_audio::playback::Engine` owns.

## Conventions

- `n0_watcher::Watchable` and `Direct<T>` for state that changes, not
  `tokio::sync::watch`.
- `CancellationToken` for shutdown, `AbortOnDropHandle` to tie a task to a
  handle.
- Bounded channels only. Frames to a renderer go through `VideoFrames`, not a
  queue.
- `tracing` with structured fields. Use `throttled-tracing` for anything that
  logs per frame.
- Doc comments follow RFC 1574: third-person sentences that start with a verb,
  no headings in item docs, and types linked as ``[`Type`]``.
- Prose uses full sentences, no em dashes, and no emoji.

## Known gaps

Adaptation compares the delivery estimate against each rung's advertised
bitrate, which is a ceiling for the encoder rather than what it sends.
`Adaptation::fit_ratio` (1.25) covers that gap. It was measured with openh264,
VA-API, and one patchbay lab only. Loss on a subscriber is measured over its
own packets, which are mostly acknowledgements. See
[docs/architecture/adaptive.md](docs/architecture/adaptive.md).
