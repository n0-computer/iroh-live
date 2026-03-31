#!/usr/bin/env bash
# Cross-compile an aarch64 binary using cargo-zigbuild with a Debian sysroot.
#
# Sets up pkg-config, CC/CXX, and linker environment for aarch64, then
# delegates to `cargo zigbuild`. All arguments are forwarded.
#
# Usage:
#   ./cross/zigbuild.sh -p pi-zero-demo --release
#   ./cross/zigbuild.sh -p iroh-live-cli --release
#   SYSROOT=/custom/path ./cross/zigbuild.sh -p pi-zero-demo --release
#
# The sysroot is found in this order:
#   1. $SYSROOT env var (set by Docker or user)
#   2. cross/sysroot-aarch64 (default for host-native builds)
#
# Prerequisites:
#   - zig:            sudo pacman -S zig           (Arch)
#   - cargo-zigbuild: cargo install cargo-zigbuild
#   - sysroot:        ./cross/create-sysroot.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SYSROOT="${SYSROOT:-${SCRIPT_DIR}/sysroot-aarch64}"
TARGET="aarch64-unknown-linux-gnu"

# Debian Bookworm ships glibc 2.36. Pin the target to this version so the
# binary runs on Raspberry Pi OS Bookworm without requiring a newer glibc.
TARGET_WITH_GLIBC="${TARGET}.2.36"

# ── Validate prerequisites ────────────────────────────────────────────

if ! command -v zig &>/dev/null; then
    echo "ERROR: zig not found. Install it:" >&2
    echo "  Arch:   sudo pacman -S zig" >&2
    echo "  Debian: sudo apt install zig" >&2
    echo "  Or:     https://ziglang.org/download/" >&2
    exit 1
fi

if ! command -v cargo-zigbuild &>/dev/null; then
    echo "ERROR: cargo-zigbuild not found. Install it:" >&2
    echo "  cargo install cargo-zigbuild" >&2
    exit 1
fi

if [ ! -d "$SYSROOT" ]; then
    echo "ERROR: sysroot not found at $SYSROOT" >&2
    echo "" >&2
    echo "Create it with:" >&2
    echo "  cargo make cross-sysroot" >&2
    echo "  # or: ./cross/create-sysroot.sh" >&2
    echo "" >&2
    echo "For Docker builds (no host setup needed):" >&2
    echo "  cargo make cross-build-docker -- -p iroh-live-cli --release" >&2
    exit 1
fi

# ── pkg-config: tell -sys crates where to find aarch64 .pc files ──────

LIB_DIR="${SYSROOT}/usr/lib/aarch64-linux-gnu"

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="${SYSROOT}"

# Tell pkg-config where to find .pc files for the target architecture.
# PKG_CONFIG_PATH is the generic var; the _aarch64_unknown_linux_gnu suffix
# is the target-specific override recognized by the pkg-config-rs crate.
export PKG_CONFIG_PATH="${LIB_DIR}/pkgconfig:${SYSROOT}/usr/share/pkgconfig"
export PKG_CONFIG_PATH_aarch64_unknown_linux_gnu="${LIB_DIR}/pkgconfig:${SYSROOT}/usr/share/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR_aarch64_unknown_linux_gnu="${SYSROOT}"

# ── C/C++ cross-compiler for -sys crate build scripts ─────────────────
# Use zig as the C/C++ compiler. This ensures C code is compiled against
# the correct glibc version (matching the zigbuild target), avoiding symbol
# mismatches like __isoc23_sscanf that occur when host GCC links newer
# glibc symbols against an older sysroot.
#
# Wrappers are needed because the cc crate doesn't handle "zig cc" as a
# compound command.
ZIG_CC_WRAPPER="/tmp/zig-cc-aarch64.sh"
cat > "$ZIG_CC_WRAPPER" <<ZIGCC
#!/bin/sh
# Strip flags that zig cc doesn't support:
#   --target=aarch64-unknown-linux-gnu* (Rust triple format; zig uses -target)
#   -Wp,-U_FORTIFY_SOURCE (zig cc doesn't support -Wp preprocessor passthrough)
ARGS=""
for arg in "\$@"; do
    case "\$arg" in
        --target=aarch64-unknown-linux-gnu*) ;;
        -Wp,*) ;;
        *) ARGS="\$ARGS \$arg" ;;
    esac
