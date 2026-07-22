# B5. Adaptation conventions + moq Error variants

Branch: moq-upstream/base (the Error variants ride whichever base or leaf PR first needs them; this file itself produces no standalone PR)          PR target: base branch, then moq main
Depends on: none
Path: shared (a reference every leaf cites, plus the concrete moq-side `Error` additions a full upstreaming needs)
Size: S (the `Error` variants are a few lines each; the checklist is documentation)

## Goal

Be the single shared reference every leaf follows so contributions arrive in moq's
shape rather than ours, and enumerate the concrete moq-side `Error` variant
additions a full codec and capture upstreaming needs (change 9). This is not a
standalone big PR: the house-style checklist is a doc leaves cite, and the `Error`
variants are additive on moq's `#[non_exhaustive]` enum, so each variant lands in
whichever leaf first raises that failure mode rather than in one upfront PR. B5
exists so no leaf re-derives the conventions and so the `Error` surface grows
coherently instead of one ad-hoc variant per leaf.

## Evidence

- The house rules are moq's own posture, sourced in `comparisons/moq-changes.md`
  section 4 ("The evidence from moq's code and posture") and section 3, and in the
  overview's "Adaptation conventions". moq removed ffmpeg entirely and replaced it
  with `yuv`, `fast_image_resize`, `zune-jpeg`, and `v4l` (`rs/moq-video/Cargo.toml`
  per section 4); NVENC is an in-tree fork, fully dlopen'd so a GPU-less builder
  links and a driverless host falls back; openh264 is vendored and always compiled;
  moq-vaapi is a trimmed vendor of cros-libva plus cros-codecs; dependencies are
  crates.io only.
- moq's `Error` is a `#[non_exhaustive]` `thiserror` enum with actionable variants
  and no `anyhow` in public signatures (`rs/moq-video/src/error.rs`, re-exported at
  `rs/moq-video/src/lib.rs:57`): `NoEncoder(String)`, `NoDecoder(String)`,
  `UnsupportedCodec(String)`, `Unsupported(String)`, `InvalidFramerate(u32)`,
  `BitrateUnsupported(&'static str)`, `Codec(#[from] anyhow::Error)` as the
  catch-all, plus `Mux`, `Net`, and `TimeOverflow`
  (`comparisons/traits-api.md` section 2.0).
- The `set_bitrate` honesty contract is moq's, not ours: `Backend::set_bitrate` has
  no default and must return `Error::BitrateUnsupported` rather than a silent no-op
  (`encode/backend/mod.rs:45-53`), and the rate loop retires a backend on that error
  (`encode/producer.rs`). Ours defaults to `Ok(())` (`rusty-codecs/src/traits.rs:352-354`),
  which section 8 D7 of `comparisons/traits-api.md` calls a footgun.
- Verified against `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`.

## moq API consumed

Every frozen-contract type indirectly: B5 is the checklist for how a leaf adapts our
code onto `Native` (B1), `Packet` and the `encode` timestamp (B2), `native()` (B3),
and, on Path B, the registration API (B4). It also specifies the additive `Error`
variants leaves return.

## Source to port

Not a code port. B5 is the mapping rules from our vocabulary to moq's, applied by
every leaf to its own source. Our `anyhow::Result` trait methods
(`rusty-codecs/src/traits.rs`), our `Duration` timestamps
(`rusty-codecs/src/format.rs:583, 393, 415`), our `config.rs` catalog mirror
(`rusty-codecs/src/config.rs`, broken against hang 0.19.5 per
`comparisons/traits-api.md` section 4.1), and our default-`Ok` `set_bitrate` are the
things B5 tells leaves to drop or convert.

## The house-style checklist (every leaf follows this)

1. **No ffmpeg anywhere, including tests.** The merge removed it; nothing
   reintroduces it. In-crate round trips use moq's own backends (openh264 encode to
   openh264 decode) as ground truth, as moq does.
2. **dlopen system libraries** (libva, libdrm, V4L2, the NDK); link nothing that can
   fail to load. A backend must build on a host without the hardware and degrade
   cleanly, matching moq-nvenc's compile-everywhere stub: a missing driver falls
   back through `backend::open` rather than failing the process load. (moq-vaapi
   currently links rather than dlopens libva, tracked upstream in #1837; a
   contributed VAAPI leaf should prefer dlopen or note the gap.)
3. **Minimal dependencies, crates.io only.** release-plz owns versions; no git
   dependencies. Our rav1d fork pin and cpal git pin are unacceptable as-is and are
   prerequisites to resolve before the relevant leaf (av1-software) lands
   (coordination point 4).
4. **Timestamps are `moq_net::Timestamp` at every boundary, never `Duration`.**
   Internally an OS-thread pipeline may keep `Duration` behind the seam, but no
   contributed signature carries it. This is a mechanical rename across every
   backend.
5. **Configs come from hang's catalog types, not our `config.rs` mirror.** The
   mirror no longer compiles against hang 0.19.5 (the `displayRatio` to
   `displayAspect` rename, plus missing H.265/VP9 and four fields). Contributed code
   uses `hang::catalog::VideoConfig`/`AudioConfig` directly, as moq's `Decoder::new`
   and `Encoder::catalog` already do; the `From` mirror is discarded.
