#!/usr/bin/env bash
# Create an aarch64 Debian Bookworm sysroot for cross-compilation.
#
# Three extraction methods are tried in order:
#   1. mmdebstrap (preferred, unprivileged, resolves deps automatically)
#   2. debootstrap (needs sudo, resolves deps automatically)
#   3. apt-get download + dpkg-deb (no special tools needed, manual dep list)
#
# The result is a directory with aarch64 headers, shared libraries, and
# pkg-config .pc files that the linker and -sys crates can use.
#
# Prerequisites (method 3, the fallback, needs only):
#   - dpkg-deb (usually pre-installed)
#   - curl or wget
#
# Usage:
#   ./cross/create-sysroot.sh                       # default: cross/sysroot-aarch64
#   ./cross/create-sysroot.sh /path/to/sysroot      # custom output path
#   SUITE=trixie ./cross/create-sysroot.sh          # use Debian Trixie instead
#   METHOD=dpkg ./cross/create-sysroot.sh           # force dpkg method

set -euo pipefail

SYSROOT="${1:-$(cd "$(dirname "$0")" && pwd)/sysroot-aarch64}"
SUITE="${SUITE:-bookworm}"
ARCH="arm64"
MIRROR="${MIRROR:-http://deb.debian.org/debian}"
METHOD="${METHOD:-auto}"

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

echo "Creating aarch64 sysroot from Debian ${SUITE}"
echo "  output:   ${SYSROOT}"
echo "  packages: ${#PACKAGES[@]} top-level packages"
echo ""

# ── Method selection ──────────────────────────────────────────────────

use_mmdebstrap() {
    local include_list
    include_list=$(IFS=,; echo "${PACKAGES[*]}")

    echo "Using mmdebstrap..."
    mmdebstrap \
        --architectures="${ARCH}" \
        --variant=extract \
        --include="${include_list}" \
        "${SUITE}" \
        "${SYSROOT}" \
        "${MIRROR}"
}

use_debootstrap() {
    local include_list
    include_list=$(IFS=,; echo "${PACKAGES[*]}")

    echo "Using debootstrap (requires root)..."
    sudo debootstrap \
        --arch="${ARCH}" \
        --variant=minbase \
        --include="${include_list}" \
        --foreign \
        "${SUITE}" \
        "${SYSROOT}" \
        "${MIRROR}"
    sudo chown -R "$(id -u):$(id -g)" "${SYSROOT}"
}

use_dpkg() {
    # Direct .deb download + extraction. No special tools beyond dpkg-deb
    # and curl. We resolve dependencies manually since we don't have apt's
    # resolver. The list below includes the transitive deps of the packages
    # above that contain files we actually link against.
    #
    # To regenerate this list, run on a Debian Bookworm system:
    #   apt-cache depends --recurse --no-suggests --no-conflicts \
    #     --no-breaks --no-replaces --no-enhances <packages> | \
    #     grep "^\w" | sort -u
    local ALL_DEBS=(
        # The top-level -dev packages
        "${PACKAGES[@]}"
        # Key transitive dependencies that contain .so files or headers
        # we need. This is NOT exhaustive -- it covers what the -sys crates
        # actually look for via pkg-config.
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
        libgbm1
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

    echo "Using dpkg-deb extraction (downloading ${#ALL_DEBS[@]} packages)..."
    local CACHE_DIR="/tmp/sysroot-debs-${SUITE}-${ARCH}"
    mkdir -p "$CACHE_DIR" "${SYSROOT}"

    # Download each .deb from the Debian pool. We use the Packages index
    # to find the exact pool path for each package.
    local PACKAGES_INDEX="${CACHE_DIR}/Packages.gz"
    if [ ! -f "$PACKAGES_INDEX" ] || [ "$(find "$PACKAGES_INDEX" -mmin +60 2>/dev/null)" ]; then
        echo "  Downloading package index..."
        curl -fSL "${MIRROR}/dists/${SUITE}/main/binary-${ARCH}/Packages.gz" \
            -o "$PACKAGES_INDEX"
    fi

    local PACKAGES_FILE="${CACHE_DIR}/Packages"
    gzip -dkf "$PACKAGES_INDEX" 2>/dev/null || true

    local DOWNLOADED=0
    local EXTRACTED=0
    for pkg in "${ALL_DEBS[@]}"; do
        # Find the pool path for this package
        local pool_path
        pool_path=$(awk -v pkg="$pkg" '
            /^Package: / { found = ($2 == pkg) }
            /^Filename: / && found { print $2; exit }
        ' "$PACKAGES_FILE")

        if [ -z "$pool_path" ]; then
            echo "  [skip] ${pkg} (not found in index)"
            continue
        fi

        local deb_file="${CACHE_DIR}/$(basename "$pool_path")"
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
}

if [ "$METHOD" = "auto" ]; then
    if command -v mmdebstrap &>/dev/null; then
        use_mmdebstrap
    elif command -v debootstrap &>/dev/null; then
        use_debootstrap
    else
        use_dpkg
    fi
elif [ "$METHOD" = "mmdebstrap" ]; then
    use_mmdebstrap
elif [ "$METHOD" = "debootstrap" ]; then
    use_debootstrap
elif [ "$METHOD" = "dpkg" ]; then
    use_dpkg
else
    echo "ERROR: unknown METHOD=${METHOD}. Use auto, mmdebstrap, debootstrap, or dpkg." >&2
    exit 1
fi

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
    echo "If using the dpkg method, some packages may need additional transitive deps."
    echo "Try with mmdebstrap for best results: sudo pacman -S mmdebstrap"
else
    echo ""
    echo "Sysroot created successfully: ${SYSROOT}"
fi

echo ""
echo "Next steps:"
echo "  cargo make zig-build-pi-zero    # build pi-zero-demo"
echo "  cargo make zig-build-irl        # build irl CLI"
