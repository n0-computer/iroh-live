# vaapi-decode. VAAPI H.264 decode exporting DMA-BUF, and the shared moq-vaapi growth

Branch: moq-upstream/vaapi-decode          PR targets: external `moq-dev/vaapi` repo (VA-layer decode) + moq main (moq-video backend wiring)
Depends on: B1 (frame vocabulary), B3 (`decode::Frame::native()`); shares moq-vaapi with vaapi-encode
Path: two-target contribution. The VA-layer decode work lands in the external
`moq-vaapi` crate (`github.com/moq-dev/vaapi`, overview coordination point 11);
the moq-video decode-backend wiring (the new decode candidate) is a separate
monorepo PR. This is not "Path: A (in-tree)"; only the backend-wiring half is
in the moq monorepo.
Size: L-XL

## Goal

Add a VAAPI hardware H.264 decode backend to moq-video that produces GPU-resident
DMA-BUF frames instead of a CPU download, and grow the `moq-vaapi` crate with the
surface-export and VPP infrastructure that both this backend and the VAAPI encoder
need. This closes moq's single largest decode gap: on any non-NVIDIA Linux machine
moq's only H.264 decode today is software openh264 into CPU I420
(`rs/moq-video/src/decode/backend/mod.rs:89-113`, hardware candidates are macOS
VideoToolbox, Windows Media Foundation, and Linux NVDEC only), so every Intel and
AMD Linux host decodes in software. The DMA-BUF handle the new backend attaches to
each decoded frame is also what gives `decode::Frame::native()` something to return
on Linux, which is the input the out-of-tree renderer imports (requirement U2).

The genuinely missing piece in `moq-vaapi` is the decode stack, not export
infrastructure. Verified against the cargo-cache crate source (`moq-vaapi 0.0.2`,
`~/.cargo/registry/src/index.crates.io-*/moq-vaapi-0.0.2`): the crate already ships
`vaExportSurfaceHandle` surface export (`src/surface.rs:341`,
`pub fn export_prime(&self) -> Result<DrmPrimeSurfaceDescriptor, VaError>`), already
re-exports the `VADRMPRIMESurfaceDescriptor` bindgen types (`src/lib.rs:33-35`), and
already carries a `VAProcPipelineParameterBuffer` VPP wrapper
(`src/buffer/proc_pipeline.rs`). What it has no counterpart for is a stateless H.264
`StatelessDecoder`/`DecodedHandle` decode path: a grep of its `src/` for
`StatelessDecoder`, `fn decode`, or `DecodedHandle` returns nothing, and its
manifest description is "(AI GENERATED) VA-API H.264 hardware encoder"
(`moq-vaapi-0.0.2/Cargo.toml`, `description`). So the shared growth this plan owns is
the decode contribution plus any VPP scale/CSC execution path, not surface export,
which already exists.

The decode contribution is a re-vendor, not a port. `moq-vaapi` does not depend on
the `cros-codecs` crate our decoder is written against; it is a bindgen-vendored trim
of cros-libva and cros-codecs with its own diverged binding types (its deps are
`bindgen`, `pkg-config`, `regex`, `bitflags`, `anyhow`, `thiserror`, with a vendored
`src/bindings.rs` and `libva/` tree). Our
`StatelessDecoder<H264, VaapiBackend<VaapiFrame>>` (`rusty-codecs/src/codec/vaapi/decoder.rs:79`),
typed against crates.io `cros-codecs 0.0.6` (`rusty-codecs/Cargo.toml:39,68`), cannot
drop into moq-vaapi's diverged type surface, so the decode half of cros-codecs must be
re-vendored into moq-vaapi's binding style, not lifted across intact.

## Evidence

- moq has no VAAPI decode backend of any kind: `comparisons/codecs.md:60-65` (decode
  backend grid), `comparisons/maps/moq-video.md:736-748` (decode coverage matrix,
  Linux H.264 = NVDEC or openh264 only), `comparisons/zerocopy.md:246-261` (verdict
  "ours, and it is not close").
- The upstream requirement U2 and the moq-side change 4 that drive this plan:
  `comparisons/zerocopy.md:440-455`, `comparisons/moq-changes.md:223-235`,
  `comparisons/moq-changes.md:656` (change 4 sizing L-XL, moq-vaapi ~1000-1400 plus
  backend ~900).
