# VAAPI H.264 Encode

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: central. This is the Linux capture-to-encode half of the DMA-BUF
> zero-copy story, the Intel and AMD analog of moq's NVDEC-to-NVENC transcode
> loop; it imports DMA-BUF surfaces directly as VA surfaces and stays on the GPU
> through VPP scale and color conversion. See ../zerocopy.md, section "2a. Capture
> to encode: verdict complementary".

## What it is

A hardware H.264 encoder over VAAPI that takes GPU-resident DMA-BUF surfaces as
input, imports them directly as VA surfaces with no CPU copy, runs VPP hardware
scale and color conversion when the input dimensions or format do not match the
encode target, retunes bitrate honestly where the driver supports it, and forces
an IDR per frame on request. moq already ships a VAAPI encode backend, but it is
CPU-only, cannot retune, and its own header says it is not yet validated on
hardware, so this is a replacement and hardware validation of an existing but
unvalidated path rather than an additive new backend. The verdict is move because
our implementation is a full cros-codecs `StatelessEncoder` integration validated
on Intel Meteor Lake, decisively more capable than moq's 111-line placeholder.

## iroh-live vs moq

moq ships a 111-line adapter (`moq:encode/backend/vaapi.rs`) over the external
`moq-vaapi` 0.0.2 crate that takes CPU frames only: every frame is interleaved to
NV12 on the CPU (`i420_to_nv12`, `moq:encode/backend/vaapi.rs:97-111`), there is
no GPU-surface input path, it cannot retune (`set_bitrate` returns
`Error::BitrateUnsupported(NAME)` because moq-vaapi 0.0.2 has no setter for its
private bitrate field, `:80-88`), it hard-links libva so a libva-less host fails
to load the binary before fallback (#1837), and the header states outright "NOT
YET VALIDATED ON HARDWARE" (`:19-21`). It does support per-frame forced IDR
through `encode_nv12(&nv12, keyframe)`. Notably, `moq-vaapi` already ships an
encoder plus surface export plus a VPP wrapper: verified against the cargo-cache
source, `src/surface.rs:341` exposes `export_prime()`, `src/lib.rs:33-35`
re-exports the `VADRMPRIMESurfaceDescriptor` bindgen types, and
`src/buffer/proc_pipeline.rs` carries a `VAProcPipelineParameterBuffer` VPP
wrapper. What is missing is a validated GPU-surface encode path.

Ours is a hardware-validated cros-codecs `StatelessEncoder` integration
(`rusty-codecs/src/codec/vaapi/encoder.rs`, 1,533 LOC, plus `vpp_scaler.rs`, 529
LOC): zero-copy DMA-BUF input imported as VA surfaces via `DRM_PRIME_2`
descriptors, VPP hardware scale on dimension mismatch, VPP GPU color conversion
for non-NV12 DMA-BUFs, CBR rate control with QP clamped to 18-36 to prevent
keyframe compression bursts, and a `LowDelay` prediction structure with the
H.264 `max_frame_num` floor handled. Two gaps on our side that the port closes:
no `set_bitrate` (it inherits the silent no-op trait default; moq's honest-fail
contract is better), and no per-frame forced IDR through the trait
(`force_keyframe: false` is hardcoded in `push_frame`, `encoder.rs:1352`);
cros-codecs `FrameMetadata` supports both one plumbing step away. This is the
most lopsided comparison in the codec survey, in our favor; verdict "upstream
ours" (`../comparison/codecs.md`, section "VAAPI vs VAAPI (+ moq-vaapi)" and
verdict-table row).

## Zero-copy

This backend is the Linux capture-to-encode half of the zero-copy pipeline, and
it pairs with the decode-to-render half in `codec-vaapi-decode.md`. See
../zerocopy.md, section "2a. Capture to encode: verdict complementary" (on Linux
ours is strictly ahead; DMA-BUF capture into VAAPI encode with GPU-side scale and
color conversion has no moq counterpart) and section "5. Concrete upstream
requirements", requirement U3 (capture DMA-BUF delivery with the matching encoder
input).

The zero-copy mechanics to preserve:

