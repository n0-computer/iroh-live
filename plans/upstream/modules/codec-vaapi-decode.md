# VAAPI H.264 Decode

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: central. This is the only VAAPI decode on either stack and the sole
> source of GPU-resident DMA-BUF decode output on non-NVIDIA Linux; it exports a
> PRIME descriptor per decoded surface and feeds the decode-to-render zero-copy
> path. See ../zerocopy.md, section "2b. Decode to render: verdict ours".

## What it is

A hardware H.264 decode backend for moq-video that produces GPU-resident DMA-BUF
frames instead of a CPU download, plus the growth of the `moq-vaapi` crate with
the stateless decode stack that this backend needs. It closes moq's single
largest decode gap: on any non-NVIDIA Linux machine moq's only H.264 decode today
is software openh264 into CPU I420 (the hardware decode candidates are macOS
VideoToolbox, Windows Media Foundation, and Linux NVDEC only), so every Intel and
AMD Linux host decodes in software. moq has no VAAPI decode of any kind, so the
verdict is move: our 1,188-line cros-codecs stateless decoder is the only
implementation on either side. The DMA-BUF handle the new backend attaches to
each decoded frame is also what gives `decode::Frame::native()` something to
return on Linux, the input the renderer imports (requirement U2).

## iroh-live vs moq

moq has no VAAPI decode backend at all: the decode-coverage matrix shows Linux
H.264 is NVDEC or openh264 only (`../comparison/maps/moq-video.md:736-748`), and
the zero-copy survey records the verdict "ours, and it is not close"
(../zerocopy.md section "2b. Decode to render: verdict ours"). The `moq-vaapi`
0.0.2 crate, verified against the cargo-cache source, already ships
`vaExportSurfaceHandle` surface export (`src/surface.rs:341`,
`pub fn export_prime(&self) -> Result<DrmPrimeSurfaceDescriptor, VaError>`),
already re-exports the `VADRMPRIMESurfaceDescriptor` bindgen types
(`src/lib.rs:33-35`), and already carries a `VAProcPipelineParameterBuffer` VPP
wrapper (`src/buffer/proc_pipeline.rs`). What it has no counterpart for is a
stateless H.264 decode path: a grep of its `src/` for `StatelessDecoder`,
`fn decode`, or `DecodedHandle` returns nothing, and its manifest description is
"(AI GENERATED) VA-API H.264 hardware encoder". So the growth this leaf owns is
the decode contribution plus any VPP scale/CSC execution path, not surface
export, which already exists.

Ours is a full cros-codecs stateless H.264 decode
(`rusty-codecs/src/codec/vaapi/decoder.rs`, 1,188 LOC), producing GPU-resident
DMA-BUF frames with cached PRIME export, a `reset()` for post-loss reinit
(`decoder.rs:731`), `burst_size()` for DPB flush (`decoder.rs:616`), and a
Baseline `constraint_set0_flag` SPS patch fixing a real cros-codecs
profile-mapping failure (`decoder.rs:37-76`). Verified H.264 only:
`grep -ri "h265\|hevc"` over `rusty-codecs/src/codec/vaapi/` returns nothing, and
the decoder is typed `StatelessDecoder<H264, VaapiBackend<VaapiFrame>>`
(`decoder.rs:79`), so the backend registers for `Codec::H264` alone. This is a
re-vendor, not a port: `moq-vaapi` does not depend on the crates.io `cros-codecs`
crate; it is a bindgen-vendored trim of cros-libva and cros-codecs with its own
diverged binding types, so our `StatelessDecoder<H264, VaapiBackend<VaapiFrame>>`
typed against crates.io `cros-codecs 0.0.6` cannot drop into moq-vaapi's diverged
type surface and must be re-expressed against moq-vaapi's bindings. Verdict
"upstream ours" (`../comparison/codecs.md`, section "VAAPI vs VAAPI (+
moq-vaapi)" and verdict-table row; "theirs does not exist; fills moq's Intel/AMD
Linux decode gap").

