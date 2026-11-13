# Documentation site

This directory builds the iroh-live documentation site from the Markdown in
[`docs/`](../docs). The pages stay the single source and read the same on
GitHub; nothing here changes them.

```text
docs-site/
  book.toml                 mdBook configuration, with `src = "../docs"`
  theme/iroh-live.css       palette, typography, code blocks, sidebar
  theme/iroh-live.js        sidebar wordmark, "Dark" label, edit link
  theme/head.hbs            extra <head> tags
  theme/logo.svg            the logo, also used as the favicon
  landing/index.html        the landing page, one self-contained file
  preprocessors/repo-links.py   points links that leave docs/ at GitHub
  build.sh                  builds build/ (landing page + build/book/)
  check-links.py            checks the links between built pages
```

## Build and serve

The only tool is [mdBook](https://rust-lang.github.io/mdBook/) 0.5, plus the
`python3` that every Linux and macOS machine has:

```sh
cargo install --locked mdbook@0.5.2
cargo make docs-site          # build into docs-site/build and check links
cargo make docs-site-serve    # build and serve on http://127.0.0.1:3000/
```

`docs-site/build.sh [--serve [PORT]]` does the same without cargo-make. The
landing page is at `/` and the book at `/book/`. While editing pages,
`mdbook serve docs-site` serves the book alone with live reload.

The [Docs site workflow](../.github/workflows/docs-site.yml) builds the site
and deploys it to GitHub Pages. It runs only when started by hand from the
Actions tab, and Pages has to be set to deploy from GitHub Actions.

## Why mdBook with a theme

The docs were already laid out for mdBook (`SUMMARY.md`, relative `.md`
links), so mdBook reads them unchanged. The alternatives cost more than they
add:

- **Zola** needs front matter on every page and its own section layout, so
  the Markdown would stop being a plain book that GitHub renders.
- **Astro Starlight** looks good out of the box, but it brings a Node
  toolchain, a `package.json` with a few hundred transitive dependencies to
  keep updated, and its own content conventions.

mdBook is one Rust binary with built-in search, a light and dark theme,
syntax highlighting and edit links. The theme here only overrides mdBook's
CSS variables and adds a stylesheet and a small script, so it uses mdBook's
stock templates and an mdBook upgrade does not need a template merge. If an
upgrade renames a CSS variable or a sidebar class, the fix is in
`theme/iroh-live.css`. The landing page is plain HTML and CSS, apart from
mdBook, and shares the theme choice with the book through the
`mdbook-theme` key in local storage.

No page uses mermaid, so the ASCII diagrams stay as they are.

## Links that leave the book

Some pages link to examples or crate directories with relative paths such as
`../../iroh-live/examples/moq_only.rs`, which work on GitHub but point
nowhere on the site. The `repo-links` preprocessor rewrites those, and links
to Markdown files that are not chapters (such as `SUMMARY.md`), to GitHub
URLs on `main`.
