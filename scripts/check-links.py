#!/usr/bin/env python3
"""Checks the relative links and anchors in the repository's Markdown files.

Every link that is not an absolute URL must name a file that exists, and its
`#anchor`, if any, must match a heading of that file. Anchors follow GitHub's
rule, which mdBook shares for ASCII headings: lowercase, drop punctuation
other than `-` and `_`, and turn spaces into `-`.

Usage: scripts/check-links.py [ROOT]. Exits 1 if a link is broken.
"""

import re
import sys
from pathlib import Path

SKIP_DIRS = {"target", "node_modules", "plans", ".git", "build", ".gradle", "dist"}
LINK = re.compile(r"(?<!!)\[(?:[^\[\]]|\[[^\]]*\])*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
IMAGE = re.compile(r"!\[[^\]]*\]\(([^)\s]+)\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
FENCE = re.compile(r"^\s*(```|~~~)")


def strip_code(lines):
    """Yields (line number, text) outside fenced code, with inline code removed."""
    fenced = False
    for number, line in enumerate(lines, 1):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            continue
        yield number, re.sub(r"`[^`]*`", "``", line)


def slug(heading):
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    text = text.replace("`", "").strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def anchors(path, cache={}):
    if path not in cache:
        found, seen = set(), {}
        fenced = False
        for line in path.read_text(encoding="utf-8").splitlines():
            if FENCE.match(line):
                fenced = not fenced
                continue
            match = None if fenced else HEADING.match(line)
            if match:
                base = slug(match.group(2))
                count = seen.get(base, 0)
                seen[base] = count + 1
                found.add(base if count == 0 else f"{base}-{count}")
        cache[path] = found
    return cache[path]


def markdown_files(root):
    for path in sorted(root.rglob("*.md")):
        if not SKIP_DIRS.intersection(path.relative_to(root).parts):
            yield path


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    broken = 0
    checked = 0
    for path in markdown_files(root):
        lines = path.read_text(encoding="utf-8").splitlines()
        for number, line in strip_code(lines):
            for match in list(LINK.finditer(line)) + list(IMAGE.finditer(line)):
                target = match.group(1)
                if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("<"):
                    continue
                checked += 1
                file_part, _, anchor = target.partition("#")
                linked = (path.parent / file_part).resolve() if file_part else path
                where = f"{path.relative_to(root)}:{number}"
                if not linked.exists():
                    print(f"{where}: missing file {target}")
                    broken += 1
                elif anchor and linked.suffix == ".md" and anchor not in anchors(linked):
                    print(f"{where}: missing anchor {target}")
                    broken += 1
    print(f"{checked} relative links checked, {broken} broken")
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main())
