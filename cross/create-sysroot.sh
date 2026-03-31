#!/usr/bin/env bash
# Create an aarch64 Debian Bookworm sysroot for cross-compilation.
#
# Downloads .deb packages directly from the Debian archive and extracts them
# with dpkg-deb. No sudo, mmdebstrap, or debootstrap needed.
#
# Prerequisites:
#   - dpkg-deb (usually pre-installed on Debian/Ubuntu; available on Arch via dpkg)
#   - curl
#   - gzip
#
# Usage:
#   ./cross/create-sysroot.sh                       # default: cross/sysroot-aarch64
#   ./cross/create-sysroot.sh /path/to/sysroot      # custom output path
#   SUITE=trixie ./cross/create-sysroot.sh          # use Debian Trixie instead

set -euo pipefail

SYSROOT="${1:-$(cd "$(dirname "$0")" && pwd)/sysroot-aarch64}"
SUITE="${SUITE:-bookworm}"
ARCH="arm64"
MIRROR="${MIRROR:-http://deb.debian.org/debian}"

# All the -dev packages needed by workspace -sys crates when targeting aarch64.
# Grouped by subsystem.
PACKAGES=(
    # Audio
    libasound2-dev
    libopus-dev
    # Video / GPU
    libva-dev
    libdrm-dev
    libgbm-dev
    libegl-dev
    libgl-dev
    # V4L2 (camera + M2M encoder on Pi)
    libv4l-dev
    # Display / windowing
    libx11-dev
    libxext-dev
    libxfixes-dev
    libxkbcommon-dev
    libwayland-dev
    libxcb1-dev
    # TLS
    libssl-dev
    # PipeWire
    libpipewire-0.3-dev
    # libcamera (Raspberry Pi camera stack)
    libcamera-dev
    # Base
    libc6-dev
    linux-libc-dev
)

# Transitive dependencies that contain .so files or headers we need.
# This is NOT exhaustive -- it covers what the -sys crates actually look
# for via pkg-config.
#
# To regenerate this list, run on a Debian Bookworm system:
#   apt-cache depends --recurse --no-suggests --no-conflicts \
#     --no-breaks --no-replaces --no-enhances <packages> | \
#     grep "^\w" | sort -u
TRANSITIVE_DEPS=(
    libasound2
    libopus0
    libva2
    libva-drm2
    libva-x11-2
    libva-wayland2
    libva-glx2
    libdrm2
    libdrm-common
    libgbm1
    libegl1
    libgl1
    libglvnd0
    libglx0
    libgles2
    libegl-mesa0
    libv4l-0
    libx11-6
    libxext6
    libxfixes3
    libxkbcommon0
    libwayland-client0
    libwayland-server0
    libwayland-cursor0
    libwayland-egl1
    libssl3
    libpipewire-0.3-0
    libspa-0.2-dev
    # Transitive deps of the above that provide needed .so stubs
    libx11-xcb1
    libxau6
    libxdmcp6
    libxcb1
    libffi8
    zlib1g
    libbsd0
)

ALL_DEBS=("${PACKAGES[@]}" "${TRANSITIVE_DEPS[@]}")

echo "Creating aarch64 sysroot from Debian ${SUITE}"
echo "  output:   ${SYSROOT}"
echo "  packages: ${#ALL_DEBS[@]} total (${#PACKAGES[@]} top-level + ${#TRANSITIVE_DEPS[@]} transitive)"
echo ""

# ── Download and extract ─────────────────────────────────────────────

echo "Downloading ${#ALL_DEBS[@]} packages via dpkg-deb extraction..."
CACHE_DIR="/tmp/sysroot-debs-${SUITE}-${ARCH}"
mkdir -p "$CACHE_DIR" "${SYSROOT}"

# Download the Packages index (cached for 60 minutes).
PACKAGES_INDEX="${CACHE_DIR}/Packages.gz"
if [ ! -f "$PACKAGES_INDEX" ] || [ "$(find "$PACKAGES_INDEX" -mmin +60 2>/dev/null)" ]; then
    echo "  Downloading package index..."
    curl -fSL "${MIRROR}/dists/${SUITE}/main/binary-${ARCH}/Packages.gz" \
        -o "$PACKAGES_INDEX"
fi

