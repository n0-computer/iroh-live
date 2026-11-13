# iroh-live

Real-time audio and video over [iroh](https://github.com/n0-computer/iroh),
written in Rust. Peers connect directly, with no media server in the middle.
The wire protocol is [Media over QUIC](https://moq.dev/): every video
rendition and every audio track travels on its own QUIC streams, so a lost
video packet never delays audio. Each viewer's player picks the rendition its
link can carry, and keeps audio and video in step.

> **Early tech preview.** Expect frequent API changes. Linux is the main
> target, Windows has never been run, and on-device testing has been
> limited. iroh-live depends on moq's unreleased `main` branch by git rev, so
> it is not on crates.io. The iroh-live relay lets anyone read every
> broadcast on it. `--auth-key` does not change that: it changes only what
> sessions with a token may do. See
> [Run the iroh-live relay](docs/how-to/run-relay.md).

## Quick start

Download `irl` from the
[rolling release](https://github.com/n0-computer/iroh-live/releases/tag/rolling-release),
or build it:

```sh
cargo install --locked --path iroh-live-cli

# Publish the camera and microphone. Prints a ticket and a QR code.
irl publish

# On another machine
irl watch <TICKET>
```

Without a camera, `irl publish --test-source` publishes a test pattern and a
beeping tone. To open the stream in a browser, run the iroh-live relay and
open `http://localhost:4443/?name=<TICKET>`, as the
[CLI quickstart](docs/getting-started/cli-quickstart.md#open-it-in-a-browser)
shows.

## Using iroh-live in Rust

```toml
[dependencies]
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full"] }
```

The [library quickstart](docs/getting-started/library-quickstart.md) has a
complete publisher and viewer.

## Documentation

The [book in `docs/`](docs/SUMMARY.md) has:

- **Getting started:** [install](docs/getting-started/install.md),
  [CLI quickstart](docs/getting-started/cli-quickstart.md),
  [library quickstart](docs/getting-started/library-quickstart.md).
- **Concepts:** [how it fits together](docs/concepts/overview.md), nodes,
  paths, routes, access control and relays.
- **How-to guides:** publish, watch, call, rooms, browsers, recording,
  access, tuning, Android and Raspberry Pi.
- **Reference:** the [`irl` CLI](docs/reference/cli.md),
  [Cargo features](docs/reference/features.md),
  [environment variables](docs/reference/environment.md),
  [the relay](docs/reference/relay.md) and
  [platform support](docs/reference/platforms.md).
- [Architecture](docs/architecture/index.md),
  [troubleshooting](docs/troubleshooting.md),
  [glossary](docs/glossary.md), and
  [migrating from the previous release](docs/migration.md).

## Crates

[`iroh-live`](iroh-live) is the library most applications need.
[`iroh-live-cli`](iroh-live-cli) is the `irl` tool, and
[`iroh-live-relay`](iroh-live-relay) the relay for browsers. The
[crates reference](docs/reference/crates.md) lists the rest.

## Demos

- [`demos/android`](demos/android): an Android app that watches, publishes and
  calls, with hardware H.264 and zero-copy rendering.
- [`demos/pi-zero`](demos/pi-zero): a Raspberry Pi camera publisher with an
  e-paper QR display and a GLES2 viewer.

## Contributing

[DEVELOPMENT.md](DEVELOPMENT.md) covers the build, the tests and the
conventions, and [Contributing](docs/contributing.md) the docs. Issues go in
the [tracker](https://github.com/n0-computer/iroh-live/issues).

## License

Copyright 2025 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