6. **Errors adopt moq's `Error` with additive variants.** No `anyhow` in a
   contributed public signature; return moq's `#[non_exhaustive]` `Error`, adding a
   variant when a failure mode is genuinely new (see the variant list below).
7. **Public configs are `#[non_exhaustive]`.** Build via `default()`/`new()` and set
   fields, so new options stay additive. Audio formats mirror WebCodecs
   `AudioData.format`. No backend type appears in any public API (`lib.rs:37-44`).
8. **Honest capability contracts.** `set_bitrate` succeeds or returns
   `Error::BitrateUnsupported`, never a silent no-op. Every encoder supports
   per-frame forced IDR (the `keyframe: bool` argument). Every backend that can be
   tested on real hardware ships a hardware round-trip test in the style of moq's
   VideoToolbox and NVENC tests, marked `#[ignore]` with a reason where the CI
   runner lacks the hardware.
9. **Conventional commits with crate scope and `!` for breaking changes.**

## The moq-side Error variant additions (change 9)

These are additive on the `#[non_exhaustive]` `Error` enum (`rs/moq-video/src/error.rs`),
so each lands in the leaf that first raises it, not in one upfront PR. A leaf that
needs one adds it there and cites B5; B5 keeps the list coherent so two leaves do
not add near-duplicate variants.

- `SurfaceExport(String)` (or a structured variant): a DMA-BUF or VA-surface export
  failed (`vaExportSurfaceHandle`, the PRIME descriptor build). Raised by B1's
  `DmaBuf::export`, the vaapi-decode leaf, and pipewire capture.
- `DmaBufImport(String)`: importing a foreign DMA-BUF into a backend failed (an
  unsupported modifier, a plane-layout mismatch). Raised by the vaapi-encode leaf's
  `import_prime` path and any zero-copy consumer.
- Reuse existing variants where they fit rather than adding new ones:
  `BitrateUnsupported(&'static str)` for an encoder that cannot retune,
  `UnsupportedCodec(String)` for a catalog codec no backend serves, `InvalidFramerate`
  and `Unsupported(String)` for construction-time rejections, and `Codec(#[from]
  anyhow::Error)` as the catch-all for a vendor FFI error that does not warrant its
  own variant. The audio Opus work (opus-improvements leaf) touches moq-audio's
  parallel `Error`, not moq-video's, and reserves a `decode_lost`-shaped PLC/FEC
  entry point rather than adding a variant.

## Implementation steps

Not a PR of its own. Leaves apply this reference. The only moq-side edits B5
prescribes are the `Error` variants, and each rides the leaf that first needs it:
the vaapi-decode leaf adds `SurfaceExport`, the vaapi-encode leaf adds
`DmaBufImport`, and so on. When a leaf adds a variant it updates this list so the
next leaf reuses rather than duplicates.

## Tests

None of its own. B5 is verified by every leaf's PR passing the checklist: no ffmpeg,
dlopen-and-degrade builds on hardware-less hosts, `moq_net::Timestamp` boundaries,
hang catalog types, moq `Error`, `#[non_exhaustive]` configs, honest `set_bitrate`,
per-frame forced IDR, and a hardware round-trip test (or an `#[ignore]` with a
reason).

## Adaptation notes

- B5 is deliberately a reference, not a big PR. Bundling all the `Error` variants
  and a "conventions" doc into one moq PR would be a doc-only change with no code,
  which moq would rightly question; the variants are more reviewable riding the leaf
  that exercises them. Cite B5 from each leaf's Adaptation notes rather than
  duplicating the checklist.
- Where the checklist and a leaf conflict (a leaf genuinely needs a `Duration`
  boundary, a git dependency, a backend type in a signature), that is a coordination
  point 1 stop: the leaf files the conflict against B5 or the relevant base plan
  rather than diverging silently.

## Coordination

- Coordination point 1 (base API freeze): B5 is the shared style contract; a leaf
  that cannot follow it files against B5.
- Coordination point 4 (rav1d fork): checklist item 3 (crates.io only) blocks
  av1-software until the rav1d git-fork pin resolves; B5 records the rule, the leaf
  enforces it.
- Coordination point 2 (shared candidate tables) and point 3 (shared moq-vaapi) are
  leaf-level; B5 only reminds leaves that a candidate-table or moq-vaapi edit beyond
  a leaf's own additive entry is a stop.

## Acceptance checklist

- [ ] Every leaf PR cites B5 and satisfies the nine checklist items.
- [ ] No contributed public signature carries `anyhow` or `Duration`.
- [ ] Contributed configs are `#[non_exhaustive]` and use hang catalog types.
- [ ] `set_bitrate` is honest (no silent no-op) and every encoder supports forced IDR.
- [ ] Each new `Error` variant is additive, rides the leaf that raises it, and is
      recorded in the variant list here so no leaf duplicates one.
- [ ] Every hardware-testable backend ships a round-trip test or an `#[ignore]` with
      a stated reason.
