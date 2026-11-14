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
[Cross-compile for aarch64](../how-to/cross-compile.md).

## Use the library

iroh-live depends on the unreleased `main` branch of
[moq-dev/moq](https://github.com/moq-dev/moq), pinned by git rev in the root
`Cargo.toml`. The pinned rev is `6aa4f9c1c` (2026-11-13). The moq crates on
crates.io come from moq's `release` branch, which lags `main` by weeks, and
iroh-live uses APIs that exist only on `main`. A crate with git dependencies
cannot be published to crates.io, so none of the iroh-live crates is there
yet. Depend on them through git:

```toml
[dependencies]
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full"] }
```

Cargo builds moq from the same pinned rev. A crate of yours that also uses a
moq crate directly, such as `moq-mux`, must name the same rev, or Cargo
builds two copies whose types do not match:

```toml
moq-mux = { git = "https://github.com/moq-dev/moq", rev = "6aa4f9c1c" }
```

`iroh-live` has no default features. `full` turns on the media stack with
device capture, GPU rendering, the sound server and recording. Without
`media`, the crate is the node, tickets and layouts, which is what a relay
needs. The [features reference](../reference/features.md) lists every
feature of every crate.

The same system libraries as for `irl` are needed for `capture`, `pipewire`
and `render`. A build without those features needs none of them.

When a moq release contains the APIs iroh-live uses, crates.io versions will
replace the git pins. Breaking changes are listed in
[Migrating](../migration.md).

## Next steps

- [Quickstart: CLI](cli-quickstart.md)
- [Quickstart: library](library-quickstart.md)
