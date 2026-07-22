# Adversarial review of the moq upstream plan (2026-07-22)

This is a source-grounded adversarial review of the whole `plans/upstream/`
campaign: the overview, the five base plans, the ten codec leaves, the three
capture leaves, and the render leaf. Four specialist reviewers cross-checked
every load-bearing claim against real source (moq at HEAD `3a3e0ea8`, the
iroh-live working tree, and the `moq-vaapi` crate in the cargo cache). Their
full per-area findings are in `plans/upstream/review-0722-base.md`,
`review-0722-vaapi.md`, `review-0722-backends.md`, and
`review-0722-capture-meta.md`. This document consolidates them, resolves one
cross-reviewer contradiction against source, and gives a prioritized fix list.

## Verdict

The plans are anchor-accurate and the analysis behind them holds: nearly every
cited `file:line` resolves and says what the plan claims, the LOC counts match,
and the base API changes B1 through B4 are confirmed genuinely absent from moq
today, so the campaign is not built on stale assumptions. The problems are not in
the analysis; they are in buildability and in two structural premises. Two things
must change before the campaign launches: the base-branch-then-leaf git model
does not work for an external contribution, and the VAAPI sub-tree rests on a
wrong picture of the `moq-vaapi` crate. Beyond those, the base plans need real
buildability detail (the `moq-frame` home, the feature graph, the candidate-tier
flag) and a set of missing details across the leaves. None of this invalidates
the campaign; it sharpens it.

Counts across the four reviews: 6 blocking, 24 substantive, and roughly 20 nits
(line drifts, catalogued in the per-area files).

## The two structural corrections

### 1. The git and PR model does not work as written (blocking, meta M2)

The overview proposes a base branch off moq main with leaf branches off the base,
leaves targeting the base branch until base merges. That works for a private
worktree, not for contributing to an external project. A pull request to upstream
moq cannot target a branch on our fork; it targets moq's `main`. A leaf opened
before base merges would render as the union of the B1 through B5 diff plus the
leaf diff, which cannot be reviewed in isolation. The realistic model is:

- Base (B1, B2, B3, B5) lands on moq `main` first, as its own PR or short series.
  Wave 0 therefore serializes ahead of every leaf; it is a hard gate, not merely
  a recommendation.
- Each leaf is then a normal PR against moq `main`, rebased onto the merged base.
  Leaves remain independent of each other, subject to the shared-file coordination
  points.
- Locally, a base integration branch is still useful to compile leaves against the
  proposed API before base merges, but that is a development convenience, not the
  PR path.

Fix: rewrite the overview "Git and PR model" section and the wave-0 framing to
state that base must merge upstream before leaves can be reviewed, and that Wave 0
is a serializing gate.

### 2. The `moq-vaapi` reality (blocking; resolves a reviewer contradiction)

The VAAPI reviewer and the meta reviewer contradicted each other on whether
`moq-vaapi` exists. Resolved against source, the VAAPI reviewer is correct:

- `moq-vaapi` is a real dependency. `rs/moq-video/Cargo.toml:34,95` declares
  `vaapi = ["dep:moq-vaapi"]` and `moq-vaapi = { workspace = true, optional = true }`,
  and `encode/backend/vaapi.rs:1` imports `moq_vaapi::encode::{Config, Encoder}`.
