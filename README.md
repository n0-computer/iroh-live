# iroh-live

> **Early tech preview.** Expect frequent API changes. Windows has never been
> run, on-device testing has been limited, and the relay accepts anyone.

Real-time audio and video over [iroh](https://github.com/n0-computer/iroh),
written in Rust. Connections are peer-to-peer, with no media server in the
middle. An optional relay carries broadcasts to peers that cannot connect
directly, and to browsers over WebTransport. The wire protocol is
[Media over QUIC](https://moq.dev/): every video rendition and every audio track
travels on its own QUIC streams, so a lost video packet never delays audio.

Capture, codecs, and GPU rendering come from
[moq-video and moq-audio](https://doc.moq.dev/lib/rs/). This repository adds
the iroh transport, simulcast with adaptive rendition switching, a playout
clock that keeps audio and video in step, and the application layer on top.

## Quick start

```sh
cargo install --path iroh-live-cli

# Terminal 1: publish camera and microphone, print a ticket and a QR code
irl publish

# Terminal 2, or another machine
irl watch <TICKET>
```

Without a camera, `irl publish --test-source` publishes a generated pattern and
a beeping tone.

To reach browsers, attach to a relay. The
[relay guide](docs/guide/browser-relay.md) explains how.

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

All commands and flags are in [docs/cli.md](docs/cli.md).

## Using iroh-live in Rust

The [`iroh-live`](iroh-live) crate is the library: a `Live` node publishes
broadcasts and plays other nodes' broadcasts, and its player picks a rendition
from the ladder as the link allows. [Getting started](docs/guide/index.md)
walks through a publisher and a subscriber, and
[`iroh-live/examples/publish.rs`](iroh-live/examples/publish.rs) is a complete
publisher.

The workspace patches the moq crates with one fix for building `moq-video` on
Windows, and [Cargo.toml](Cargo.toml) says why. To build for Windows
downstream, copy its `[patch.crates-io]` block.

## Crates

| Crate | Description |
|---|---|
| [`iroh-live`](iroh-live) | `Live`: publish and subscribe at `live/<endpoint id>/<name>`, `BroadcastTicket`, endpoint setup, and re-exports of the crates below |
| [`iroh-moq`](iroh-moq) | MoQ over iroh: one route table fed by direct sessions and relay links, publications, and grants |
| [`iroh-live-rooms`](iroh-live-rooms) | Rooms: gossip membership and members-only broadcasts. No media dependency |
| [`iroh-live-media`](iroh-live-media) | Sources, broadcasts, and players over moq-video and moq-audio. No iroh dependency |
| [`iroh-live-egui`](iroh-live-egui) | egui video views and the debug overlay |
| [`iroh-live-media-android`](iroh-live-media-android) | The Android EGL renderer and JNI handle helpers |
| [`iroh-live-cli`](iroh-live-cli) | The `irl` binary |
| [`iroh-live-relay`](iroh-live-relay) | Relay server that serves iroh-live broadcasts to browsers |

## Demos

- [`demos/android`](demos/android): an Android app that watches, publishes, and
  calls, with hardware H.264 and zero-copy EGL rendering.
- [`demos/pi-zero`](demos/pi-zero): a Raspberry Pi camera publisher with an
  e-paper QR display and a GLES2 viewer.
- [`iroh-live/examples/publish-pi.rs`](iroh-live/examples/publish-pi.rs): the
  shortest Pi publisher, with no flags.

## Platform support

| Platform | State |
|---|---|
| Linux, Intel and AMD | Primary target. VA-API encode and decode behind the `vaapi` feature |
| Linux, NVIDIA | NVENC and NVDEC behind the `nvidia` feature. Untested here |
| macOS | Builds in CI. VideoToolbox and ScreenCaptureKit from upstream. Lightly tested |
| Android | Tested on device, two-way audio and video |
| Raspberry Pi | Tested on a Pi Zero 2 W and a Pi 4. Publishes the camera's own H.264 through `rpicam-vid`, or raw pictures with `--video rpicam:raw`. The V4L2 codecs behind `v4l2` run on a Pi 4 |
| Windows | The release workflow builds an x86-64 binary. Never run here |
| iOS | Upstream has the backends. Never built here |

Details are in [docs/platforms.md](docs/platforms.md).

## Building

```sh
cargo build --workspace
```

Codecs need no system libraries. Device access and graphics do, and
[Getting started](docs/guide/index.md#system-dependencies) lists them.

Every codec compiles upstream, so there are no per-codec features. The
features gate devices and graphics. `iroh-live-media` defines them, and its
[README](iroh-live-media/README.md#feature-flags) lists them. `iroh-live`
passes them through and turns on `capture`, `render`, and `sound-server` by
default. `irl` also turns on `playback` and `aec`. `iroh-live` adds `media`
(default), which turns the media stack on, and each device feature implies
it. It also adds `rooms`, which re-exports `iroh-live-rooms`.

### Cross-compiling for aarch64

```sh
cargo make cross-sysroot-aarch64                              # once
cargo make cross-build-aarch64 -- -p iroh-live-cli --release
```

This uses `cargo-zigbuild` against a Debian Bookworm sysroot, built without
sudo. There is also a Docker path. See [cross/README.md](cross/README.md).

## Contributing

[DEVELOPMENT.md](DEVELOPMENT.md) covers the build, the tests, and the
conventions. Issues go in the
[tracker](https://github.com/n0-computer/iroh-live/issues).

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
