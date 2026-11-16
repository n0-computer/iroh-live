#!/usr/bin/env bash
# Builds the documentation site into docs-site/build/:
#   build/index.html   the landing page
#   build/book/        the mdBook built from docs/
#   build/api/         the rustdoc of the library crates
#
# Usage: docs-site/build.sh [--no-api] [--serve [PORT]]
#   --no-api  skips the rustdoc, which needs the system libraries of a full
#             build and takes a few minutes the first time.
#   --serve   also serves build/ on http://127.0.0.1:PORT (default 3000).
# For live reload while editing pages, `mdbook serve docs-site` serves the
# book alone.
#
# DOCS_SITE_CARGO overrides the cargo command, for a wrapper that limits
# concurrent builds.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(dirname "$here")"
out="$here/build"

api=1
serve=""
port=3000
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-api) api=0 ;;
        --serve)
            serve=1
            if [[ "${2:-}" =~ ^[0-9]+$ ]]; then port="$2"; shift; fi
            ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

rm -rf "$out"
mdbook build "$here"
cp "$here/landing/index.html" "$out/index.html"
cp "$here/theme/logo.svg" "$out/logo.svg"

if [[ "$api" == 1 ]]; then
    # The library crates only: the CLI is a binary, and the demos are apps.
    read -r -a cargo <<< "${DOCS_SITE_CARGO:-cargo}"
    (cd "$repo" && "${cargo[@]}" doc --locked --workspace --all-features --no-deps \
        --exclude iroh-live-cli --exclude iroh-live-android \
        --exclude gles-yuv --exclude pi-zero-demo)
    target="$(cd "$repo" && cargo metadata --format-version 1 --no-deps \
        | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
    cp -r "$target/doc" "$out/api"
    # rustdoc writes no index of its own for a workspace.
    cp "$here/api-index.html" "$out/api/index.html"
fi

# GitHub Pages would otherwise run Jekyll over the output.
touch "$out/.nojekyll"
echo "site written to $out"

if [[ -n "$serve" ]]; then
    echo "serving on http://127.0.0.1:$port/"
    exec python3 -m http.server "$port" --bind 127.0.0.1 --directory "$out"
fi
