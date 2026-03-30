# Cross-compilation for aarch64

Two approaches are available. The zigbuild approach is preferred for new
development because it provides access to modern libraries (libcamera-dev,
PipeWire 0.3) that the cross-rs Docker image lacks.

## Approach 1: zigbuild + Debian sysroot (preferred)

Uses `cargo-zigbuild` with Zig as the linker and a Debian Bookworm aarch64
sysroot for native headers/libraries. No Docker required.

### Prerequisites

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

### Create the sysroot (one-time)

```sh
./cross/create-sysroot.sh
```

This bootstraps a minimal Debian Bookworm aarch64 filesystem under
`cross/sysroot-aarch64/` containing all the development headers and
pkg-config files. Takes 1-2 minutes. The sysroot is cached by CI.

### Build

```sh
# Individual packages
cargo make zig-build-pi-zero         # pi-zero-demo (release)
cargo make zig-build-irl             # irl CLI (release)

# Check the whole workspace
cargo make zig-check-aarch64

# Manual invocation with arbitrary arguments
./cross/zigbuild.sh -p pi-zero-demo --release --features raspberry-pi

# Custom sysroot location
SYSROOT=/path/to/sysroot ./cross/zigbuild.sh -p pi-zero-demo --release
```

### Deploy to Pi

```sh
cargo make zig-deploy-pi-zero                       # default: livepizero
PI_HOST=mypi cargo make zig-deploy-pi-zero          # custom host
```

### How it works

1. `create-sysroot.sh` uses `mmdebstrap` to extract aarch64 packages from
   Debian Bookworm into a local directory (no chroot, no root needed).

2. `zigbuild.sh` sets `PKG_CONFIG_SYSROOT_DIR` and `PKG_CONFIG_PATH` so
   that `-sys` crates find the aarch64 `.pc` files and headers.

3. `cargo-zigbuild` uses Zig as the linker, targeting `aarch64-unknown-linux-gnu.2.36`
   (glibc 2.36 = Debian Bookworm = Raspberry Pi OS Bookworm).

4. For C compilation in build scripts (aws-lc-sys, etc.), `aarch64-linux-gnu-gcc`
   is used with `--sysroot` pointing to the Debian sysroot. If GCC is not
   available, Zig is used as CC instead.

### Libraries available in the sysroot

Unlike the cross-rs Docker image (Ubuntu 20.04), the Bookworm sysroot includes:

- `libcamera-dev` -- Raspberry Pi camera stack
- `libpipewire-0.3-dev` -- PipeWire screen/camera capture
- `libva-dev`, `libdrm-dev`, `libgbm-dev` -- GPU/display
- `libasound2-dev`, `libopus-dev` -- audio
- `libv4l-dev` -- V4L2 camera/encoder
- `libssl-dev` -- TLS
- `libx11-dev`, `libxkbcommon-dev`, `libwayland-dev` -- windowing

## Approach 2: cross-rs Docker (legacy)

Docker-based cross-compilation using [cross](https://github.com/cross-rs/cross).
The custom Docker image (`Dockerfile.aarch64`) extends the cross-rs base with
aarch64 development libraries for ALSA, VAAPI, V4L2, and X11.

Limitation: the base image is Ubuntu 20.04 (Focal), which does not have
`libcamera-dev` or modern PipeWire packages.

### Setup

```sh
cargo install cross --git https://github.com/cross-rs/cross
docker build -t iroh-live-cross-aarch64 -f cross/Dockerfile.aarch64 .
```

### Build

```sh
cargo make cross-check-aarch64
cargo make cross-build-irl-aarch64
cargo make cross-build-pi-zero-aarch64
```

## CI

Both approaches have CI workflows:

- `.github/workflows/ci-aarch64-zig.yml` -- zigbuild (preferred)
- `.github/workflows/ci-aarch64.yml` -- cross-rs Docker (legacy)

Pre-built aarch64 binaries are uploaded to the `rolling-release` GitHub
release on push to main.

## Binary compatibility

The resulting binaries target aarch64 glibc (not musl) and are compatible
with Raspberry Pi OS Bookworm (Debian 12, glibc 2.36).