- DMA-BUF surface input: `build_prime_descriptor` builds a
  `VADRMPRIMESurfaceDescriptor` from the DMA-BUF handle (`encoder.rs:91`, the
  descriptor builder at `encoder.rs:87-119`), and `push_frame` selects the
  zero-copy path when the frame carries a DMA-BUF handle (`encoder.rs:1268-1289`;
  the `native_handle()` check and import at `encoder.rs:1273-1280`). Capture at
  1080p BGRx and encode at 720p NV12 stays a pure GPU pipeline.
- VPP on mismatch, staying on the GPU: `vpp_scale_or_cpu` for wrong dimensions
  (`encoder.rs:1069`, backed by the cached-context `VppScaler` in
  `vaapi/vpp_scaler.rs:24-37`) and `vpp_convert_or_cpu` for non-NV12 formats
  (`encoder.rs:1009`), each with a permanent CPU fallback after a failure so a
  broken VPP never loops.
- Input-timestamp propagation through cros metadata (`encoder.rs:1302-1353`).

The CPU path remains only as the non-DMA-BUF fallback: a borrowing `to_i420(&self)`
download for other frame variants. moq's `decode::Frame::into_i420(self)` consumes
and its `to_i420` is `pub(crate)` today (`moq:decode/mod.rs:92`), so a public
borrowing `to_i420(&self)` must exist; coordinate that with B1/B3 rather than
cloning the frame. This borrow gap chiefly bites the decode-to-render path, but
the encoder's CPU fallback needs the same borrowing accessor.

## What to do

MOVE. Two edit sites, one in the external `moq-vaapi` crate and one in
moq-video. The encoder logic is re-vendored into moq-vaapi's `src/encode.rs`
`Encoder` (which today exposes `Config`/`Encoder` with `encode_nv12` expressed in
moq-vaapi's own diverged bindings, not the crates.io `cros-codecs` crate). The
moq-video backend keeps consuming `moq_vaapi::encode`; it does not switch to a
direct `cros-codecs` dependency, and moq-vaapi does not gain the DMA-BUF-import
plus VPP plus tunings surface twice. Our `rusty-codecs` `H264StatelessEncoder`
integration is the reference specification for the re-vendored logic, not a
drop-in.

Dependencies: B1 (`Frame::DmaBuf` frame vocabulary), B2 (PTS through encode);
shares moq-vaapi with `codec-vaapi-decode.md`.

moq API consumed:

- The B2 encode `Backend` signature `fn encode(&mut self, frame: &Frame,
  timestamp: Timestamp, keyframe: bool) -> Result<Vec<Packet>, Error>` with
  `Packet { payload: Bytes, timestamp: Timestamp }` (`../comparison/moq-changes.md`
  section 2). VAAPI is one-in one-out and echoes the frame's timestamp, but it
  consumes the trait's new signature so the backend table stays uniform.
- The B1 `Frame::DmaBuf(dmabuf::Frame)` input variant (`moq:frame.rs:23-36`) and
  its `to_i420()` CPU fallback arm (`frame.rs:63-74`).
- The existing encode `Config` (`moq:encode/encoder.rs:55-70`: `width`, `height`,
  `framerate`, `bitrate`, `gop`, `codec`, `kind`), read from hang catalog types,
  not our `config.rs` mirror.
- moq's `Codec::H264` (the public encode enum is `{H264, H265}`,
  `moq:encode/encoder.rs:21-48`); this backend registers H.264 only.

