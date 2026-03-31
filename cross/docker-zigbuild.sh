#!/usr/bin/env bash
# Run zigbuild inside Docker. Builds the Docker image on first use.
#
# This is the zero-setup cross-compilation path: no zig, no sysroot, no
# host packages needed. Just Docker.
#
# Usage:
#   ./cross/docker-zigbuild.sh -p iroh-live-cli --release
#   ./cross/docker-zigbuild.sh -p pi-zero-demo --release --features raspberry-pi
#
# The Docker image includes zig, cargo-zigbuild, and a pre-built aarch64
# sysroot, so the first build takes a few minutes while the image is
# constructed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IMAGE="iroh-live-zigbuild"

# Build image if it doesn't exist
if ! docker image inspect "$IMAGE" &>/dev/null; then
    echo "Building Docker image $IMAGE (first time only)..."
    docker build -t "$IMAGE" -f "$SCRIPT_DIR/Dockerfile.zigbuild" "$WORKSPACE_ROOT"
fi

# Run with source mounted and cargo registry cached to avoid re-downloading
# crates on every build.
exec docker run --rm \
    -v "${WORKSPACE_ROOT}:/io" \
    -v "${CARGO_HOME:-$HOME/.cargo}/registry:/usr/local/cargo/registry" \
    -w /io \
    "$IMAGE" \
    ./cross/zigbuild.sh "$@"
