# Contributing

This page points a contributor at the rest. Issues go in the
[tracker](https://github.com/n0-computer/iroh-live/issues).

- [DEVELOPMENT.md](../DEVELOPMENT.md) covers the build, the tests, the
  commit rules, the threading and other conventions, and working against a
  local moq checkout.
- The [internals overview](architecture/index.md) maps each crate to the
  page that explains how it works inside.
- [Instrumentation and tests](architecture/devtools.md) explains the test
  layers and the debug overlay.
- [docs-site/README.md](../docs-site/README.md) builds the documentation
  site, with the API docs, and serves it locally.

## The docs

The docs are an [mdBook](https://rust-lang.github.io/mdBook/) under `docs/`,
with [SUMMARY.md](SUMMARY.md) as the table of contents. Each page has one job:

- `getting-started/` takes a new reader from nothing to a running program.
- `concepts/` explains one idea, in under 150 lines.
- `how-to/` does one task, CLI first, then a Rust program, in under 250
  lines.
- `reference/` lists flags, defaults and limits.
- `architecture/` explains how the crates work inside, for contributors.

Rules that keep the pages consistent:

- A fact lives in one page, and other pages link to it. The latency modes
  and the adaptation defaults live in [Player settings](reference/player.md),
  `IROH_SECRET` in [Environment variables and files](reference/environment.md),
  the feature list in [Cargo features](reference/features.md), and the
  cross-compile steps in
  [Publish from a Raspberry Pi](how-to/raspberry-pi.md#cross-compiling).
- The pages use the terms of the [glossary](glossary.md). Always qualify
  "relay" as an iroh relay, the iroh-live relay, a MoQ relay, or a relay
  link.
- A path's id placeholder is `<endpoint id>` in prose, and `<peer>` in
  reference pages that also show `*` and `**` patterns.
- Link text matches the title of the page it links to.
- A page ends with "Where to go next".
- Paragraphs have six sentences or fewer, and sentences stay under 25 words
  where they can. Define a term at its first use.
- In `architecture/`, explain the behavior first. Put file and constant
  names in lists, not in the middle of the explanation.

## Code in the docs

`rust` blocks in the getting-started, how-to and concept pages and in
[Player settings](reference/player.md) are compiled as doctests of
`iroh-live`, through the `guide` module at the end of `iroh-live/src/lib.rs`.
A new page with Rust code goes into its `pages!` list. Hide setup lines with
`#`, mark a program `rust,no_run`, and mark a fragment that cannot compile on
its own `rust,ignore`. Blocks in the other pages are not compiled, so link to
the API docs instead of pasting a signature.

Run these checks before you commit docs:

```sh
cargo test --doc -p iroh-live --all-features
python3 scripts/check-links.py
python3 scripts/check-docs.py
cargo make docs-site
```

The first compiles the guide programs. The second checks every relative
link and anchor in the repository's Markdown. The third checks that each
page with a `rust,no_run` block is in the `pages!` list, that every moq rev
in the docs matches the root `Cargo.toml`, and that every Cargo feature is
in [Cargo features](reference/features.md). The last builds the site, with
the API docs, and checks the links between its pages. A test in
`iroh-live-cli` also checks that the [`irl` CLI reference](reference/cli.md)
names every flag.