## Zero-copy

This is the only decode-to-render GPU path on either stack, and it is the decode
half of the DMA-BUF zero-copy pipeline whose encode half is
`codec-vaapi-encode.md`. See ../zerocopy.md, section "2b. Decode to render:
verdict ours" (moq has no VAAPI decode, no GPU-resident decode output on non-NVIDIA
Linux, and no import machinery) and section "5. Concrete upstream requirements",
requirement U2 (per-decoder surface export).

The two hard-won details this plan must preserve verbatim:

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
  instead of re-running surface re-import, sync, and export each time. This is the
  on-demand-export shape moq's store-the-fd enum lacks today (../zerocopy.md
  section "1.3 Analysis").

Also preserved:

- The cros-codecs stateless H.264 decode loop and its DMA-BUF-backed frame pool
  (`GenericDmaVideoFrame` surfaces created with `USAGE_HINT_DECODER |
  USAGE_HINT_EXPORT`, `decoder.rs:319-339`).
- The Baseline `constraint_set0_flag` SPS patch (`patch_baseline_constraint_flag`,
  `decoder.rs:37-76`): cros-codecs requires `constraint_set0_flag = 1` for
  `profile_idc = 66` to map to `VAProfileH264ConstrainedBaseline`, and openh264
  and some encoders emit Baseline without it, producing "Could not get
  VAProfile!".
- The `vaDeriveImage`-based NV12 plane mapping used for the CPU fallback download
  (`derive_nv12_planes`, `decoder.rs:127` onward), which handles Intel 4-tile
  formats the generic mapper cannot.

The DMA-BUF surfaces this backend exports are the input the renderer imports over
Vulkan, EGL, or Metal; that consumer is `render-video-render.md`, and the
`VppRetiler` Y_TILED-to-CCS re-tile that makes Intel DMA-BUFs Vulkan-importable
lives there, not in this backend.

## What to do

MOVE. A two-target contribution. The VA-layer decode work lands in the external
`moq-vaapi` crate (`github.com/moq-dev/vaapi`); the moq-video decode-backend
wiring (the new decode candidate) is a separate monorepo PR. Only the
backend-wiring half is in the moq monorepo.

Dependencies: B1 (frame vocabulary), B3 (`decode::Frame::native()`); shares
moq-vaapi with `codec-vaapi-encode.md`.

moq API consumed:

- The `crate::frame::Frame::DmaBuf(dmabuf::Frame)` variant added by B1
  (`moq:frame.rs:23-36`), gated `cfg(all(target_os = "linux", feature =
  "dmabuf"))` (the `vaapi` feature enables `dmabuf`, so this backend sees the
  variant), with its `to_i420` CPU-download arm so the container's CPU consumers
  keep working.
- The public `Native::DmaBuf(DmaBuf)` vocabulary from B1 (the on-demand `export()`
  shape), which `decode::Frame::native()` (B3) returns for a `Frame::DmaBuf`.
- The decode `Backend` trait and `Candidate` table as they stand today
  (`moq:decode/backend/mod.rs:81-114`): a `Candidate` carries `supports: fn(Codec)
  -> bool` and `open: fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>`, and
  `Decoded { timestamp: Timestamp, frame: Frame }` already carries the per-picture
  presentation timestamp, so no encode-style PTS change is needed on the decode
  side.

