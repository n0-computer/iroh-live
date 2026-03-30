# Cross-compilation for aarch64

Docker-based cross-compilation using [cross](https://github.com/cross-rs/cross).
The custom Docker image (`Dockerfile.aarch64`) extends the cross-rs base with
aarch64 development libraries for ALSA, VAAPI, V4L2, PipeWire, and X11.

## Setup

```sh
# Install cross (one-time)
cargo install cross --git https://github.com/cross-rs/cross

# Build the Docker image (one-time, from repo root)
docker build -t iroh-live-cross-aarch64 -f cross/Dockerfile.aarch64 .
```

## Local build

```sh
# Check the whole workspace compiles for aarch64
cargo make cross-check-aarch64

# Build the irl CLI (release)
cargo make cross-build-irl-aarch64

# Build the pi-zero demo with hardware codec support (release)
cargo make cross-build-pi-zero-aarch64
```

Binaries land in `target/aarch64-unknown-linux-gnu/release/`.

## Deploy to Pi

```sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_IP>:~/
scp target/aarch64-unknown-linux-gnu/release/irl pi@<PI_IP>:~/
```

## CI

The `.github/workflows/ci-aarch64.yml` workflow runs cross-check on every PR
and builds release binaries on push to main. Pre-built aarch64 binaries are
uploaded to the `rolling-release` GitHub release.

## What is not included

**libcamera-dev**: Not available in the Ubuntu version used by the cross-rs
base image (Ubuntu 20.04 / Focal). The Pi demos use V4L2 directly for camera
capture, so this is not a blocker. If you need libcamera for a different use
case, build it from source in the Dockerfile or build natively on the Pi.
