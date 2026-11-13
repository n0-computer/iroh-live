# Development guide

[README.md](README.md) has the quick start. The book in
[docs/](docs/SUMMARY.md) has the guides, the reference and the architecture,
and the crate docs describe the main types.

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
| `cargo make check-all` | `cargo check` with all features, no default features, default features, the relay and the facade without media, then `cargo clippy -D warnings` with the first three, then `format-check` |
| `cargo make check` | Only the five `cargo check` runs |
| `cargo make clippy` | Only the three clippy runs |
| `cargo make format` | `cargo fmt` with the import grouping the project uses |
| `cargo make test` | `cargo nextest run --workspace` |
| `cargo make test-patchbay` | The patchbay network simulation tests in `iroh-live`, ignored ones included. Linux only |
| `cargo make test-e2e` | The Playwright browser suite in `tests/e2e-browser`, after building the relay, the CLI, and the `subscribe_test` example |
| `cargo make test-full` | `check-all`, `test`, and `test-e2e` |

Run `cargo make check-all` before you commit code. The `--no-default-features`
run is where feature-gated mistakes show up. The browser suite needs
`npm ci` and `npx playwright install chromium` in `tests/e2e-browser` once.

For docs, run `cargo test --doc -p iroh-live --all-features`, which compiles
the guide's Rust programs, and `python3 scripts/check-links.py`. See
[docs/contributing.md](docs/contributing.md).

A test that has to see a rendition switch within its timeout shortens the
timers in `PlayerConfig::adaptation`, as the patchbay suite does.

`iroh-live-node` and `moq-ext-player` have a `test-util` feature with the
helpers their dependents' tests share: in-process endpoints that find each
other, `step` and `eventually` waits, and the source-switch helpers. Each
crate's tests turn it on through a dev-dependency on the crate itself. It is
hidden from the docs and is not a public API.

Some examples need a feature: `cargo run -p iroh-live-node --features rcan
--example delegated_publisher` runs the rcan example, and `cargo make
check-all` builds every example with all features.

Cross-compiling for aarch64 is `cargo make cross-sysroot-aarch64` once, then
`cargo make cross-build-aarch64 -- <cargo args>`. See
[cross/README.md](cross/README.md).

## Working against a local moq checkout

The workspace builds the moq crates from the revision of moq's `main` branch
pinned in the root `Cargo.toml`. To try a local checkout, add a `[patch]` block
for the moq repository that points at `../moq/rs/<crate>`. Do not commit it.

To bump the pin, change the rev of every moq crate in the root `Cargo.toml` at
once, and update the rev and date in
[docs/getting-started/install.md](docs/getting-started/install.md#use-the-library).

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
  no headings in item docs besides `# Errors`, `# Panics`, `# Examples` and
  `# Safety`, and types linked as ``[`Type`]``.
- Prose uses full sentences, no em dashes, no semicolons, and no emoji.

## Known gaps

Adaptation compares the delivery estimate against each rung's advertised
bitrate, which is a ceiling for the encoder rather than what it sends.
`Adaptation::fit_ratio` (1.25) covers that gap. It was measured with openh264,
VA-API, and one patchbay lab only. Loss on a subscriber is measured over its
own packets, which are mostly acknowledgements. See
[docs/architecture/adaptive.md](docs/architecture/adaptive.md).
