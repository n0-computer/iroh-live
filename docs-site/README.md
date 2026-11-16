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
  api-index.html            the index of the API docs, copied to build/api/
  preprocessors/repo-links.py   points links that leave docs/ at GitHub
  build.sh                  builds build/: the landing page, build/book/ and build/api/
  check-links.py            checks the links between built pages
```

## Build and serve

The tools are [mdBook](https://rust-lang.github.io/mdBook/) 0.5, the `python3`
that every Linux and macOS machine has, and for the API docs the Rust
toolchain with the system libraries and `npm` that a full build needs (see
[Install](../docs/getting-started/install.md)):

```sh
cargo install --locked mdbook@0.5.2
cargo make docs-site          # build into docs-site/build and check links
cargo make docs-site-book     # the same without the API docs, in seconds
cargo make docs-site-serve    # build and serve on http://127.0.0.1:3000/
```

`docs-site/build.sh [--no-api] [--serve [PORT]]` does the same without
cargo-make. The landing page is at `/`, the book at `/book/` and the API docs
at `/api/`. The API docs are `cargo doc --workspace --all-features --no-deps`
over the library crates, without the CLI and the demos. rustdoc writes no
index page for a workspace, so `api-index.html` is that page. While editing pages,
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

## Links to the API docs

A page links to the API docs by their published URL, such as
`https://n0-computer.github.io/iroh-live/api/iroh_live/`, so the link also
works on GitHub. The `repo-links` preprocessor turns those URLs into relative
links in the built book, and `check-links.py` checks them like any other
link. It skips them when the site was built with `--no-api`.

## Links that leave the book

Some pages link to examples or crate directories with relative paths such as
`../../iroh-live/examples/moq_only.rs`, which work on GitHub but point
nowhere on the site. The `repo-links` preprocessor rewrites those, and links
to Markdown files that are not chapters (such as `SUMMARY.md`), to GitHub
URLs on `main`.