- It is a separate external crate (crates.io `0.0.2`, the maintainer's own org),
  not a crate in the moq monorepo. The cargo cache copy carries `LICENSE.libva`,
  `LICENSE.cros-codecs`, a `bindgen_gen.rs`, and a vendored `libva/` tree: it is a
  bindgen-vendored trim of cros-libva and cros-codecs with its own diverged types,
  and it does not depend on the `cros-codecs` crate our decoder is written against.
- It already ships an encoder, `vaExportSurfaceHandle` surface export, and a VPP
  `VAProcPipelineParameterBuffer` wrapper. It has no decode stack, and its encode
  path is compile-verified only, "NOT YET VALIDATED ON HARDWARE"
  (`encode/backend/vaapi.rs:19-21`).

The consequences for the VAAPI plans, all of which currently mis-state this:

- The premise "moq-vaapi has no surface export or VPP" is false; both exist. The
  plan's largest claimed piece of work is smaller than stated for those parts.
- moq already ships a VAAPI encode backend over moq-vaapi, so `vaapi-encode` is a
  replacement and validation of an unvalidated path, not an additive new backend.
- The decode contribution is a re-vendor, not a port: moq-vaapi has no decode and
  uses diverged bindgen types, so our cros-codecs-based decoder cannot drop in.
  The dependency-spine decision (re-vendor cros-codecs decode into moq-vaapi's
  style, add a `cros-codecs` dependency to moq-vaapi, or another route) must be
  made and is a maintainer conversation.
- The contribution has two targets: the `moq-dev/vaapi` repo for the VA-layer work
  and the moq monorepo for the moq-video backend wiring. The "Path: A (in-tree)"
  label is wrong; it is partly a separate-repo contribution.

Fix: rewrite `vaapi-decode.md` and `vaapi-encode.md` against the real moq-vaapi
(read the cargo-cache source), correct the export/VPP premises, name the two PR
targets, and add the dependency-spine decision as a coordination point.

## Base-plan buildability (blocking + substantive, from review-0722-base)

B2 is essentially ready: the semantics are confirmed (each `Bytes` is one whole
access unit, so per-packet timestamping is behavior-preserving; `Timestamp` is
`Copy`). B1, B3, and B4 need real detail before they are buildable:

- B1 feature graph is unbuildable: it enables `dmabuf` from a `v4l2` moq-video
  feature that does not exist (moq-video has only `nvenc`/`nvdec`/`vaapi`/`pipewire`;
  V4L2 is unconditional through the `v4l` crate). Fix the enabler set to the
  features that exist, and introduce a `v4l2` feature only if V4L2 EXPBUF wants
  DMA-BUF later.
- B1 `moq-frame` crate home is a dependency cycle: a public `DmaBuf` that holds a
  moq-vaapi exporter and returns moq-video's `Error` and `I420` sits above those
  crates, so it cannot be a leaf crate they depend on. Resolve by choosing the
  in-moq-video module home, or by abstracting the exporter behind a trait and
  giving `moq-frame` its own error type. This is a real design decision to make
  now, since every GPU leaf depends on the outcome.
- B4 loses the hardware/software tier: collapsing the two candidate slices into a
  single `OnceLock<Vec<Candidate>>` drops the tier that `Kind::{Auto,Hardware,
  Software}` selection derives from having two slices, and neither `Candidate` nor
  the proposed `Registration` carries a tier flag. Also, a `OnceLock` cannot be
  appended after first read, so registration needs a `Mutex<Vec<Registration>>`
  staging area consulted at selection time; and publishing `Decoded` in a public
  trait requires its `frame` field to become the public `decode::Frame`, or a
  `pub(crate)` type leaks through a `pub` signature. The phrase "additive-sealed"
  is a contradiction for a registerable trait and should be dropped.

Substantive B1 details to add: the `Native::DmaBuf` arm is gated `target_os =
"linux"` but its exporter only exists under `dmabuf`/`vaapi`, so a no-default
Linux build fails to compile; `native()` builds an owned `Native` from `&self`
but `macos::Surface` and `d3d11::Texture` are not `Clone`; the new GPU frame types
are `!Send`/`!Sync` and need an explicit `unsafe impl` with a safety argument;
`download_i420` for tiled or CCS DMA-BUF and for `AHardwareBuffer` is unspecified
and non-trivial (VA readback keyed on the modifier, or `AHardwareBuffer_lock`);
and the workspace wiring (root members, `[workspace.dependencies] moq-frame`, the
concrete `dmabuf` feature stanzas, the `OwnedFd` import) is omitted.

## Leaf details to add (substantive, from review-0722-backends and -capture-meta)

- v4l2-encode: `libc` is not a declared moq-video dependency; the feature must add
  `dep:libc`, not merely gate a module.
- v4l2-decode: `I420::from_nv12` (`frame.rs:208`) is `#[cfg(target_os = "windows")]`
  and stride-less, so calling it on Linux "honoring per-plane stride" will not
  compile and cannot honor stride; the plan needs its own strided NV12-to-I420
  packer. The ported function is `decoder_thread` (`decoder.rs:162`), not
  `run_decoder`. And no EXPBUF or DMA-BUF path exists anywhere in our V4L2 code, so
  the zero-copy follow-up starts from nothing (conclusion right, evidence wrong).
- av1-software: "pure Rust, compile-everywhere" collides with fork resolution. The
  crates.io `dav1d-rs` wraps C libdav1d (a system dependency, so not
  compile-everywhere), pure-Rust decode is only the forbidden git `memorysafety/rav1d`,
  and the pin's `asm` feature needs NASM. Spell the three options out honestly.
- v4l2-encode / av1 session reuse: the transcode note's session-reuse ask is
  concrete work: V4L2 needs an `EncoderCmd::Reset` path (its `new()` blocks on
  device open plus REQBUFS plus STREAMON, confirming the cost ranking), and
  cros-codecs and rav1e have no in-place reconfigure, so "reuse" means holding the
  session and resetting rate-control state, which must be scoped.
- render: `into_i420(self)` consumes the frame, but the renderer borrows
  (`render(&mut self, &Frame)`), so there is no public borrowing download path; the
  plan hand-waves this. Either `native()`-first with `into_i420()` only on the
  no-handle branch, or a borrowing `to_i420(&self)` accessor is needed.
- vtb-mf-decode-surface: retaining `macos::Surface` past decode silently changes
  its documented "downloaded to I420 before the fanout" `Sync` justification; the
  safety argument must be revisited, not assumed.
- capture: `publish_preencoded` needs its exact signature (mirror `publish_capture`'s
  async form, and resolve whether `encode::Options` still applies when there is no
  encoder), and the `org.freedesktop.portal.Camera` fd and enumeration flow differs
  from ScreenCast and must be specified. `v4l2-camera-enum` cites `v4l2.rs:88-96`
  for YUYV conversion, but that is `Camera::open`; the conversion is at `:146`.

## Cross-cutting concerns missing from the coordination points (substantive, meta M3)

The overview has seven coordination points; four cross-cutting concerns are
absent and should be added:

- Licensing and provenance of ported FFI: the VAAPI, V4L2, and Vulkan or Metal
  code carries libva, DRM, and graphics-API bindings with their own licenses; each
  contribution states the provenance and license of what it vendors or binds.
- CI hardware gating as policy: most of this cannot run in moq CI (no Intel or AMD
  GPU, no Pi, no Android device), so every hardware path ships a cfg-gated
  round-trip test modeled on moq's own `round_trip` helper (`decode/backend/nvdec.rs:513`,
  which is cfg-gated rather than `#[ignore]`) plus a reproducible host validation
  script, and the plan says plainly what CI can and cannot verify.
