# Install

The `irl` command line tool is the quickest way to try iroh-live. The
library is a git dependency.

## Download `irl`

Every push to `main` builds `irl` and attaches it to the
[`rolling-release`](https://github.com/n0-computer/iroh-live/releases/tag/rolling-release)
release on GitHub:

| Asset | Platform |
|---|---|
| `irl-linux-x86_64` | Linux x86-64, glibc 2.39 or newer (Ubuntu 24.04) |
| `irl-linux-aarch64` | Linux aarch64, glibc 2.36 or newer (Raspberry Pi OS Bookworm) |
| `irl-macos-aarch64` | macOS 14 or newer on Apple silicon |
| `irl-windows-x86_64.exe` | Windows 10 or newer, x86-64. Never tested |
| `iroh-live-demo-arm64-v8a.apk` | The Android demo app |

`SHA256SUMS` lists the checksums. The tag moves with every push, so the
assets are development builds. On Linux, make the binary executable with
`chmod +x` and install the runtime libraries of the next section, without the
`-dev` suffix where your distribution splits them.

## Build `irl` from source

Building needs a stable Rust toolchain and some system libraries for device
access and graphics. The codecs need none.

```sh
# Debian and Ubuntu
sudo apt install libasound2-dev libpipewire-0.3-dev libclang-dev \
                 libegl-dev libgbm-dev libdrm-dev libfontconfig-dev libva-dev nasm

# Arch
sudo pacman -S alsa-lib pipewire clang mesa fontconfig libva nasm
```

macOS needs `libtool` and `automake` from Homebrew.

Then install from a checkout:

```sh
git clone https://github.com/n0-computer/iroh-live
cd iroh-live
cargo install --locked --path iroh-live-cli
```

`irl` turns on every feature by default. The Linux-only ones compile to
nothing on macOS and Windows. To cross-compile for a Raspberry Pi, see
[Publish from a Raspberry Pi](../how-to/raspberry-pi.md#cross-compiling).

## Build the iroh-live relay

The relay has no prebuilt binary. Its build script bundles the web viewer it
serves to browsers, so building it also needs [Node.js](https://nodejs.org/)
and `npm` on `PATH`. The first build runs `npm install` in
`iroh-live-relay/web`, which downloads the viewer's dependencies.

```sh
cargo install --locked --path iroh-live-relay
```

A workspace build, such as `cargo build --workspace`, builds the relay too and
needs `npm` as well. [Run the iroh-live relay](../how-to/run-relay.md)
continues from here.

## Use the library

iroh-live depends on the unreleased `main` branch of
[moq-dev/moq](https://github.com/moq-dev/moq), pinned to rev `4a6f52248`
(2026-11-18) in the root `Cargo.toml`. iroh-live uses APIs that are not yet
in a moq release. A crate with git dependencies cannot be published, so the
iroh-live crates are not on crates.io yet. Depend on them through git:

```toml
[dependencies]
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full"] }
```

Cargo builds moq from the same pinned rev. A crate of yours that also uses a
moq crate directly, such as `moq-mux`, must name the same rev, or Cargo
builds two copies whose types do not match:

```toml
moq-mux = { git = "https://github.com/moq-dev/moq", rev = "4a6f52248" }
```

`iroh-live` has no default features. `full` turns on the media stack with
device capture, speaker playback, GPU rendering, the sound server and
recording. It does not include audio files (`file`): add it when you need it.
Without `media`, the crate is the node, tickets and layouts, which is what a
relay needs. The [features reference](../reference/features.md) lists every
feature of every crate.

The same system libraries as for `irl` are needed for `capture`, `pipewire`
and `render`. A build without those features needs none of them.

When a moq release contains the APIs iroh-live uses, crates.io versions will
replace the git pins. [Migrating from the previous
release](../migration.md) lists the breaking changes.

## Where to go next

- [Quickstart: CLI](cli-quickstart.md)
- [Quickstart: library](library-quickstart.md)
