#!/usr/bin/env python3
"""Checks the links between pages of the built site.

Walks every HTML file under the given directory (default: docs-site/build)
and reports relative links whose file is missing, or whose `#fragment` names
no id in the target page. External URLs are not fetched. Exits non-zero when
a link is broken.
"""

import html.parser
import os
import sys
import urllib.parse


class Page(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids, self.links = set(), []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        for key in ("id", "name"):
            if attrs.get(key):
                self.ids.add(attrs[key])
        for key in ("href", "src"):
            if attrs.get(key) and tag in ("a", "img", "link", "script"):
                self.links.append(attrs[key])


def main():
    root = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else
                           os.path.join(os.path.dirname(__file__), "build"))
    pages = {}
    for dirpath, _, files in os.walk(root):
        for name in files:
            if name.endswith(".html"):
                path = os.path.join(dirpath, name)
                page = Page()
                with open(path, encoding="utf-8") as f:
                    page.feed(f.read())
                pages[path] = page

    broken = 0
    for path, page in sorted(pages.items()):
        # print.html repeats every page and 404.html uses absolute site URLs.
        if os.path.basename(path) in ("print.html", "404.html", "toc.html"):
            continue
        for link in page.links:
            url = urllib.parse.urlparse(link)
            if url.scheme or link.startswith(("//", "mailto:", "javascript:")):
                continue
            target = path if not url.path else os.path.normpath(
                os.path.join(os.path.dirname(path), urllib.parse.unquote(url.path)))
            if os.path.isdir(target):
                target = os.path.join(target, "index.html")
            if not os.path.exists(target):
                print(f"{os.path.relpath(path, root)}: missing {link}")
                broken += 1
            elif url.fragment and target in pages and url.fragment not in pages[target].ids:
                print(f"{os.path.relpath(path, root)}: no anchor {link}")
                broken += 1
    print(f"checked {len(pages)} pages, {broken} broken links")
    sys.exit(1 if broken else 0)


if __name__ == "__main__":
    main()
