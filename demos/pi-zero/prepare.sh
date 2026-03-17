#!/usr/bin/env bash
# Prepare a Raspberry Pi for cross-compiling pi-zero-demo.
#
# Usage:
#   ./demos/pi-zero/prepare.sh pi@livepizero setup         # install dev libs on Pi
#   ./demos/pi-zero/prepare.sh pi@livepizero sync-sysroot  # rsync libs to ~/pi-sysroot
#   PI_SYSROOT=/other/path ./prepare.sh pi@host sync-sysroot

set -euo pipefail

if [ $# -lt 2 ]; then
    echo "usage: $0 USER@HOST {setup|sync-sysroot}" >&2
    exit 1
fi

HOST="$1"
CMD="$2"
SYSROOT="${PI_SYSROOT:-$HOME/pi-sysroot}"

case "$CMD" in
    setup)
        echo "installing dev libraries on $HOST ..."
        ssh "$HOST" 'sudo apt-get update && sudo apt-get install -y \
            libasound2-dev \
            libgbm-dev \
            libdrm-dev \
            libegl-dev \
            libgles-dev'
        echo "done — now run: $0 $HOST sync-sysroot"
        ;;

    sync-sysroot)
        echo "syncing sysroot from $HOST to $SYSROOT ..."
        mkdir -p "$SYSROOT"
        rsync -az "$HOST":/usr/lib/aarch64-linux-gnu/ "$SYSROOT/usr/lib/aarch64-linux-gnu/"
        rsync -az "$HOST":/usr/include/                "$SYSROOT/usr/include/"
        rsync -az "$HOST":/lib/aarch64-linux-gnu/      "$SYSROOT/lib/aarch64-linux-gnu/"

        # Fix ld-linux symlink (Debian's libc.so linker script uses absolute paths).
        if [ ! -e "$SYSROOT/lib/ld-linux-aarch64.so.1" ] && \
           [ -e "$SYSROOT/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1" ]; then
            ln -sf aarch64-linux-gnu/ld-linux-aarch64.so.1 "$SYSROOT/lib/ld-linux-aarch64.so.1"
        fi

        # Fix common missing dev symlinks.
        for lib in asound gbm drm EGL GLESv2; do
            so="$SYSROOT/usr/lib/aarch64-linux-gnu/lib${lib}.so"
            if [ ! -e "$so" ]; then
                # Find the versioned .so and symlink.
                versioned=$(ls "$SYSROOT/usr/lib/aarch64-linux-gnu/lib${lib}.so."* 2>/dev/null | head -1)
                if [ -n "$versioned" ]; then
                    ln -sf "$(basename "$versioned")" "$so"
                    echo "  created symlink: lib${lib}.so -> $(basename "$versioned")"
                fi
            fi
        done

        echo "sysroot ready at $SYSROOT"
        echo "now run: ./demos/pi-zero/build.sh"
        ;;

    *)
        echo "unknown command: $CMD" >&2
        echo "usage: $0 USER@HOST {setup|sync-sysroot}" >&2
        exit 1
        ;;
esac