Source to port. `rusty-codecs/src/codec/vaapi/encoder.rs` (1,533 LOC) is the
reference specification, validated on Intel Meteor Lake. Carries over: the
DMA-BUF surface input and VPP-on-mismatch paths detailed under Zero-copy above;
the CBR rate-control quality clamping (QP 18-36) and the `LowDelay` prediction
structure with the `max_frame_num` floor (`../comparison/codecs.md`, "VAAPI vs
VAAPI" bullet list); avcC-free avc3 in-band output (moq emits Annex-B in-band
only, so drop our avcC priming-encoder path); per-frame forced IDR wired through
cros-codecs `FrameMetadata::force_keyframe` (replacing the hardcoded `false` at
`encoder.rs:1352` -- note the separate internal priming encoder with
`force_keyframe: true` at `encoder.rs:928` stays internal); and an honest
`set_bitrate` via cros-codecs `Tunings.rate_control`
(`../comparison/codecs.md`, "VAAPI vs VAAPI"). Dropped: our
`VideoEncoderFactory`/`VideoEncoder` push/pop trait glue
(`encoder.rs:1230,1251`), replaced by moq's synchronous `encode(...) ->
Vec<Packet>`; our `config.rs` catalog mirror and `DmaBufInfo` type, replaced by
hang catalog types and moq's `dmabuf::Frame`; and `anyhow` returns, replaced by
moq's `Error`.

Target in moq:

- In the external `moq-vaapi` crate (`github.com/moq-dev/vaapi`), extend
  `src/encode.rs` so `Encoder` gains DMA-BUF PRIME import, VPP scale/convert,
  honest bitrate retune, and forced-IDR behavior, expressed in moq-vaapi's own
  bindings and reusing the crate's existing `surface.rs:341` `export_prime()` and
  `buffer/proc_pipeline.rs` VPP wrapper (do not duplicate them).
- In moq-video, `rs/moq-video/src/encode/backend/vaapi.rs` keeps consuming
  `moq_vaapi::encode` and is edited only to feed it DMA-BUF frames and thread the
  new `set_bitrate`/`keyframe` calls through. Keep `pub(crate) const NAME: &str =
  "vaapi";`, the `open(config: &Config)` shape, and the existing `unsafe impl Send
  for Vaapi` (`encode/backend/vaapi.rs:37-40`).
- No new `const Candidate` is needed: the encode candidate table already lists a
  VAAPI H.264 entry (`../comparison/maps/moq-video.md:145`), so this changes the
  backend behind the existing row and does not touch the table. This is unlike
  `codec-vaapi-decode.md`, which adds a new decode row.
- The `i420_to_nv12` CPU helper (`encode/backend/vaapi.rs:97-111`) gives way to
  the DMA-BUF import plus VPP path in moq-vaapi, with a borrowing CPU upload
  retained only as the non-DMA-BUF fallback.

Implementation steps:

1. Confirm B1 (`Frame::DmaBuf`) and B2 (the `timestamp` argument plus `Packet`)
   are in the base branch, and settle the shared moq-vaapi API with
   `codec-vaapi-decode.md`: both leaves reuse the crate's existing
   `export_prime()` and `proc_pipeline` VPP wrapper, so the coordination is on
   the encode-module additions and any shared VPP execution path, not on re-adding
   export.
2. Extend `moq-vaapi`'s `src/encode.rs` `Encoder`: re-vendor the
   `StatelessEncoder` setup into moq-vaapi's bindings, creating or reusing the
   shared `Display` and configuring H.264 CBR with the QP 18-36 clamp, the
   `LowDelay` prediction structure, and the `max_frame_num` floor. The moq-video
   backend keeps its `moq_vaapi::encode::{Config, Encoder}` field.
3. Implement `encode(frame, timestamp, keyframe) -> Result<Vec<Packet>, Error>`:
   match the frame variant. The trait takes `frame: &Frame`, and the DMA-BUF path
   imports the surface by borrow, so this step does not consume the frame. For
   `Frame::DmaBuf`, import via `build_prime_descriptor` and the `push_frame`
   dispatch (`encoder.rs:91,1268-1289`). For any other variant, download through a
   borrowing `to_i420(&self)` accessor (the borrow gap noted under Zero-copy).
4. Route mismatches through VPP on the GPU: wrong dimensions to `vpp_scale_or_cpu`
   (`encoder.rs:1069`), non-NV12 to `vpp_convert_or_cpu` (`encoder.rs:1009`), each
   with the permanent-CPU-fallback-after-failure guard preserved.
5. Wire `keyframe` through cros-codecs `FrameMetadata::force_keyframe` (replacing
   the hardcoded `false` at `encoder.rs:1352`), and stamp each returned `Packet`
   with the incoming `timestamp`.
6. Implement `set_bitrate` against cros-codecs `Tunings.rate_control`: retune
   where the driver accepts it, return `Error::BitrateUnsupported(NAME)` only
   where it truly cannot, and do not force an IDR on retune (moq's rate-control
   contract, `moq:encode/backend/mod.rs` `set_bitrate` doc).
7. Delete `i420_to_nv12` and keep the whole thing gated `cfg(all(target_os =
   "linux", feature = "vaapi"))`; confirm non-Linux and `vaapi`-off builds
   compile.

Adaptation notes:

- Timestamps are `moq_net::Timestamp` on `Packet`; echo the input timestamp since
  VAAPI is zero-frame-delay.
- Errors are moq's `Error`; reuse the `DmaBufImport` variant (shared with
  `codec-vaapi-decode.md`) and add nothing that leaf already added.
- No ffmpeg. Prefer dlopen for libva so a libva-less host falls back (#1837); at
  minimum do not regress the current behavior.
- The `Packet`-per-frame return expresses the streaming backend without changing
  moq's one-shot public `Encoder` front end (`../comparison/moq-changes.md`
  section 3, item 4): VAAPI returns exactly one packet per input.

iroh-live removal side. The VAAPI encode and decode modules
(`rusty-codecs/src/codec/vaapi/`, 3,257 LOC for the pair) are upstream-ours: they
are deleted only after both `codec/vaapi-decode.md` and `codec/vaapi-encode.md`
merge and release, because deleting them earlier would drop hardware decode on
Intel and AMD Linux and regress the decode-to-render zero-copy input. The cut
lands on the paired iroh-live `up/vaapi-decode` and `up/vaapi-encode` branches;
Linux flips atomically once the VAAPI and V4L2 series have all released, and the
DMA-BUF-to-render path must already run through `render-adopt` before the
deletion (`../comparison/codecs.md` removal ledger, VAAPI row; the deletion
commit and the moq version bump travel together in one revertible commit).

## Tests

- A hardware round-trip test in moq's VideoToolbox/NVENC style: feed a DMA-BUF
  NV12 surface (or a synthetic I420 frame through the CPU path), encode, decode
  the emitted Annex-B, and assert the result matches within tolerance and that
  keyframes appear when requested. Mark `#[ignore]` with a reason where no
  Intel/AMD VA-API device is present; validate locally on Meteor Lake. This
  replaces the "NOT YET VALIDATED ON HARDWARE" caveat.