- moq-vaapi is a separate external crates.io crate (`github.com/moq-dev/vaapi`, not in
  the moq monorepo), "vendored from cros-libva + cros-codecs", shipping an encoder plus
  surface export plus a VPP wrapper but no decode stack: `comparisons/maps/moq-video.md:23-27`,
  verified at `rs/moq-video/Cargo.toml:34,95` (`vaapi = ["dep:moq-vaapi"]`,
  `moq-vaapi = { workspace = true, optional = true }`), the workspace pin
  `Cargo.toml:95` (`moq-vaapi = "0.0.2"`), and the crate source
  (`moq-vaapi-0.0.2/src/surface.rs:341` export, `src/lib.rs:33-35` DRM PRIME types,
  `src/buffer/proc_pipeline.rs` VPP wrapper, `src/encode.rs` encoder, no `decode` module).
- Our reference decoder and the two hard-won details: `comparisons/codecs.md:164-172`,
  `comparisons/maps/rusty-codecs.md:234`.

## moq API consumed

- The `crate::frame::Frame::DmaBuf(dmabuf::Frame)` variant added by B1
  (`rs/moq-video/src/frame.rs:23-36`), gated
  `cfg(all(target_os = "linux", feature = "dmabuf"))` (the `vaapi` feature enables
  `dmabuf`, so this backend sees the variant), with its `to_i420` CPU-download arm
  so the container's CPU consumers keep working.
- The public `Native::DmaBuf(DmaBuf)` vocabulary from B1 (the on-demand `export()`
  shape), which `decode::Frame::native()` (B3) returns for a `Frame::DmaBuf`.
- The decode `Backend` trait and `Candidate` table as they stand today
  (`rs/moq-video/src/decode/backend/mod.rs:81-114`): a `Candidate` carries
  `supports: fn(Codec) -> bool` and `open: fn(Codec, &Config) -> Result<Box<dyn
  Backend>, Error>`, and `Decoded { timestamp: Timestamp, frame: Frame }` already
  carries the per-picture presentation timestamp, so no encode-style PTS change is
  needed on the decode side.

## Source to port

- `rusty-codecs/src/codec/vaapi/decoder.rs` (1,188 LOC) is the reference
  implementation for the re-vendor. Because moq-vaapi uses diverged bindgen types
  rather than the `cros-codecs` crate, this decoder cannot be lifted across as source;
  its logic and the two hard-won details below are the specification that the
  re-vendored moq-vaapi decode module must reproduce against moq-vaapi's own bindings.
  Verified
  H.264 only: `grep -ri "h265\|hevc"` over `rusty-codecs/src/codec/vaapi/` returns
  nothing, and the decoder is typed `StatelessDecoder<H264, VaapiBackend<VaapiFrame>>`
  (`decoder.rs:79`). There is no H.265 decode to carry; the backend registers for
  `Codec::H264` alone.
- Carries over:
  - The cros-codecs stateless H.264 decode loop and its DMA-BUF-backed frame pool
    (`GenericDmaVideoFrame` surfaces created with `USAGE_HINT_DECODER |
    USAGE_HINT_EXPORT`, `decoder.rs:319-339`).
  - The two hard-won details this plan must preserve verbatim:
    - `vaSyncSurface` before `vaExportSurfaceHandle`, because the export does not
      synchronize; without the sync the exported DMA-BUF can reference an in-progress
      decode and glitch. The sync sits inside `extract_dma_buf_info`
      (`decoder.rs:247-311`, the sync call and its comment at `decoder.rs:269-275`,
      immediately before `surface.export_prime()` at `decoder.rs:277`).
    - Per-frame export caching: the PRIME export is memoized in a
      `OnceCell<Option<CachedDmaBufExport>>` on the frame (`CachedDmaBufExport` at
      `decoder.rs:85-94`, the `OnceCell` field on `VaapiGpuFrame` at
      `decoder.rs:102-113`), so repeated handle access from the render thread and the
      playout buffer dup the same fd (`decoder.rs:228`, `export.fd.try_clone()`)
      instead of re-running surface re-import, sync, and export each time.
  - The Baseline `constraint_set0_flag` SPS patch that fixes a real cros-codecs
    profile-mapping failure (`patch_baseline_constraint_flag`, `decoder.rs:37-76`):
    cros-codecs requires `constraint_set0_flag = 1` for `profile_idc = 66` to map to
    `VAProfileH264ConstrainedBaseline`, and openh264 and some encoders emit Baseline
    without it, producing "Could not get VAProfile!".
  - `reset()` for post-loss reinit (`decoder.rs:731`) and `burst_size()` for DPB
    flush (`decoder.rs:616`). moq's decode `Backend` trait
    (`rs/moq-video/src/decode/backend/mod.rs:67-75`) has neither method today; both
    are needed by a stateful hardware decoder and must be threaded into moq's trait
    or handled inside `decode(...)` (see Coordination).
  - The `vaDeriveImage`-based NV12 plane mapping used for the CPU fallback download
    (`derive_nv12_planes`, `decoder.rs:127` onward), which the map notes handles
    Intel 4-tile formats the generic mapper cannot.
