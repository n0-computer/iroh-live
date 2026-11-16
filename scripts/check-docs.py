#!/usr/bin/env python3
"""Checks that hand-written docs agree with their sources of truth.

- Every page under `docs/` with a `rust,no_run` block is in the `pages!` list
  of the `guide` module in `iroh-live/src/lib.rs`, so its programs compile.
- Every moq git rev that a page or README names equals the rev the root
  `Cargo.toml` pins.
- Every Cargo feature of `iroh-live` and `iroh-live-cli` appears in
  `docs/reference/features.md`.

The CLI flags are checked by a unit test in `iroh-live-cli` instead, since
clap is the source of truth there.

Usage: scripts/check-docs.py [ROOT]. Exits 1 if a check fails.
"""

import re
import sys
import tomllib
from pathlib import Path

MOQ_REV = re.compile(r'github\.com/moq-dev/moq"[^\n]*?rev\s*=\s*"([0-9a-f]+)"')
SKIP_DIRS = {"target", "node_modules", "plans", ".git", "build", "dist"}


def guide_pages(root):
    lib = (root / "iroh-live/src/lib.rs").read_text()
    block = lib[lib.index("pages! {"):]
    block = block[: block.index("}")]
    return set(re.findall(r'=>\s*"([^"]+)"', block))


def check_guide(root):
    listed = guide_pages(root)
    errors = []
    for page in sorted((root / "docs").rglob("*.md")):
        rel = page.relative_to(root / "docs").as_posix()
        if "```rust,no_run" in page.read_text() and rel not in listed:
            errors.append(f"docs/{rel} has a `rust,no_run` block but is not in the "
                          "`pages!` list in iroh-live/src/lib.rs")
    for rel in sorted(listed):
        if not (root / "docs" / rel).exists():
            errors.append(f"`pages!` lists docs/{rel}, which does not exist")
    return errors


def check_moq_rev(root):
    pinned = set(MOQ_REV.findall((root / "Cargo.toml").read_text()))
    if len(pinned) != 1:
        return [f"the root Cargo.toml pins {len(pinned)} moq revs, expected one"]
    (rev,) = pinned
    errors = []
    for md in sorted(root.rglob("*.md")):
        if SKIP_DIRS & set(md.relative_to(root).parts):
            continue
        text = md.read_text()
        for found in MOQ_REV.findall(text) + re.findall(r"pinned to rev `([0-9a-f]+)`", text):
            if found != rev:
                errors.append(f"{md.relative_to(root)} names moq rev {found}, "
                              f"but Cargo.toml pins {rev}")
    return errors


def check_features(root):
    doc = (root / "docs/reference/features.md").read_text()
    errors = []
    for crate in ("iroh-live", "iroh-live-cli"):
        manifest = tomllib.loads((root / crate / "Cargo.toml").read_text())
        for feature in manifest.get("features", {}):
            if feature != "default" and f"`{feature}`" not in doc:
                errors.append(f"feature `{feature}` of {crate} is missing from "
                              "docs/reference/features.md")
    return errors


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent)
    errors = check_guide(root) + check_moq_rev(root) + check_features(root)
    for error in errors:
        print(error)
    print(f"docs checks: {len(errors)} problems")
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