- A forced-IDR test: `encode(frame, ts, keyframe: true)` yields an access unit
  the decoder treats as random-access.
- A `set_bitrate` test: assert it either applies (readback where feasible) or
  returns `Error::BitrateUnsupported`, never silently succeeds, matching moq's
  honest-contract rule.
- A VPP-fallback assertion: a mismatched-dimension input still encodes (GPU VPP or
  the permanent CPU fallback), and a forced VPP failure does not loop.

CI can compile-gate the backend on every platform (non-Linux and `vaapi`-off
builds must compile), but it cannot exercise the hardware path without an
Intel/AMD VA-API device, so the round-trip test is `#[ignore]`d with a stated
reason on runners that lack one and validated on our Meteor Lake hardware. This
matches moq's compile-everywhere posture for NVENC.

## Evidence

- The lopsided comparison and the "upstream ours" verdict:
  `../comparison/codecs.md`, section "VAAPI vs VAAPI (+ moq-vaapi)" (the whole
  VAAPI-vs-VAAPI section) and the section-10 verdict-table row (H.264 encode /
  VAAPI).
- moq's current placeholder: `../comparison/maps/moq-video.md:199-206`, verified
  at `rs/moq-video/src/encode/backend/vaapi.rs` (111 lines; CPU `i420_to_nv12` at
  `:97-111`; `set_bitrate` returns `Error::BitrateUnsupported(NAME)` at `:80-88`;
  "NOT YET VALIDATED ON HARDWARE" header at `:19-21`). The encode candidate table
  already lists a VAAPI H.264 entry at `../comparison/maps/moq-video.md:145`.
- The moq-side change 5 and requirement U3 driving this: `../comparison/moq-changes.md`
  section 1 (the encode dispatch sketch) and section 5 change 5 (sizing L, ~800);
  ../zerocopy.md section "5. Concrete upstream requirements", U3.
- Our reference encoder anchors: `../comparison/codecs.md`, "VAAPI vs VAAPI"
  bullet list, and `../comparison/maps/rusty-codecs.md` section 2 (the vaapi row,
  `VaapiEncoder`, `VppScaler`, DMA-BUF `Gpu` frames).