- Dropped:
  - Our `GpuFrameInner`/`NativeFrameHandle` trait glue and the `DmaBufInfo` struct;
    moq's `dmabuf::Frame` (B1) is the target type. Note the export path itself is not
    new work in moq-vaapi: `surface.rs:341` `export_prime()` already wraps
    `vaExportSurfaceHandle`, so the re-vendored decode module calls the crate's
    existing export rather than adding one. What ports is the surrounding usage
    (sync-before-export, per-frame caching), reproduced against moq-vaapi's bindings.
  - The RGBA convenience conversion (`nv12_to_rgba_data` import, `decoder.rs:33`);
    that is presentation logic and belongs in the renderer, not the decode backend.
  - Our `config.rs` catalog mirror; the backend reads hang catalog types via moq's
    `Codec`/`Config` as the existing candidates do.
  - `anyhow` return types; contributed code returns moq's `Error`.

## Target in moq

- New file `rs/moq-video/src/decode/backend/vaapi.rs`. A `pub(crate) struct Vaapi`
  implementing the decode `Backend` trait, with `pub(crate) const NAME: &str =
  "vaapi";` and an `open(codec: Codec, config: &Config) -> Result<Box<dyn Backend>,
  Error>` constructor, mirroring the shape of
  `rs/moq-video/src/decode/backend/openh264.rs`. Its `decode(...)` returns
  `Vec<Decoded>` whose `frame` is `Frame::DmaBuf(...)`.
- One additive `const Candidate` in `rs/moq-video/src/decode/backend/mod.rs`, in the
  `HARDWARE` slice (`mod.rs:89-108`), gated
  `cfg(all(target_os = "linux", feature = "vaapi"))`, with `supports: |c|
  matches!(c, Codec::H264)` and `open: vaapi::Vaapi::open`. This is the coordination
  point 2 additive-table edit; add only this row.
- `moq-vaapi` crate growth (this plan owns it, coordination point 3, targeting the
  external `moq-dev/vaapi` repo per overview coordination point 11). Surface export
  already exists (`surface.rs:341` `export_prime()`, `lib.rs:33-35` DRM PRIME types),
  as does the VPP `VAProcPipelineParameterBuffer` wrapper
  (`buffer/proc_pipeline.rs`), so the additive work is the decode stack: a re-vendored
  stateless H.264 `StatelessDecoder`-equivalent decode path expressed in moq-vaapi's
  diverged bindings, plus the caller-side reader that turns an exported
  `VADRMPRIMESurfaceDescriptor` into the fd, modifier, fourcc, coded and display size,
  and per-plane offset and pitch, plus any VPP scale/CSC execution path (context, caps,
  submit) not yet present. Because `moq-vaapi` is a separate external crate
  (`Cargo.toml:95`), this is a PR to `moq-dev/vaapi` and a maintainer-cut release, not
  an in-tree moq-video edit; the moq-video PR then bumps the pin.
- Dependency-spine decision (coordination point 11, a maintainer conversation, not a
  leaf-agent choice). Our decoder is written against the crates.io `cros-codecs 0.0.6`
  crate, which moq-vaapi does not depend on. Before decode can be contributed, the
  spine must be chosen: (i) re-vendor the cros-codecs decode half into moq-vaapi's
  binding style (matches the crate's current provenance, but grows the AI-generated
  divergence to maintain), (ii) add a real `cros-codecs` dependency to moq-vaapi and
  retire the vendored fork for the decode path (cleaner, but large and needs maintainer
  buy-in), or (iii) another route such as moq-video depending on `cros-codecs`
  directly. This plan does not pick one unilaterally; it surfaces the decision for the
  maintainer.
