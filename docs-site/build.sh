#!/usr/bin/env bash
# Builds the documentation site into docs-site/build/:
#   build/index.html   the landing page
#   build/book/        the mdBook built from docs/
#
# Usage: docs-site/build.sh [--serve [PORT]]
#   --serve  also serves build/ on http://127.0.0.1:PORT (default 3000).
# For live reload while editing pages, `mdbook serve docs-site` serves the
# book alone.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/build"

rm -rf "$out"
mdbook build "$here"
cp "$here/landing/index.html" "$out/index.html"
cp "$here/theme/logo.svg" "$out/logo.svg"
# GitHub Pages would otherwise run Jekyll over the output.
touch "$out/.nojekyll"
echo "site written to $out"

if [[ "${1:-}" == "--serve" ]]; then
    port="${2:-3000}"
    echo "serving on http://127.0.0.1:$port/"
    exec python3 -m http.server "$port" --bind 127.0.0.1 --directory "$out"
fi