Source to port. `rusty-codecs/src/codec/vaapi/decoder.rs` (1,188 LOC) is the
reference implementation for the re-vendor. Its logic and the two hard-won details
under Zero-copy are the specification the re-vendored moq-vaapi decode module must
reproduce against moq-vaapi's own bindings. Dropped: our
`GpuFrameInner`/`NativeFrameHandle` trait glue and the `DmaBufInfo` struct,
replaced by moq's `dmabuf::Frame` (B1) -- note the export path itself is not new
work in moq-vaapi, since `surface.rs:341` `export_prime()` already wraps
`vaExportSurfaceHandle`, so the re-vendored decode module calls the crate's
existing export rather than adding one; what ports is the surrounding usage
(sync-before-export, per-frame caching). Also dropped: the RGBA convenience
conversion (`nv12_to_rgba_data` import, `decoder.rs:33`), which is presentation
logic belonging in the renderer; our `config.rs` catalog mirror, replaced by hang
catalog types via moq's `Codec`/`Config`; and `anyhow` return types, replaced by
moq's `Error`.

Target in moq:

- New file `rs/moq-video/src/decode/backend/vaapi.rs`. A `pub(crate) struct Vaapi`
  implementing the decode `Backend` trait, with `pub(crate) const NAME: &str =
  "vaapi";` and an `open(codec: Codec, config: &Config) -> Result<Box<dyn
  Backend>, Error>` constructor, mirroring `rs/moq-video/src/decode/backend/
  openh264.rs`. Its `decode(...)` returns `Vec<Decoded>` whose `frame` is
  `Frame::DmaBuf(...)`.
- One additive `const Candidate` in `rs/moq-video/src/decode/backend/mod.rs`, in
  the `HARDWARE` slice (`mod.rs:89-108`), gated `cfg(all(target_os = "linux",
  feature = "vaapi"))`, with `supports: |c| matches!(c, Codec::H264)` and `open:
  vaapi::Vaapi::open`. Add only this row; do not refactor the table.
- `moq-vaapi` crate growth (this leaf owns it, targeting the external
  `moq-dev/vaapi` repo). Surface export already exists (`surface.rs:341`,
  `lib.rs:33-35`), as does the VPP `VAProcPipelineParameterBuffer` wrapper
  (`buffer/proc_pipeline.rs`), so the additive work is the decode stack: a
  re-vendored stateless H.264 decode path expressed in moq-vaapi's diverged
  bindings, plus the caller-side reader that turns an exported
  `VADRMPRIMESurfaceDescriptor` into the fd, modifier, fourcc, coded and display
  size, and per-plane offset and pitch, plus any VPP scale/CSC execution path
  (context, caps, submit) not yet present. Because `moq-vaapi` is a separate
  external crate, this is a PR to `moq-dev/vaapi` and an upstream-cut release, not
  an in-tree moq-video edit; the moq-video PR then bumps the pin.
- `rs/moq-video/src/frame.rs` gains the `dmabuf` module backing `Frame::DmaBuf`
  (owned by B1; this leaf consumes it and, if B1 leaves the backing type a stub,
  fills in the exporter that mints the fd on demand).

Open question: the dependency spine. Our decoder is written against the crates.io
`cros-codecs 0.0.6` crate, which moq-vaapi does not depend on; vaapi-decode owns
the decode-stack contribution to moq-vaapi, and that contribution is a re-vendor
against moq-vaapi's diverged bindgen types, not a drop-in of our cros-codecs
decoder. Before decode can be contributed the spine must be chosen: (i) re-vendor
the cros-codecs decode half into moq-vaapi's binding style (matches the crate's
current provenance, but grows the AI-generated divergence to maintain), (ii) add
a real `cros-codecs` dependency to moq-vaapi and retire the vendored fork for the
decode path (cleaner, but large and needs upstream buy-in), or (iii) another
route such as moq-video depending on `cros-codecs` directly. Current proposal:
route (i), re-vendor the decode half into moq-vaapi's style, matching how the
crate already treats cros-libva. This leaf does not pick one unilaterally; the
question is settled upstream before code.

Implementation steps:

