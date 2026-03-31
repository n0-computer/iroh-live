# Cross-compilation for aarch64

Two paths: host-native (Linux with zig installed) or Docker (any OS).

## Host-native (Linux)

### Prerequisites

```sh
# Arch Linux
sudo pacman -S zig dpkg python
cargo install cargo-zigbuild

# Debian/Ubuntu
sudo apt install zig dpkg-dev python3
cargo install cargo-zigbuild

# Rust target
rustup target add aarch64-unknown-linux-gnu
```

### Create the sysroot (one-time)

```sh
cargo make cross-sysroot
```

Downloads aarch64 .deb packages from Debian Bookworm and extracts headers
and libraries into `cross/sysroot-aarch64/`. Uses `dpkg-deb` only, no
sudo needed. Takes 1-2 minutes on a decent connection.

### Build

```sh
# irl CLI (release)
cargo make cross-build -- -p iroh-live-cli --release

# Pi Zero demo with libcamera
cargo make cross-build -- -p pi-zero-demo --release --features libcamera

# Check the whole workspace
cargo make cross-build -- --workspace --exclude bevy-demo --exclude bevy-call

# Arbitrary example
cargo make cross-build -- --release --example publish
```

Everything after `--` is forwarded to `cargo zigbuild`.

## Docker (macOS, Windows, any host)

No zig, no sysroot, no host packages needed. Just Docker.

```sh
# irl CLI (release)
cargo make cross-build-docker -- -p iroh-live-cli --release

# Pi Zero demo
cargo make cross-build-docker -- -p pi-zero-demo --release --features raspberry-pi
```

The Docker image is built on first use (~5 minutes) and cached. It
includes zig, cargo-zigbuild, and a pre-built aarch64 sysroot.

### Direct script invocation

Both paths can also be used without cargo-make:

```sh
# Host-native
./cross/create-sysroot.sh                               # one-time
./cross/zigbuild.sh -p iroh-live-cli --release

# Docker
./cross/docker-zigbuild.sh -p iroh-live-cli --release

# Custom sysroot location
SYSROOT=/path/to/sysroot ./cross/zigbuild.sh -p pi-zero-demo --release
```

## How it works

1. `create-sysroot.sh` downloads aarch64 `.deb` packages from the Debian
   archive and extracts them with `dpkg-deb`. No chroot, no root, no
   mmdebstrap needed.

2. `zigbuild.sh` sets `PKG_CONFIG_SYSROOT_DIR`, `PKG_CONFIG_PATH`, CC/CXX
   (using zig as a cross-compiler), and linker flags so that `-sys` crates
   find the aarch64 headers and libraries.

3. `cargo-zigbuild` uses Zig as the linker, targeting
   `aarch64-unknown-linux-gnu.2.36` (glibc 2.36 = Debian Bookworm =
   Raspberry Pi OS Bookworm).

4. `docker-zigbuild.sh` wraps the above in a Docker container with
   everything pre-installed, mounting the source tree and cargo registry.

## Libraries available in the sysroot

The Bookworm sysroot includes:

- `libcamera-dev` -- Raspberry Pi camera stack
- `libpipewire-0.3-dev` -- PipeWire screen/camera capture
- `libva-dev`, `libdrm-dev`, `libgbm-dev` -- GPU/display
- `libasound2-dev`, `libopus-dev` -- audio
- `libv4l-dev` -- V4L2 camera/encoder
- `libssl-dev` -- TLS
- `libx11-dev`, `libxkbcommon-dev`, `libwayland-dev` -- windowing

## Binary compatibility

The resulting binaries target aarch64 glibc (not musl) and are compatible
with Raspberry Pi OS Bookworm (Debian 12, glibc 2.36).
