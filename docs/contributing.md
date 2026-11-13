# Contributing

[DEVELOPMENT.md](../DEVELOPMENT.md) covers the build, the tests, the
conventions, and working against a local moq checkout. Issues go in the
[tracker](https://github.com/n0-computer/iroh-live/issues).

## The docs

The docs are an [mdBook](https://rust-lang.github.io/mdBook/) under `docs/`,
with [SUMMARY.md](SUMMARY.md) as the table of contents. Each page has one job:

- `concepts/` explains one idea, in under 150 lines.
- `how-to/` does one task, CLI first, then a Rust program, in under 200
  lines.
- `reference/` lists flags, defaults and limits. A fact lives in one
  reference page, and other pages link to it.
- `architecture/` explains how the crates work inside.

The pages use the terms of the [glossary](glossary.md). Always qualify
"relay" as an iroh relay, the iroh-live relay, or a relay link.

`rust` blocks in the how-to and getting-started pages are compiled as
doctests of `iroh-live`, through the `guide` module at the end of
`iroh-live/src/lib.rs`. Mark a fragment that cannot compile on its own as
`rust,ignore`, and a program as `rust,no_run`. Run both checks before you
commit docs:

```sh
cargo test --doc -p iroh-live --all-features
python3 scripts/check-links.py
```

The link check covers every relative link and anchor in the repository's
Markdown.
