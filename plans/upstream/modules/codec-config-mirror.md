# Catalog config mirror

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path. The config mirror is catalog metadata, not
> a frame surface. See ../zerocopy.md for the paths that do.

## What it is

`rusty-codecs/src/config.rs` (318 LOC) is a transport-agnostic mirror of the hang
catalog types, written so the crate needed no hang dependency. The alignment
makes that reason moot: both moq codec crates depend on hang and pass its catalog
types across the codec boundary unchanged. The mirror is now pure duplication, and
worse, it no longer compiles against hang 0.19.5, so the cut is already forced.
This is a local delete with no moq change; we adopt `hang::catalog` types
directly.

## iroh-live vs moq

Ours: a mirror of the hang catalog. `VideoConfig` (`config.rs:11-33`),
`AudioConfig` (`:38-50`), `VideoCodec { H264(H264), AV1(AV1), Other(String) }`
(`:53-61`), `AudioCodec { Opus, Pcm, Other(String) }` (`:64-72`), with
feature-gated `From` conversions to `hang::catalog` types (`:159-318`).

Theirs: no mirror. Both moq codec crates depend on hang and pass its types
directly, `Decoder::new(&hang::catalog::VideoConfig)` (`moq:decode/decoder.rs:94`)
and `Encoder::catalog() -> hang::catalog::AudioConfig`
(`moq:encode/encoder.rs:263`).

The mirror has drifted from current hang and is already broken against 0.19.5:

- Field rename. hang's `VideoConfig` now names the display-aspect fields
  `display_aspect_width`/`display_aspect_height`, with a `serde(alias =
  "displayRatioWidth"/"displayRatioHeight")` for older catalogs
  (`rs/hang/src/catalog/video/mod.rs:133-138`). Our mirror still uses
  `display_ratio_width`/`display_ratio_height` (`config.rs:24-26`) and our `From`
  impls assign to the renamed destination fields (`config.rs:171,186`), so they no
  longer compile.
- Missing codecs. hang's `VideoCodec` is `H264`, `H265`, `VP9`, `AV1`,
  `Unknown(String)` (`rs/hang/src/catalog/video/codec.rs:13-29`); ours collapses
  H.265 and VP9 into `Other`.
- Missing fields. hang's `VideoConfig` also carries `broadcast`, `container`,
  `jitter`, and `timeline` (`rs/hang/src/catalog/video/mod.rs:98-178`); ours
  carries none and hard-codes `container = Default::default()` and `jitter = None`
  (`config.rs:190-191`).

## What to do

Delete the mirror and use `hang::catalog` types directly.

- Removal (local delete, the local pre-work stage): delete `rusty-codecs/src/config.rs`
  (318 LOC) and its entire `hang_interop` `From` layer (`config.rs:159-318`).
  Replace every mirror type at its use sites with the direct `hang::catalog` type,
  as both moq crates already do. No moq-side change: this is local convergence
  work (D5).
- Sequencing (the codec removal sequencing, step 1): this runs
  first, with the independent local cuts, before any adoption diff has to carry a
  conversion shim. The mirror being already broken against hang 0.19.5 forces the
  cut regardless of schedule.

## Tests

`cargo make check-all` must be green after the type substitution; the mirror not
compiling against hang 0.19.5 means the crate already needs this change to build
once it depends on hang. The pipeline and conformance tests exercise catalog
round-trips through the direct `hang::catalog` types. All CI-verifiable, no
hardware gate.

## Evidence

- ../comparison/codecs.md, final section (the hang 0.19.5 / displayRatio-to-
  displayAspect rename forcing the mirror cut).
- ../comparison/traits-api.md, sections 4.1 (mirror versus hang 0.19.5), 4.2
  (adopting hang directly), and the D5 decision.
- ../comparison/maps/rusty-codecs.md for the `config.rs` inventory.

## Coordination

- No release gate and no upstream leaf: local delete at stage M0, ahead of the
  release bump.
- D5 (drop the catalog-config mirror) is local convergence work that begins
  immediately, independent of any moq contribution.
- The mirror's removal touches every codec that produced a catalog config
  (openh264, VTB, and the deferred AV1 encoder), so their catalog output moves to
  `hang::catalog::VideoConfig`/`AudioConfig` as they are adopted or dropped.
</content>
