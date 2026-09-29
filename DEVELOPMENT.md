# Development guide

[README.md](README.md) lists the crates and has the quick start.
[docs/](docs/index.md) has the architecture and the guides, and the crate docs
describe the main types.

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

`Cargo.toml` patches the moq crates, and the comment above its
`[patch.crates-io]` block says why and when the block can go. `Cargo.lock` pins
the revision. To work against a local moq checkout, point the block at
`../moq/rs/<crate>` and do not commit that change.

## Commits

Use conventional prefixes: `feat:`, `fix:`, `test:`, `refactor:`, `perf:`,
`ci:`, `docs:`, `chore:`. The message says why first, then what changed. Keep
each commit small, and leave the workspace compiling after every one. New
behaviour needs a test.

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

The runtime conventions are in
[the architecture overview](docs/architecture/index.md#conventions). For code
and prose:

- `tracing` with structured fields. Use `throttled-tracing` for anything that
  logs per frame.
- Doc comments follow RFC 1574: third-person sentences that start with a verb,
  no headings in item docs besides `# Errors`, `# Panics` and `# Examples`,
  and types linked as ``[`Type`]``.
- Prose uses full sentences, no em dashes, and no emoji.

## Known gaps

Adaptation compares the delivery estimate against each rung's advertised
bitrate, which is a ceiling for the encoder rather than what it sends.
`Adaptation::fit_ratio` (1.25) covers that gap. It was measured with openh264,
VA-API, and one patchbay lab only. Loss on a subscriber is measured over its
own packets, which are mostly acknowledgements. See
[docs/architecture/adaptive.md](docs/architecture/adaptive.md).