- `rs/moq-video/src/frame.rs` gains the `dmabuf` module backing `Frame::DmaBuf` (owned
  by B1; this plan consumes it and, if B1 leaves the backing type a stub, fills in the
  exporter that mints the fd on demand).

## Implementation steps

1. Confirm the B1 `Frame::DmaBuf(dmabuf::Frame)` variant and the public
   `Native::DmaBuf`/`DmaBuf::export()` shape are in the base branch. If the backing
   `dmabuf::Frame` cannot express the on-demand export (holding the VA surface and
   dup'ing an fd per `export()` call, our `decoder.rs:85-113,228` design), stop and
   file the gap against B1 (coordination point 1); do not invent a divergent type.
2. Grow `moq-vaapi` (coordination point 3, external repo per coordination point 11):
   settle the dependency-spine decision above with the maintainer, then re-vendor the
   decode half of cros-codecs (a stateless H.264 decode path) into moq-vaapi's diverged
   bindings. Reuse the crate's existing `export_prime()` (`surface.rs:341`) and DRM
   PRIME types rather than adding export; add only the caller-side
   `VADRMPRIMESurfaceDescriptor` reader and any missing VPP execution path. Agree the
   crate's public surface with the maintainer, land the PR in `moq-dev/vaapi`, have the
   maintainer cut a new release, and bump the pin at `rs/moq-video/Cargo.toml` and the
   workspace `Cargo.toml:95`. This is the largest and most resistance-prone piece;
   treat it as the critical path.
3. Write `decode/backend/vaapi.rs`: construct the shared `Display`, the
   `GenericDmaVideoFrame` pool sized from `StreamInfo::coded_resolution`, and the
   `StatelessDecoder<H264, ...>`. Port `patch_baseline_constraint_flag`
   (`decoder.rs:37-76`) as a private helper run over each incoming Annex-B access
   unit before it reaches the decoder.
4. Implement `decode(access_unit, timestamp, keyframe) -> Result<Vec<Decoded>,
   Error>`: feed the patched Annex-B unit, drain ready pictures, and for each wrap the
   pooled surface as a `Frame::DmaBuf` whose exporter caches its PRIME descriptor.
   Carry the input timestamp onto each `Decoded` (H.264 stateless is one-in one-out
   here; echo it), matching the openh264 backend.
5. Port the export path into the `dmabuf::Frame` exporter: `to_native_handle` re-import
   (`decoder.rs:253-268` with its `catch_unwind` guard against cros-codecs' panic on
   EMFILE), then `surface.sync()` before `surface.export_prime()`
   (`decoder.rs:269-283`), then fill moq's DMA-BUF handle from the first layer and
   object (`decoder.rs:290-310`). Preserve the `OnceCell` memoization so a second
   `export()` dup's the cached fd.
6. Thread `reset()` and `burst_size()` (`decoder.rs:731,616`). moq's decode `Backend`
   trait has neither; either add them as defaulted trait methods (additive) or fold
   the reset-on-loss and DPB-flush behavior into `decode(...)`. If a trait change is
   needed, file it against B3/the base plan (coordination point 1) rather than
   editing the trait unilaterally.
7. Add the `const Candidate` row to `decode/backend/mod.rs` (coordination point 2).
8. Gate everything on `cfg(all(target_os = "linux", feature = "vaapi"))` and confirm a
   non-Linux and a `vaapi`-off build still compiles, matching moq's compile-everywhere
   posture.

## Tests

- A hardware round-trip test in the style of moq's NVDEC hardware test
  (`rs/moq-video/src/decode/backend/nvdec.rs:666`, per
  `comparisons/zerocopy.md:280`): encode a known pattern to H.264, decode it through
  the VAAPI backend, assert the output frame is `Frame::DmaBuf` (that
  `decode::Frame::native()` returns `Some(Native::DmaBuf(_))`), and assert the CPU
  `into_i420()` fallback matches the source within tolerance. Mark it `#[ignore]` with
  a reason if the CI runner has no Intel or AMD VA-API device, exactly as the plan's
  hardware-gating rule requires; validate locally on Intel Meteor Lake hardware.
- A Baseline-SPS unit test over `patch_baseline_constraint_flag` asserting
  `profile_idc = 66` gains `constraint_set0_flag` and other NAL types are untouched
  (this is pure bit manipulation, no hardware).
- An export-caching assertion: two `export()` calls on one frame yield two valid but
  distinct fds without a second surface re-import (observable via the `OnceCell` being
  populated once).

## Adaptation notes

- Timestamps are `moq_net::Timestamp` on `Decoded`; keep any internal `Duration`
  behind the boundary.
- Errors are moq's `#[non_exhaustive]` `Error`; add a `SurfaceExport` (and, if not
  already present from the encoder, a `DmaBufImport`) variant additively rather than
  returning `anyhow`.
