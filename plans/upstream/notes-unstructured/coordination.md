# Cross-cutting coordination: licensing, CI gating, semver, the moq-vaapi repo

> Campaign: upstream (media stack) | Kind: cross-cutting note | Read
> `../overview.md` first. These concerns span several modules, so they live here.

## The moq-vaapi external repo

`moq-vaapi` is a separate external crate (crates.io 0.0.2,
github.com/moq-dev/vaapi), not a crate in the moq monorepo. moq-video depends on
it. It already ships an encoder, `vaExportSurfaceHandle` surface export, and a
VPP `VAProcPipelineParameterBuffer` wrapper, but no decode stack, and its types
are a diverged bindgen trim of cros-libva and cros-codecs that does not use the
`cros-codecs` crate our decoder is written against.

- The VAAPI VA-layer work (a decode stack, plus any export or VPP additions) is
  a PR to that repo; the moq-video backend wiring that consumes it is a separate
  monorepo PR. Track two PR targets per VAAPI module.
- Open question: the dependency spine for the decode contribution (re-vendor
  cros-codecs decode into moq-vaapi's style, add a `cros-codecs` dependency, or
  another route). Current proposal: re-vendor the decode half into moq-vaapi's
  diverged bindgen style, matching how the crate already treats cros-libva. See
  `../modules/codec-vaapi-decode.md`.
- vaapi-decode owns the decode-stack contribution; vaapi-encode contributes the
  validation and hardware-correctness fixes to the existing encode path. If
  authored in parallel they coordinate on the shared moq-vaapi types rather than
  one duplicating the other's export or VPP.

## Licensing and provenance of ported FFI

The VAAPI, V4L2, Vulkan, and Metal code carries libva, DRM, and graphics-API
bindings, each with its own license, and some is ported from cros-libva and
cros-codecs. Every contribution states the provenance and license of what it
vendors or binds, and matches moq's existing posture (moq-vaapi ships
`LICENSE.libva` and `LICENSE.cros-codecs`). Do not introduce a
license-incompatible dependency.

## CI hardware gating

Most of this cannot run in moq CI: there is no Intel or AMD GPU, no Raspberry
Pi, and no Android device on the runners. Every hardware path ships a cfg-gated
round-trip test modeled on moq's own `round_trip` helper
(`decode/backend/nvdec.rs:513`, cfg-gated rather than `#[ignore]`), plus a
reproducible host-validation script, and each module states plainly what CI can
and cannot verify. A backend that only compiles in CI is explicitly marked
unvalidated, as moq's own VAAPI encoder is today. Every validation report sent
upstream carries reproducible scripts and exact environment versions so the
results can be re-run without us.

## Semver across the fan

B1 through B4 change moq-video's public surface, and a string of module PRs
follow. Agree the versioning expectation upstream up front (one base bump, then
additive modules) so the fan of PRs does not thrash the crate version.

## The B4 breaking change and the Android placement

Publishing the `Backend` trait (`../base/B4-backend-trait-registration.md`) is
the only breaking change and is worth it only if moq wants out-of-tree backends.
Open question: the Android placement (in-tree with its NDK build cost, or
external over the registration API). Current proposal: external (Path B), which
is what B4 exists for. Do not open B4 as a PR until the placement is settled
upstream. See `../modules/codec-android-mediacodec.md`.