- Semver across the fan: B1 through B4 change moq-video's public surface, and a
  string of leaf PRs follow; the plan states the versioning expectation so the fan
  does not thrash the version.
- moq-vaapi ownership and PR target: the VA-layer work targets a separate repo
  under the maintainer's org, which is a distinct review path from the monorepo.

## Prioritized fix list

Before the campaign launches:

1. Rewrite the overview git and PR model: base merges upstream first, Wave 0 is a
   serializing gate, leaves are normal PRs against moq main (blocking, meta M2).
2. Rewrite `vaapi-decode` and `vaapi-encode` against the real moq-vaapi: correct
   the export and VPP premises, the re-vendor-not-port decode reality, the two PR
   targets, and the dependency-spine decision (blocking, VAAPI + resolved M1).
3. Fix B1: choose the `moq-frame` home (module vs abstracted crate), fix the
   feature graph to existing features, and add the buildability details (Clone,
   Send or Sync, download_i420, cfg, workspace wiring) (blocking + substantive).
4. Fix B4: add a tier flag to the candidate and registration types, use a `Mutex`
   staging area not a bare `OnceLock`, make `decode::Frame` public for `Decoded`,
   and drop "additive-sealed" (blocking + substantive).
5. Add the four cross-cutting coordination points (licensing, CI hardware gating,
   semver, moq-vaapi PR target) (substantive, meta M3).
6. Fold the leaf details: v4l2 `dep:libc`, the strided NV12-to-I420 packer and the
   `decoder_thread` name, the rav1d/dav1d-rs three options, the render borrowing
   download path, the vtb-mf `Sync` argument, `publish_preencoded`'s signature and
   the portal Camera flow, and the `v4l2-camera-enum` YUYV anchor (substantive).
7. Sweep the nits (line drifts) catalogued in the four per-area review files.

The campaign's structure, dependency tree, and scope survive the review. What it
needs is the git-model correction, the moq-vaapi correction, and a pass to turn
the base plans and a handful of leaves from accurate descriptions into buildable
instructions.
