#!/usr/bin/env bash
# Cross-compile pi-zero-demo for Raspberry Pi Zero 2 W (aarch64).
#
# Prerequisites:
#   1. Rust aarch64 target:  rustup target add aarch64-unknown-linux-gnu
#   2. Cross-linker:         sudo pacman -S aarch64-linux-gnu-gcc   (Arch)
#                            sudo apt install gcc-aarch64-linux-gnu  (Debian/Ubuntu)
#   3. Pi sysroot with dev libs (libasound2-dev, libgbm-dev, libdrm-dev, libegl-dev):
#        mkdir -p ~/pi-sysroot
#        rsync -az pi@<IP>:/usr/lib/aarch64-linux-gnu ~/pi-sysroot/usr/lib/
#        rsync -az pi@<IP>:/usr/include ~/pi-sysroot/usr/
#        rsync -az pi@<IP>:/lib/aarch64-linux-gnu ~/pi-sysroot/lib/
#
# Usage:
#   ./demos/pi-zero/build.sh                          # build only
#   ./demos/pi-zero/build.sh --deploy livepizero      # build + scp to pi@livepizero
#   PI_SYSROOT=/path/to/sysroot ./build.sh            # custom sysroot path

set -euo pipefail

DEPLOY_HOST=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --deploy) DEPLOY_HOST="$2"; shift 2 ;;
        *) echo "usage: $0 [--deploy HOSTNAME]" >&2; exit 1 ;;
    esac
done

TARGET="aarch64-unknown-linux-gnu"
SYSROOT="${PI_SYSROOT:-$HOME/pi-sysroot}"
LIB_DIR="$SYSROOT/usr/lib/aarch64-linux-gnu"
LIB_DIR2="$SYSROOT/lib/aarch64-linux-gnu"

# --- validate sysroot ---

if [ ! -d "$SYSROOT" ]; then
    echo "error: sysroot not found at $SYSROOT" >&2
    echo "Set PI_SYSROOT or create it — see README.md for instructions." >&2
    exit 1
fi

# Fix missing .so dev symlink (rsync from Pi often copies only the versioned .so.2).
if [ ! -f "$LIB_DIR/libasound.so" ] && [ -f "$LIB_DIR/libasound.so.2" ]; then
    echo "note: creating libasound.so symlink in sysroot" >&2
    ln -sf libasound.so.2 "$LIB_DIR/libasound.so"
fi

if [ ! -f "$LIB_DIR/libasound.so" ]; then
    echo "error: libasound.so not found in $LIB_DIR" >&2
    echo "Install libasound2-dev on the Pi and re-rsync the sysroot." >&2
    exit 1
fi

# Debian's libc.so is a linker script with absolute paths like:
#   GROUP ( /lib/aarch64-linux-gnu/libc.so.6 ... AS_NEEDED ( /lib/ld-linux-aarch64.so.1 ) )
# With --sysroot the linker prepends the sysroot, expecting e.g.
# $SYSROOT/lib/ld-linux-aarch64.so.1. But rsync puts everything under
# lib/aarch64-linux-gnu/. Create the top-level symlink so the linker script
# resolves correctly.
if [ -d "$LIB_DIR2" ] && [ ! -e "$SYSROOT/lib/ld-linux-aarch64.so.1" ]; then
    echo "note: creating ld-linux-aarch64.so.1 symlink in sysroot" >&2
    ln -sf aarch64-linux-gnu/ld-linux-aarch64.so.1 "$SYSROOT/lib/ld-linux-aarch64.so.1"
fi

# --- create a linker wrapper ---
# Cargo's RUSTFLAGS env var can be overridden by .cargo/config.toml. A linker
# wrapper is the most reliable way to inject -L search paths for the sysroot.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Use a fixed /tmp path so cargo's build cache isn't invalidated when
# building from different clones (the linker path is part of the cache key).
WRAPPER="/tmp/rpi-linker.sh"
cat > "$WRAPPER" <<LINKER_EOF
#!/bin/sh
exec aarch64-linux-gnu-gcc --sysroot="$SYSROOT" -L "$LIB_DIR" -L "$LIB_DIR2" "\$@"
LINKER_EOF
chmod +x "$WRAPPER"

# --- pkg-config for build scripts (alsa-sys, etc.) ---

export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_PATH="$LIB_DIR/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1

# --- build ---

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$WRAPPER"

cargo build \
    --manifest-path "$WORKSPACE_ROOT/Cargo.toml" \
    -p pi-zero-demo \
    --target "$TARGET" \
    --release

# --- strip & output ---

TARGET_DIR="$(cargo metadata --manifest-path "$WORKSPACE_ROOT/Cargo.toml" --format-version 1 --no-deps | jq -r '.target_directory')"
BINARY="$TARGET_DIR/$TARGET/release/pi-zero-demo"

# The workspace sets debug=true in release profile. Strip debug symbols to
# shrink the binary from ~500 MB to ~30 MB — fine for deployment.
aarch64-linux-gnu-strip "$BINARY"

SIZE="$(du -h "$BINARY" | cut -f1)"
echo ""
echo "built: $BINARY ($SIZE)"

if [ -n "$DEPLOY_HOST" ]; then
    echo "deploying to pi@$DEPLOY_HOST..."
    scp "$BINARY" "pi@$DEPLOY_HOST:~/pi-zero-demo"
    echo "deployed: pi@$DEPLOY_HOST:~/pi-zero-demo"
else
    echo "deploy: scp $BINARY pi@<HOST>:~/pi-zero-demo"
fi