- No ffmpeg. dlopen or degrade cleanly: `moq-vaapi 0.0.2` hard-links libva today
  (`rs/moq-video/Cargo.toml:89-93`, #1837), so the decode growth should not regress
  that and ideally moves toward dlopen so a libva-less host falls back instead of
  failing to load.
- The backend is `Box<dyn Backend>` where `Backend: Send`; the cros-codecs objects are
  `!Send` (libva `Rc`), so use a documented `unsafe impl Send` justified by
  thread-confinement, matching the encode VAAPI backend's pattern
  (`rs/moq-video/src/encode/backend/vaapi.rs:37-40`).

## Coordination

- Coordination point 1 (base API freeze): if `Frame::DmaBuf`'s backing type or the
  `Native::DmaBuf` export shape cannot express on-demand export, or if the decode
  `Backend` trait needs `reset()`/`burst_size()`, stop and file against B1/B3.
- Coordination point 2 (shared candidate table): add only the single VAAPI row to
  `decode/backend/mod.rs`; do not refactor the table. Later leaves rebase.
- Coordination point 3 (shared moq-vaapi): this plan owns the moq-vaapi decode-stack
  growth. Surface export (`surface.rs:341`) and the VPP parameter-buffer wrapper
  (`buffer/proc_pipeline.rs`) already exist and are reused, not authored. vaapi-encode
  reuses the same existing export/VPP and must not duplicate the decode work;
  coordinate the shared moq-vaapi API shape with the encode agent.
- Coordination point 11 (external moq-vaapi repo, overview): the VA-layer decode work
  is a PR to `github.com/moq-dev/vaapi`, a separate repository under the maintainer's
  org with its own review path and release cadence. We do not hold publish rights to
  the crate name; the critical path runs through the maintainer's release, followed by
  the moq-video pin bump. The dependency-spine decision (re-vendor vs. `cros-codecs`
  dependency vs. another route) is settled here, with the maintainer, before code.
- Licensing and provenance: moq-vaapi already carries `LICENSE.libva` and
  `LICENSE.cros-codecs` alongside its BSD-3-Clause `LICENSE`, and is self-described as
  derived from discord/cros-libva and discord/cros-codecs. Our re-vendored decode half
  originates from the same cros-codecs BSD-3-Clause source, so the contribution matches
  the crate's existing provenance and license; carry the same attribution.

## Transcode and rate control (overview coordination point 7)

The decode side feeds moq-transcode's decode-once plus GPU-resize fanout. Our
VAAPI decode exporting `Frame::DmaBuf` plus VPP GPU scale is the Intel and AMD
analog of the NVDEC free-scaling that fanout uses on NVIDIA, so VAAPI decode into
VPP scale into VAAPI encode is a full Intel and AMD per-segment transcode
pipeline. `reset()` and `burst_size()` support the per-group fetch and seek
pattern, and where moq-transcode requests scaled decode, that maps to our VPP
scale. This is alignment with the maintainer's per-segment transcoding goal, not
a collision.

## Acceptance checklist

- `moq-vaapi` grown with the re-vendored decode stack (reusing its existing
  `export_prime()` and VPP wrapper), landed in `moq-dev/vaapi`, released, and pinned in
  moq-video, with the dependency-spine decision and API shape agreed with the maintainer.
- `decode/backend/vaapi.rs` decodes H.264 to `Frame::DmaBuf`; `decode::Frame::native()`
  returns `Some(Native::DmaBuf(_))` for its output.
- `vaSyncSurface`-before-export and per-frame export caching both preserved and
  covered by tests.
- Baseline SPS patch ported and unit-tested.
- The `const Candidate` row is the only edit to `decode/backend/mod.rs`.
- Non-Linux and `vaapi`-off builds compile; the hardware round-trip test passes on
  Intel/AMD or is `#[ignore]`d with a stated reason.
- Conventional commit with the `moq-video` (and `moq-vaapi`) scope.
