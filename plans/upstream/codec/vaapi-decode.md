# vaapi-decode. VAAPI H.264 decode exporting DMA-BUF, and the shared moq-vaapi growth

Branch: moq-upstream/vaapi-decode          PR target: base branch, then moq main
Depends on: B1 (frame vocabulary), B3 (`decode::Frame::native()`); shares moq-vaapi with vaapi-encode
Path: A (in-tree)
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
on Linux, which is the input the out-of-tree renderer imports (requirement U2). This
plan additionally owns the shared `moq-vaapi` growth (surface export via
`vaExportSurfaceHandle` plus `VADRMPRIMESurfaceDescriptor`, VPP, and the decode half
of cros-codecs), the largest single piece of the whole program and the most
resistance-prone, because the current `moq-vaapi 0.0.2` is an encode-only trim with
no decode path and no surface export at all.

## Evidence

- moq has no VAAPI decode backend of any kind: `comparisons/codecs.md:60-65` (decode
  backend grid), `comparisons/maps/moq-video.md:736-748` (decode coverage matrix,
  Linux H.264 = NVDEC or openh264 only), `comparisons/zerocopy.md:246-261` (verdict
  "ours, and it is not close").
- The upstream requirement U2 and the moq-side change 4 that drive this plan:
  `comparisons/zerocopy.md:440-455`, `comparisons/moq-changes.md:223-235`,
  `comparisons/moq-changes.md:656` (change 4 sizing L-XL, moq-vaapi ~1000-1400 plus
  backend ~900).
- moq-vaapi is an external crates.io crate, encode-only, "vendored from cros-libva +
  cros-codecs": `comparisons/maps/moq-video.md:23-27`, verified at
  `rs/moq-video/Cargo.toml:34,95` (`vaapi = ["dep:moq-vaapi"]`) and the workspace pin
  `Cargo.toml:95` (`moq-vaapi = "0.0.2"`).
- Our reference decoder and the two hard-won details: `comparisons/codecs.md:164-172`,
  `comparisons/maps/rusty-codecs.md:234`.

## moq API consumed

- The `crate::frame::Frame::DmaBuf(dmabuf::Frame)` variant added by B1
  (`rs/moq-video/src/frame.rs:23-36`), gated
  `cfg(all(target_os = "linux", feature = "vaapi"))`, with its `to_i420` CPU-download
  arm so the container's CPU consumers keep working.
- The public `Native::DmaBuf(DmaBuf)` vocabulary from B1 (the on-demand `export()`
  shape), which `decode::Frame::native()` (B3) returns for a `Frame::DmaBuf`.
- The decode `Backend` trait and `Candidate` table as they stand today
  (`rs/moq-video/src/decode/backend/mod.rs:81-114`): a `Candidate` carries
  `supports: fn(Codec) -> bool` and `open: fn(Codec, &Config) -> Result<Box<dyn
  Backend>, Error>`, and `Decoded { timestamp: Timestamp, frame: Frame }` already
  carries the per-picture presentation timestamp, so no encode-style PTS change is
  needed on the decode side.

## Source to port

- `rusty-codecs/src/codec/vaapi/decoder.rs` (1,188 LOC) is the reference. Verified
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
    moq's `dmabuf::Frame` (B1) is the target type. The export body ports, the type it
    fills in does not.
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
- `moq-vaapi` crate growth (this plan owns it, coordination point 3). The current
  crate is encode-only, so the decode half of cros-codecs plus surface export must be
  added: the stateless H.264 decode backend, `vaExportSurfaceHandle` with a
  `VADRMPRIMESurfaceDescriptor` reader producing the fd, modifier, fourcc, coded and
  display size, and per-plane offset and pitch, and the VPP plumbing the encoder half
  reuses. Because `moq-vaapi` is an external crate (`Cargo.toml:95`), this is a
  version bump of that crate, not an in-tree moq-video edit; coordinate the crate's
  API shape with the maintainer before the moq-video PR can pin the new version.
- `rs/moq-video/src/frame.rs` gains the `dmabuf` module backing `Frame::DmaBuf` (owned
  by B1; this plan consumes it and, if B1 leaves the backing type a stub, fills in the
  exporter that mints the fd on demand).

## Implementation steps

1. Confirm the B1 `Frame::DmaBuf(dmabuf::Frame)` variant and the public
   `Native::DmaBuf`/`DmaBuf::export()` shape are in the base branch. If the backing
   `dmabuf::Frame` cannot express the on-demand export (holding the VA surface and
   dup'ing an fd per `export()` call, our `decoder.rs:85-113,228` design), stop and
   file the gap against B1 (coordination point 1); do not invent a divergent type.
2. Grow `moq-vaapi` (coordination point 3): add the decode half of cros-codecs (the
   stateless H.264 `StatelessDecoder` path) and surface export
   (`vaExportSurfaceHandle` plus the `VADRMPRIMESurfaceDescriptor` reader). Agree the
   crate's public surface with the maintainer, cut a new `moq-vaapi` release, and
   bump the pin at `rs/moq-video/Cargo.toml` and the workspace `Cargo.toml:95`. This
   is the largest and most resistance-prone piece; treat it as the critical path.
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
- Coordination point 3 (shared moq-vaapi): this plan owns the moq-vaapi surface-export
  plus VPP plus decode-half growth. vaapi-encode builds on these additions and must
  not duplicate them; coordinate their shape with the encode agent.

## Acceptance checklist

- `moq-vaapi` grown with decode plus surface export, released, and pinned in
  moq-video, with the API shape agreed with the maintainer.
- `decode/backend/vaapi.rs` decodes H.264 to `Frame::DmaBuf`; `decode::Frame::native()`
  returns `Some(Native::DmaBuf(_))` for its output.
- `vaSyncSurface`-before-export and per-frame export caching both preserved and
  covered by tests.
- Baseline SPS patch ported and unit-tested.
- The `const Candidate` row is the only edit to `decode/backend/mod.rs`.
- Non-Linux and `vaapi`-off builds compile; the hardware round-trip test passes on
  Intel/AMD or is `#[ignore]`d with a stated reason.
- Conventional commit with the `moq-video` (and `moq-vaapi`) scope.