- The removal ledger: `../comparison/codecs.md` removal side (VAAPI encode+decode
  row, 3,257 LOC, upstream-ours, waits on both leaves) and the DISPOSITION VAAPI
  row.

## Coordination

Base plans needed: B1 (frame vocabulary), B2 (PTS through encode). The release
gate is the moq release carrying the merged moq-vaapi encode logic plus the
moq-video backend edit; the iroh-live cut waits on it.

- Shared candidate table: no table edit needed; this changes the backend behind
  the existing VAAPI row. Do not refactor the table.
- Shared moq-vaapi: surface export (`surface.rs:341`) and the VPP parameter-buffer
  wrapper (`buffer/proc_pipeline.rs`) already exist in moq-vaapi and are reused by
  both leaves, not authored here. This leaf owns the encode-module logic;
  `codec-vaapi-decode.md` owns the decode stack. The two agents agree the shared
  moq-vaapi API (encode extensions, any shared VPP execution path) before either
  lands.
- External moq-vaapi repo: the VA-layer encode work is a PR to
  `github.com/moq-dev/vaapi`, a separate repository under the moq-dev org with its
  own review path and release cadence; it already ships an encoder plus surface
  export plus VPP but unvalidated on hardware, so our work is a
  replacement-and-validation of that encode path. The moq-video backend change is
  a separate monorepo PR that bumps the pin. We do not hold publish rights to the
  crate name; the critical path runs through the upstream release.
- Two PR targets: the `moq-dev/vaapi` repo for the VA layer, the moq monorepo for
  the moq-video wiring and pin bump.
- Licensing and provenance: moq-vaapi already carries `LICENSE.libva` and
  `LICENSE.cros-codecs` alongside its BSD-3-Clause `LICENSE`, self-described as
  derived from discord/cros-libva and discord/cros-codecs. Our re-vendored encode
  logic originates from the same cros-codecs BSD-3-Clause source, so the
  contribution matches the crate's existing provenance and license; carry the same
  attribution.

Per-segment transcode and FETCH rate control. moq-transcode drives this backend
through the public `encode::{Kind, Config, Encoder}` front end: it selects by
`Kind`, sets a per-rung CBR target through `Config.bitrate` at construction, and
forces an IDR per group. This backend must therefore honor `Config.bitrate` as a
per-encoder CBR target, expose a QP or target-quality knob for custom per-GOP
rate control, and keep `set_bitrate` honest and free of any forced-IDR side
effect. Add an encoder-session-reuse path so a per-group transcode loop does not
pay a fresh VA-context open per group. Scope this precisely: neither cros-codecs'
`H264StatelessEncoder` nor moq-vaapi's `encode.rs` `Encoder` has an in-place
resolution or GOP reconfigure, and our encoder constructs via `new` with no
`reset` (`rusty-codecs/src/codec/vaapi/encoder.rs`, `grep 'fn reset'` empty;
contrast the decoder's `reset()` at `decoder.rs:731`). So "reuse" means holding
the VA `Display` and the encoder session across groups and resetting the
rate-control state and forcing a fresh IDR, rather than reopening a VA context per
group. If a same-resolution same-GOP session cannot re-arm rate control without a
rebuild, the fallback is a controlled teardown and rebuild of the encoder session
over a retained `Display`. This is new work on moq-vaapi (or cros-codecs),
coordinated with moq's per-segment-transcode goal, not a one-liner. Defer the
rate-control policy to moq-transcode; do not embed a streaming controller. The
decode side of this pipeline, VAAPI decode into VPP scale into VAAPI encode, is
the full Intel and AMD per-segment transcode loop described in
`codec-vaapi-decode.md`.

CI hardware gating. The backend compiles everywhere and the hardware round-trip
test is `#[ignore]`d with a reason where no Intel/AMD VA-API device is present,
matching moq's compile-everywhere stub posture; local validation is on Meteor
Lake hardware we run.

The dispatch selection layer that carries the `reset()`/`burst_size()` decoder
needs and the `Kind::Named`/`Hardware` model is covered in `codec-dispatch.md`;
the decode-to-render consumer of the DMA-BUF surfaces this pipeline produces on
the decode side is `render-video-render.md`.
