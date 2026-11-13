#!/usr/bin/env python3
"""mdBook preprocessor that points links leaving `docs/` at GitHub.

The pages link to examples and crate directories with relative paths such as
`../../iroh-live/examples/moq_only.rs`. Those work when reading the Markdown
on GitHub, but the site only contains `docs/`, so this rewrites every relative
link whose target resolves outside the book source, or to a Markdown file
that is not a chapter (such as `SUMMARY.md`), to a GitHub URL. Links to
chapters, absolute URLs and anchors are left alone.
"""

import json
import os
import posixpath
import re
import sys

REPO = "https://github.com/n0-computer/iroh-live"
BRANCH = "main"
DOCS_DIR = "docs"

# Inline links `](target)` and reference definitions `[label]: target`.
INLINE = re.compile(r"(\]\()([^)\s]+)(\))")
REFDEF = re.compile(r"^(\s{0,3}\[[^\]]+\]:\s*)(\S+)", re.MULTILINE)
FENCE = re.compile(r"^(```|~~~)", re.MULTILINE)


def rewrite_target(target, chapter_dir, chapters):
    if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith(("#", "/")):
        return target
    path, sep, fragment = target.partition("#")
    resolved = posixpath.normpath(posixpath.join(chapter_dir, path))
    if not resolved.startswith("../") and (
        not resolved.endswith(".md") or resolved in chapters
    ):
        return target
    repo_path = posixpath.normpath(posixpath.join(DOCS_DIR, resolved))
    kind = "blob" if "." in posixpath.basename(repo_path) else "tree"
    return f"{REPO}/{kind}/{BRANCH}/{repo_path}{sep}{fragment}"


def rewrite_text(text, chapter_dir, chapters):
    def inline(m):
        return m.group(1) + rewrite_target(m.group(2), chapter_dir, chapters) + m.group(3)

    def refdef(m):
        return m.group(1) + rewrite_target(m.group(2), chapter_dir, chapters)

    # Splits on fence lines so code blocks pass through untouched.
    parts = re.split(r"(^(?:```|~~~).*$)", text, flags=re.MULTILINE)
    out, in_code = [], False
    for part in parts:
        if FENCE.match(part):
            in_code = not in_code
            out.append(part)
        elif in_code:
            out.append(part)
        else:
            out.append(REFDEF.sub(refdef, INLINE.sub(inline, part)))
    return "".join(out)


def chapters_of(items):
    for item in items:
        chapter = item.get("Chapter") if isinstance(item, dict) else None
        if chapter:
            yield chapter
            yield from chapters_of(chapter.get("sub_items", []))


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "supports":
        sys.exit(0)
    _context, book = json.load(sys.stdin)
    # mdBook 0.5 calls the top-level list `items`, 0.4 called it `sections`.
    chapters = [
        c for c in chapters_of(book.get("items", book.get("sections", []))) if c.get("path")
    ]
    paths = {c["path"].replace(os.sep, "/") for c in chapters}
    for chapter in chapters:
        chapter_dir = posixpath.dirname(chapter["path"].replace(os.sep, "/"))
        chapter["content"] = rewrite_text(chapter["content"], chapter_dir, paths)
    json.dump(book, sys.stdout)


if __name__ == "__main__":
    main()
