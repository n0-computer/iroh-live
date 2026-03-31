# Cross-compilation for aarch64

Uses `cargo-zigbuild` with Zig as the linker and a Debian Bookworm aarch64
sysroot for native headers/libraries. No Docker required.

## Prerequisites

```sh
# Arch Linux
sudo pacman -S zig mmdebstrap aarch64-linux-gnu-gcc
cargo install cargo-zigbuild

# Debian/Ubuntu
sudo apt install zig mmdebstrap gcc-aarch64-linux-gnu g++-aarch64-linux-gnu qemu-user-static
cargo install cargo-zigbuild

# Rust target
rustup target add aarch64-unknown-linux-gnu
```

## Create the sysroot (one-time)

```sh
./cross/create-sysroot.sh
```

This bootstraps a minimal Debian Bookworm aarch64 filesystem under
`cross/sysroot-aarch64/` containing all the development headers and
pkg-config files. Takes 1-2 minutes. The sysroot is cached by CI.

## Build

```sh
# irl CLI (release)
cargo make zig-build-irl

# Check the whole workspace
cargo make zig-check-aarch64

# Pi Zero demo (from demos/pi-zero/)
cd demos/pi-zero && cargo make zig-build
cd demos/pi-zero && cargo make zig-deploy

# Manual invocation with arbitrary arguments
./cross/zigbuild.sh -p iroh-live-cli --release
./cross/zigbuild.sh -p pi-zero-demo --release --features raspberry-pi

# Custom sysroot location
SYSROOT=/path/to/sysroot ./cross/zigbuild.sh -p pi-zero-demo --release
```

## How it works

1. `create-sysroot.sh` uses `mmdebstrap` to extract aarch64 packages from
   Debian Bookworm into a local directory (no chroot, no root needed).

2. `zigbuild.sh` sets `PKG_CONFIG_SYSROOT_DIR` and `PKG_CONFIG_PATH` so
   that `-sys` crates find the aarch64 `.pc` files and headers.

3. `cargo-zigbuild` uses Zig as the linker, targeting `aarch64-unknown-linux-gnu.2.36`
   (glibc 2.36 = Debian Bookworm = Raspberry Pi OS Bookworm).

4. For C compilation in build scripts (aws-lc-sys, etc.), Zig is used as
   CC/CXX with `--sysroot` pointing to the Debian sysroot.

## Libraries available in the sysroot

The Bookworm sysroot includes:

- `libcamera-dev` -- Raspberry Pi camera stack
- `libpipewire-0.3-dev` -- PipeWire screen/camera capture
- `libva-dev`, `libdrm-dev`, `libgbm-dev` -- GPU/display
- `libasound2-dev`, `libopus-dev` -- audio
- `libv4l-dev` -- V4L2 camera/encoder
- `libssl-dev` -- TLS
- `libx11-dev`, `libxkbcommon-dev`, `libwayland-dev` -- windowing

## CI

`.github/workflows/ci-aarch64-zig.yml` builds the `irl` CLI binary for
aarch64 in release mode. Pre-built binaries are uploaded to the
`rolling-release` GitHub release on push to main.

## Binary compatibility

The resulting binaries target aarch64 glibc (not musl) and are compatible
with Raspberry Pi OS Bookworm (Debian 12, glibc 2.36).