1. Confirm the B1 `Frame::DmaBuf(dmabuf::Frame)` variant and the public
   `Native::DmaBuf`/`DmaBuf::export()` shape are in the base branch. If the backing
   `dmabuf::Frame` cannot express the on-demand export (holding the VA surface and
   dup'ing an fd per `export()` call, our `decoder.rs:85-113,228` design), stop and
   file the gap against B1; do not invent a divergent type.
2. Grow `moq-vaapi`: settle the dependency-spine question above upstream, then
   re-vendor the decode half of cros-codecs (a stateless H.264 decode path) into
   moq-vaapi's diverged bindings. Reuse the crate's existing `export_prime()`
   (`surface.rs:341`) and DRM PRIME types rather than adding export; add only the
   caller-side `VADRMPRIMESurfaceDescriptor` reader and any missing VPP execution
   path. Agree the crate's public surface upstream, land the PR in `moq-dev/vaapi`,
   wait for the new release, and bump the pin at `rs/moq-video/Cargo.toml` and the
   workspace manifest. This is the largest and most resistance-prone piece; treat
   it as the critical path.
3. Write `decode/backend/vaapi.rs`: construct the shared `Display`, the
   `GenericDmaVideoFrame` pool sized from `StreamInfo::coded_resolution`, and the
   `StatelessDecoder<H264, ...>`. Port `patch_baseline_constraint_flag`
   (`decoder.rs:37-76`) as a private helper run over each incoming Annex-B access
   unit before it reaches the decoder.
4. Implement `decode(access_unit, timestamp, keyframe) -> Result<Vec<Decoded>,
   Error>`: feed the patched Annex-B unit, drain ready pictures, and for each wrap
   the pooled surface as a `Frame::DmaBuf` whose exporter caches its PRIME
   descriptor. Carry the input timestamp onto each `Decoded` (H.264 stateless is
   one-in one-out here; echo it), matching the openh264 backend.
5. Port the export path into the `dmabuf::Frame` exporter: `to_native_handle`
   re-import (`decoder.rs:253-268` with its `catch_unwind` guard against
   cros-codecs' panic on EMFILE), then `surface.sync()` before
   `surface.export_prime()` (`decoder.rs:269-283`), then fill moq's DMA-BUF handle
   from the first layer and object (`decoder.rs:290-310`). Preserve the `OnceCell`
   memoization so a second `export()` dup's the cached fd.
6. Thread `reset()` and `burst_size()` (`decoder.rs:731,616`). moq's decode
   `Backend` trait (`moq:decode/backend/mod.rs:67-75`) has neither method today;
   both are needed by a stateful hardware decoder. Either add them as defaulted
   trait methods (additive) or fold the reset-on-loss and DPB-flush behavior into
   `decode(...)`. If a trait change is needed, file it against B3/the base plan
   rather than editing the trait unilaterally; the dispatch model that carries
   these needs across all hardware decoders is `codec-dispatch.md`.
7. Add the `const Candidate` row to `decode/backend/mod.rs`.
8. Gate everything on `cfg(all(target_os = "linux", feature = "vaapi"))` and
   confirm a non-Linux and a `vaapi`-off build still compiles, matching moq's
   compile-everywhere posture.

Adaptation notes:

- Timestamps are `moq_net::Timestamp` on `Decoded`; keep any internal `Duration`
  behind the boundary.
- Errors are moq's `#[non_exhaustive]` `Error`; add a `SurfaceExport` (and, if not
  already present from the encoder, a `DmaBufImport`) variant additively rather
  than returning `anyhow`.
- No ffmpeg. dlopen or degrade cleanly: `moq-vaapi 0.0.2` hard-links libva today
  (#1837), so the decode growth should not regress that and ideally moves toward
  dlopen so a libva-less host falls back instead of failing to load.
- The backend is `Box<dyn Backend>` where `Backend: Send`; the cros-codecs objects
  are `!Send` (libva `Rc`), so use a documented `unsafe impl Send` justified by
  thread-confinement, matching the encode VAAPI backend's pattern
  (`moq:encode/backend/vaapi.rs:37-40`).

iroh-live removal side. The VAAPI encode and decode modules
(`rusty-codecs/src/codec/vaapi/`, 3,257 LOC for the pair) are upstream-ours: they
are deleted only after both `codec/vaapi-decode.md` and `codec/vaapi-encode.md`
merge and release, because deleting the decoder earlier would drop hardware decode
on Intel and AMD Linux and regress the decode-to-render zero-copy input. The cut
lands on the paired iroh-live `up/vaapi-decode` and `up/vaapi-encode` branches;
Linux flips atomically once the VAAPI and V4L2 series have all released, and the
DMA-BUF-to-render path must already run through `render-adopt` before the deletion
(`../comparison/codecs.md` removal ledger, VAAPI row; the deletion commit and the
moq version bump travel together in one revertible commit). The `reset()` and
`burst_size()` decoder needs must be carried into moq's decode trait before the
local dispatch is cut, tracked in `codec-dispatch.md`.

## Tests

- A hardware round-trip test in the style of moq's NVDEC hardware test
  (`moq:decode/backend/nvdec.rs:666`): encode a known pattern to H.264, decode it
  through the VAAPI backend, assert the output frame is `Frame::DmaBuf` (that
  `decode::Frame::native()` returns `Some(Native::DmaBuf(_))`), and assert the CPU
  `into_i420()` fallback matches the source within tolerance. Mark `#[ignore]`
  with a reason if the CI runner has no Intel or AMD VA-API device; validate
  locally on Intel Meteor Lake hardware.
- A Baseline-SPS unit test over `patch_baseline_constraint_flag` asserting
  `profile_idc = 66` gains `constraint_set0_flag` and other NAL types are
  untouched. This is pure bit manipulation, no hardware, so CI runs it everywhere.
- An export-caching assertion: two `export()` calls on one frame yield two valid
  but distinct fds without a second surface re-import (observable via the
  `OnceCell` being populated once).

CI can compile-gate the backend on every platform and run the pure-bit SPS-patch
test, but it cannot exercise the hardware decode or the DMA-BUF export without an
Intel/AMD VA-API device, so the round-trip and export-caching tests are
`#[ignore]`d with a stated reason on runners that lack one and validated on our
Meteor Lake hardware.

## Evidence

- moq has no VAAPI decode backend of any kind: `../comparison/codecs.md`, the
  decode-backend grid in section 1 (H.264 decode, VAAPI = none) and the section-10
  verdict-table row; `../comparison/maps/moq-video.md:736-748` (decode coverage
  matrix, Linux H.264 = NVDEC or openh264 only); ../zerocopy.md section "2b.
  Decode to render: verdict ours".
- The upstream requirement U2 and the moq-side change 4 that drive this:
  ../zerocopy.md section "5. Concrete upstream requirements", U2;
  `../comparison/moq-changes.md` section 1b (decoders export a handle, change 2)
  and section 5 change 4 (sizing L-XL, moq-vaapi ~1000-1400 plus backend ~900).
- moq-vaapi is a separate external crates.io crate shipping an encoder plus
  surface export plus a VPP wrapper but no decode stack:
  `../comparison/maps/moq-video.md:23-27`, verified at `rs/moq-video/Cargo.toml`
  (`vaapi = ["dep:moq-vaapi"]`, workspace pin `moq-vaapi = "0.0.2"`) and the crate
  source (`moq-vaapi-0.0.2/src/surface.rs:341` export, `src/lib.rs:33-35` DRM
  PRIME types, `src/buffer/proc_pipeline.rs` VPP wrapper, `src/encode.rs` encoder,
  no `decode` module).
- Our reference decoder and the two hard-won details: `../comparison/codecs.md`,
  "VAAPI vs VAAPI" decoder paragraph (`vaapi/decoder.rs:81-119`, `reset()` at
  `:731`, `burst_size()` at `:616`, the Baseline SPS patch at `:37-76`);
  `../comparison/maps/rusty-codecs.md` section 2 (the vaapi row, `VaapiDecoder`,
  DMA-BUF `Gpu` frames).
- The removal ledger: `../comparison/codecs.md` removal side (VAAPI encode+decode
  row, 3,257 LOC, upstream-ours, waits on both leaves) and the DISPOSITION VAAPI
  row.

## Coordination

Base plans needed: B1 (frame vocabulary), B3 (`decode::Frame::native()`), and the
decode-trait `reset()`/`burst_size()` thread that belongs in the base
backend-trait plan (B4) or inside `decode(...)`. The release gate is the moq
release carrying the grown moq-vaapi decode stack plus the moq-video candidate
row; the iroh-live cut waits on it.

- Base API freeze: if `Frame::DmaBuf`'s backing type or the `Native::DmaBuf`
  export shape cannot express on-demand export, or if the decode `Backend` trait
  needs `reset()`/`burst_size()`, stop and file against B1/B3/B4.
- Shared candidate table: add only the single VAAPI row to
  `decode/backend/mod.rs`; do not refactor the table. Later leaves rebase.
- Shared moq-vaapi: this leaf owns the moq-vaapi decode-stack growth. Surface
  export (`surface.rs:341`) and the VPP parameter-buffer wrapper
  (`buffer/proc_pipeline.rs`) already exist and are reused, not authored.
  `codec-vaapi-encode.md` reuses the same existing export/VPP and must not
  duplicate the decode work; coordinate the shared moq-vaapi API shape with the
  encode agent.
- External moq-vaapi repo: the VA-layer decode work is a PR to
  `github.com/moq-dev/vaapi`, a separate repository under the moq-dev org with its
  own review path and release cadence. We do not hold publish rights to the crate
  name; the critical path runs through the upstream release, followed by the
  moq-video pin bump. The open question above (re-vendor vs. `cros-codecs`
  dependency vs. another route; current proposal re-vendor into moq-vaapi's style)
  is settled upstream before code.
- Two PR targets: the `moq-dev/vaapi` repo for the VA layer, the moq monorepo for
  the moq-video wiring and pin bump.
- Licensing and provenance: moq-vaapi already carries `LICENSE.libva` and
  `LICENSE.cros-codecs` alongside its BSD-3-Clause `LICENSE`, self-described as
  derived from discord/cros-libva and discord/cros-codecs. Our re-vendored decode
  half originates from the same cros-codecs BSD-3-Clause source, so the
  contribution matches the crate's existing provenance and license; carry the same
  attribution.

Per-segment transcode and FETCH rate control. The decode side feeds
moq-transcode's decode-once plus GPU-resize fanout. Our VAAPI decode exporting
`Frame::DmaBuf` plus VPP GPU scale is the Intel and AMD analog of the NVDEC
free-scaling that fanout uses on NVIDIA, so VAAPI decode into VPP scale into VAAPI
encode is a full Intel and AMD per-segment transcode pipeline; the encode end of
that loop is `codec-vaapi-encode.md`. `reset()` and `burst_size()` support the
per-group FETCH and seek pattern, and where moq-transcode requests scaled decode,
that maps to our VPP scale. This is alignment with moq's per-segment-transcoding
goal, not a collision.

CI hardware gating. The backend compiles everywhere; the hardware round-trip and
export-caching tests are `#[ignore]`d with a reason where no Intel/AMD VA-API
device is present, with local validation on Meteor Lake hardware we run. The
Baseline-SPS unit test runs in CI on every platform because it is pure bit
manipulation.

The decode-to-render consumer of the DMA-BUF surfaces this backend exports,
including the Vulkan/EGL/Metal import machinery and the Intel `VppRetiler`
re-tile, is `render-video-render.md`. The dispatch selection layer that carries
the `reset()`/`burst_size()` needs into moq's decode trait is `codec-dispatch.md`.
