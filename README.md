# iroh-live

> **Early tech preview.** Several parts are unfinished. Windows builds in CI
> but has never been run, on-device testing has been limited, rooms are being
> redesigned, and the relay has no authentication. Expect frequent API changes.

> This repo currently depends on a [branch of upstream moq](https://github.com/Frando/moq/tree/iroh-live). We are in the process of upstreaming everything in here, but it might take a while.

Real-time audio and video over [iroh](https://github.com/n0-computer/iroh),
written in Rust. Connections are peer-to-peer by default, with no media server
in the middle, and an optional relay bridges to browsers over WebTransport. The
wire protocol is [Media over QUIC](https://moq.dev/), where every video rendition
and audio track travels as its own set of QUIC streams, so a dropped video packet
never delays audio.

Capture, encoding, decoding, and GPU rendering come from
[moq-video and moq-audio](https://doc.moq.dev/lib/rs/) upstream. What this
repository adds is an iroh transport, simulcast and adaptive rendition switching,
a shared playout clock, and the application layer over all of it.

## Quick start

```sh
cargo install --path iroh-live-cli

# Terminal 1: publish camera and microphone, print a ticket and a QR code
irl publish

# Terminal 2, or another machine
irl watch <TICKET>
```

No camera? `irl publish --test-source` publishes a generated pattern and a tone.

To reach subscribers that cannot dial this node directly, connect to a relay.
Everything the node publishes is announced over that session:

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

Full flag reference: [docs/cli.md](docs/cli.md).

## Using iroh-live in Rust

The workspace patches the moq crates to `Frando/moq@iroh-live`, which carries
the changes behind eleven open pull requests to moq and one to moq-vaapi. Until
they land in releases, a downstream user needs to copy the `[patch.crates-io]`
block from [Cargo.toml](Cargo.toml).

Publish a camera and a microphone:

```rust
use iroh_live::{Live, media::{audio, video}, ticket::LiveTicket};

let live = Live::from_env().await?.with_router().spawn();
let broadcast = live.publish("hello")?;

broadcast.video().set(video::capture::Config::default())?;
broadcast.audio().set(audio::capture::Config::default());

println!("{}", LiveTicket::new(live.endpoint().id(), "hello"));
```

Subscribe and read decoded frames:

```rust
let live = Live::from_env().await?.spawn();
let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name).await?;
let tracks = sub.media().await;

if let Some(video) = tracks.video {
    while let Some(frame) = video.recv().await {
        // hand `frame` to a renderer
    }
}
```

`iroh-live/examples/publish.rs` is the compilable version of the first, including
a simulcast ladder. More in [docs/guide/index.md](docs/guide/index.md).

## Workspace

| Crate | Description |
|---|---|
| [`iroh-live`](iroh-live) | `Live`, `Call`, `Subscription`, and tickets |
| [`iroh-moq`](iroh-moq) | MoQ transport over iroh: the node origin, sessions, and ALPN negotiation |
| [`iroh-rooms`](iroh-rooms) | Gossip rooms. Media-free, and being redesigned onto moq's announce bus |
| [`moq-media`](moq-media) | Publish and subscribe plumbing over moq-video and moq-audio. No iroh dependency |
| [`moq-media-egui`](moq-media-egui) | An egui widget over the texture `moq_video::render` returns, plus the debug overlay |
| [`moq-media-android`](moq-media-android) | The Camera2 push bridge and the EGL renderer for Android |
| [`iroh-live-cli`](iroh-live-cli) | The `irl` binary |
| [`iroh-live-relay`](iroh-live-relay) | Relay server bridging iroh publishers to browsers. No authentication yet |

## Demos

- [`demos/android`](demos/android): a Kotlin and Rust application with two-way
  calling, hardware H.264, and zero-copy EGL rendering.
- [`demos/pi-zero`](demos/pi-zero): a Raspberry Pi camera stream with an e-paper
  QR display and a GLES2 watch mode.
- [`iroh-live/examples/publish-pi.rs`](iroh-live/examples/publish-pi.rs): the
  same publisher in 35 lines, with no flags.

## Platform support

| Platform | State |
|---|---|
| Linux, Intel and AMD | Primary target. VAAPI decode behind the `vaapi` feature, checked pixel-exact against a software decoder, handing its pictures to the renderer without a copy |
| Linux, NVIDIA | NVENC and NVDEC behind the `nvidia` feature. Untested here |
| macOS | Builds in CI. VideoToolbox and ScreenCaptureKit from upstream. Lightly tested |
| Android | Tested on device, two-way audio and video |
| Raspberry Pi | Tested on a Pi Zero 2 W and a Pi 4. Publishes pre-encoded H.264 through `rpicam-vid`, or raw pictures with `--video rpicam:raw`. The V4L2 hardware encoder and decoder behind `v4l2` both run on a Pi 4 |
| Windows | Builds in CI, and the release workflow ships an x86-64 binary. Never run here |
| iOS | Upstream has the backends. Never built here |

Details, including what was lost when the in-house media stack was replaced:
[docs/platforms.md](docs/platforms.md).

## Building

```sh
cargo build --workspace
```

Codecs need no system libraries. Device access and graphics do:

```sh
# Debian and Ubuntu
sudo apt install libasound2-dev libpipewire-0.3-dev libclang-dev \
                 libegl-dev libgbm-dev libdrm-dev libfontconfig-dev libva-dev nasm

# Arch
sudo pacman -S alsa-lib pipewire clang mesa fontconfig libva nasm
```

macOS needs `brew install libtool automake`.

### Feature flags

Every codec compiles unconditionally upstream, so there are no per-codec flags.
What is left gates a build dependency or a graphics stack. `moq-media` defines
them and the other crates pass them through.

| Flag | Default in `iroh-live` | What it adds |
|---|---|---|
| `capture` | yes | Camera, screen, and microphone devices |
| `render` | yes | The wgpu renderer |
| `playback` | no | Speaker output |
| `aec` | no | Echo cancellation. Implies `capture` and `playback` |
| `pipewire` | no | Linux screen capture. Links `libpipewire-0.3` |
| `sound-server` | yes | Reaches audio devices through PipeWire or PulseAudio |
| `vaapi` | no | Intel and AMD hardware H.264 encode and decode, the decoder handing its pictures over without a copy |
| `nvidia` | no | NVIDIA hardware encode and decode |
| `v4l2` | no | The V4L2 hardware H.264 codecs on ARM SoCs. Encoder and decoder both exercised on a Raspberry Pi 4 |
| `rpicam` | no | The Raspberry Pi camera, through `rpicam-vid`. Linux only |

`moq-media` adds one of its own, `test-source`, for generated video and audio.
`iroh-live-cli` turns on `playback` as well.

### Cross-compiling for aarch64

```sh
cargo make cross-sysroot-aarch64                              # once
cargo make cross-build-aarch64 -- -p iroh-live-cli --release
```

This uses `cargo-zigbuild` against a Debian Bookworm sysroot assembled without
sudo. A Docker path exists for hosts that cannot install zig. See
[cross/README.md](cross/README.md).

## Contributing

[DEVELOPMENT.md](DEVELOPMENT.md) covers the workspace layout, the build and test
workflow, and the conventions. Issues live in the
[tracker](https://github.com/n0-computer/iroh-live/issues).

```sh
cargo make check-all   # check and clippy across three feature sets, then fmt
cargo make test        # cargo nextest across the workspace
```

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