PACKAGES_FILE="${CACHE_DIR}/Packages"
gzip -dkf "$PACKAGES_INDEX" 2>/dev/null || true

DOWNLOADED=0
EXTRACTED=0
for pkg in "${ALL_DEBS[@]}"; do
    # Find the pool path for this package
    pool_path=$(awk -v pkg="$pkg" '
        /^Package: / { found = ($2 == pkg) }
        /^Filename: / && found { print $2; exit }
    ' "$PACKAGES_FILE")

    if [ -z "$pool_path" ]; then
        echo "  [skip] ${pkg} (not found in index)"
        continue
    fi

    deb_file="${CACHE_DIR}/$(basename "$pool_path")"
    if [ ! -f "$deb_file" ]; then
        curl -fSL "${MIRROR}/${pool_path}" -o "$deb_file" 2>/dev/null || {
            echo "  [skip] ${pkg} (download failed)"
            continue
        }
        DOWNLOADED=$((DOWNLOADED + 1))
    fi

    dpkg-deb -x "$deb_file" "${SYSROOT}" 2>/dev/null || {
        echo "  [skip] ${pkg} (extract failed)"
        continue
    }
    EXTRACTED=$((EXTRACTED + 1))
done
echo "  downloaded ${DOWNLOADED} new, extracted ${EXTRACTED} total"

# ── Strip unnecessary files to shrink the sysroot ─────────────────────
echo ""
echo "Stripping unnecessary files..."
BEFORE_SIZE=$(du -sh "${SYSROOT}" | cut -f1)

rm -rf "${SYSROOT:?}"/{boot,home,media,mnt,opt,root,run,srv,tmp}
rm -rf "${SYSROOT:?}"/var/{cache,log,mail,tmp,backups}
rm -rf "${SYSROOT:?}"/usr/share/{doc,man,info,locale,i18n,lintian,bug}
rm -rf "${SYSROOT:?}"/usr/share/{bash-completion,zsh,fish}
rm -rf "${SYSROOT:?}"/usr/bin "${SYSROOT:?}"/usr/sbin
rm -rf "${SYSROOT:?}"/bin "${SYSROOT:?}"/sbin
rm -rf "${SYSROOT:?}"/etc

AFTER_SIZE=$(du -sh "${SYSROOT}" | cut -f1)
echo "  before: ${BEFORE_SIZE}, after: ${AFTER_SIZE}"

# ── Fix absolute symlinks ─────────────────────────────────────────────
# .so symlinks often point to absolute paths (/lib/aarch64-linux-gnu/...)
# which break under --sysroot. Convert them to relative paths.
echo "Fixing absolute symlinks..."
FIX_COUNT=0
while IFS= read -r -d '' link; do
    target=$(readlink "$link")
    if [[ "$target" == /* ]]; then
        link_dir=$(dirname "$link")
        rel_target=$(python3 -c "import os.path; print(os.path.relpath('${SYSROOT}${target}', '${link_dir}'))" 2>/dev/null) || continue
        ln -sf "$rel_target" "$link"
        FIX_COUNT=$((FIX_COUNT + 1))
    fi
done < <(find "${SYSROOT}" -type l -print0 2>/dev/null)
echo "  fixed ${FIX_COUNT} symlinks"

# ── Verify key files exist ────────────────────────────────────────────
echo ""
echo "Verifying sysroot..."
MISSING=0
for pc_file in alsa libdrm gbm egl libssl libpipewire-0.3 libcamera; do
    pc_path="${SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig/${pc_file}.pc"
    if [ -f "$pc_path" ]; then
        echo "  [ok] ${pc_file}.pc"
    else
        echo "  [MISSING] ${pc_file}.pc"
        ((MISSING++)) || true
    fi
done

if [ "$MISSING" -gt 0 ]; then
    echo ""
    echo "WARNING: ${MISSING} pkg-config files missing. The sysroot may be incomplete."
    echo "Some packages may need additional transitive deps added to the script."
else
    echo ""
    echo "Sysroot created successfully: ${SYSROOT}"
fi

echo ""
echo "Next steps:"
echo "  cargo make cross-build -- -p iroh-live-cli --release"
echo "  cargo make cross-build -- -p pi-zero-demo --release --features raspberry-pi"