done
exec zig cc -target aarch64-linux-gnu.2.36 --sysroot="${SYSROOT}" \$ARGS
ZIGCC
chmod +x "$ZIG_CC_WRAPPER"

ZIG_CXX_WRAPPER="/tmp/zig-cxx-aarch64.sh"
cat > "$ZIG_CXX_WRAPPER" <<ZIGCXX
#!/bin/sh
ARGS=""
for arg in "\$@"; do
    case "\$arg" in
        --target=aarch64-unknown-linux-gnu*) ;;
        -Wp,*) ;;
        *) ARGS="\$ARGS \$arg" ;;
    esac
done
exec zig c++ -target aarch64-linux-gnu.2.36 --sysroot="${SYSROOT}" \$ARGS
ZIGCXX
chmod +x "$ZIG_CXX_WRAPPER"

export CC_aarch64_unknown_linux_gnu="$ZIG_CC_WRAPPER"
export CXX_aarch64_unknown_linux_gnu="$ZIG_CXX_WRAPPER"
# AR wrapper -- cmake and the cc crate need a single executable path.
ZIG_AR_WRAPPER="/tmp/zig-ar-aarch64.sh"
cat > "$ZIG_AR_WRAPPER" <<'ZIGAR'
#!/bin/sh
exec zig ar "$@"
ZIGAR
chmod +x "$ZIG_AR_WRAPPER"
export AR_aarch64_unknown_linux_gnu="$ZIG_AR_WRAPPER"

# RANLIB wrapper (some build scripts call ranlib separately).
ZIG_RANLIB_WRAPPER="/tmp/zig-ranlib-aarch64.sh"
cat > "$ZIG_RANLIB_WRAPPER" <<'ZIGRANLIB'
#!/bin/sh
exec zig ranlib "$@"
ZIGRANLIB
chmod +x "$ZIG_RANLIB_WRAPPER"
export RANLIB_aarch64_unknown_linux_gnu="$ZIG_RANLIB_WRAPPER"

# Some -sys crates also read CFLAGS for extra include paths.
export CFLAGS_aarch64_unknown_linux_gnu="--sysroot=${SYSROOT}"
export CXXFLAGS_aarch64_unknown_linux_gnu="--sysroot=${SYSROOT}"

# aws-lc-sys uses cmake. Tell it where to find the cross toolchain.
export CMAKE_SYSTEM_NAME="Linux"
export CMAKE_SYSTEM_PROCESSOR="aarch64"
export CMAKE_C_COMPILER="$ZIG_CC_WRAPPER"
export CMAKE_CXX_COMPILER="$ZIG_CXX_WRAPPER"
export CMAKE_AR="$ZIG_AR_WRAPPER"
export CMAKE_RANLIB="$ZIG_RANLIB_WRAPPER"

# ── Linker flags ──────────────────────────────────────────────────────
# cargo-zigbuild handles the linker itself (uses zig as linker), but it
# needs to know where to find the target libraries.
export RUSTFLAGS="${RUSTFLAGS:-} -L ${LIB_DIR} -L ${SYSROOT}/lib/aarch64-linux-gnu"

# ── Build ─────────────────────────────────────────────────────────────

echo "zigbuild: target=${TARGET_WITH_GLIBC}, sysroot=${SYSROOT}"
echo "zigbuild: CC=${CC_aarch64_unknown_linux_gnu:-zig}"
echo ""

exec cargo zigbuild \
    --target "${TARGET_WITH_GLIBC}" \
    "$@"
