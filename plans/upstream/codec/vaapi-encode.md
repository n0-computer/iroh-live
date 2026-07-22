# vaapi-encode. VAAPI H.264 encode with DMA-BUF input, VPP, and honest set_bitrate

> Campaign: upstream | Kind: leaf plan | Branch: up/vaapi-encode | PR targets:
> external `moq-dev/vaapi` repo (VA-layer encode) + moq main (moq-video backend) |
> Read ../0-overview.md first.

Depends on: B1 (frame vocabulary), B2 (PTS through encode); shares moq-vaapi with vaapi-decode
Path: two-target contribution. The encoder logic (DMA-BUF import, VPP, honest
`set_bitrate`, forced IDR) is contributed into the external `moq-vaapi` crate's
existing-but-unvalidated encode module (`github.com/moq-dev/vaapi`, overview
coordination point 11); the moq-video backend keeps consuming `moq-vaapi` as it
does today. This is not "Path: A (in-tree)".
Size: L

## Goal

moq already ships a VAAPI encode backend over `moq-vaapi`
(`rs/moq-video/src/encode/backend/vaapi.rs:24` imports `moq_vaapi::encode::{Config,
Encoder}`), so this is a replacement and hardware-validation of an existing but
unvalidated path, not an additive new backend. The current path is CPU-only, its own
header says "NOT YET VALIDATED ON HARDWARE" (`encode/backend/vaapi.rs:19-21`), it
interleaves every frame to NV12 on the CPU with no GPU-surface path
(`i420_to_nv12` at `:97-111`), it cannot retune (`set_bitrate` returns
`Error::BitrateUnsupported(NAME)` at `:80-88`), and moq-vaapi hard-links libva so a
libva-less host fails to load before fallback (`comparisons/codecs.md:120-135`).

The upgrade contributes the encoder logic into moq-vaapi's encode module: zero-copy
DMA-BUF surface input imported directly as a VA surface, VPP hardware scale and color
conversion when the input dimensions or format do not match the encode target, an
honest `set_bitrate` that retunes where the hardware supports it and returns
`Error::BitrateUnsupported` where it does not, and per-frame forced IDR. This is the
Linux capture-to-encode half of the zero-copy story that pairs with vaapi-decode's
decode-to-render half.

Dependency spine (resolves the earlier incoherence): the encoder logic is authored
into `moq-vaapi`'s `src/encode.rs` module (which today exposes `Config`/`Encoder` with
`encode_nv12`, validated on hardware and extended, but expressed in moq-vaapi's own
diverged bindings, not the crates.io `cros-codecs` crate). The moq-video backend
continues to consume `moq_vaapi::encode`. moq-video does not gain a direct
`cros-codecs` dependency, and moq-vaapi does not gain the entire DMA-BUF-import plus
VPP plus tunings surface twice. The behavior of our `rusty-codecs`
`H264StatelessEncoder` integration (`rusty-codecs/src/codec/vaapi/encoder.rs`) is the
reference specification for the re-vendored moq-vaapi encode logic, not a
drop-in.

## Evidence

- The lopsided comparison and the verdict "upstream ours":
  `comparisons/codecs.md:120-182` (the whole VAAPI-vs-VAAPI section),
  `comparisons/codecs.md:554` (verdict table row).
- moq's current placeholder: `comparisons/maps/moq-video.md:199-206`, verified at
  `rs/moq-video/src/encode/backend/vaapi.rs` (111 lines; CPU `i420_to_nv12` at
  `:97-111`; `set_bitrate` returns `Error::BitrateUnsupported(NAME)` at `:80-88`; the
  "NOT YET VALIDATED ON HARDWARE" header at `:19-21`).
- The moq-side change 5 and requirement U3 driving this plan:
  `comparisons/moq-changes.md:155-178` (the encode dispatch sketch),
  `comparisons/moq-changes.md:657` (change 5 sizing L ~800), `comparisons/zerocopy.md:457-469`.
- Our reference encoder anchors: `comparisons/codecs.md:139-162`,
  `comparisons/maps/rusty-codecs.md:234`.

## moq API consumed

- The B2 encode `Backend` signature: `fn encode(&mut self, frame: &Frame, timestamp:
  Timestamp, keyframe: bool) -> Result<Vec<Packet>, Error>` with `Packet { payload:
  Bytes, timestamp: Timestamp }` (`comparisons/moq-changes.md:319-333`,
  overview "PTS through the encode path"). VAAPI is one-in one-out and can echo the
  frame's timestamp, but it consumes the trait's new signature so the table stays
  uniform.
- The B1 `Frame::DmaBuf(dmabuf::Frame)` input variant
  (`rs/moq-video/src/frame.rs:23-36`) and its `to_i420()` CPU fallback arm for the
  non-DMA-BUF path (`frame.rs:63-74`).
- The existing encode `Config` (`rs/moq-video/src/encode/encoder.rs:55-70`:
  `width`, `height`, `framerate`, `bitrate`, `gop`, `codec`, `kind`), read from hang
  catalog types, not our `config.rs` mirror.
- moq's `Codec::H264` (the public encode enum is `{H264, H265}`,
  `rs/moq-video/src/encode/encoder.rs:21-48`); this backend registers H.264 only.

## Source to port

- `rusty-codecs/src/codec/vaapi/encoder.rs` (1,533 LOC) is the reference
  specification; the encoder is a cros-codecs `StatelessEncoder` integration validated
  on Intel Meteor Lake. Because moq-vaapi uses its own diverged bindings rather than
  the `cros-codecs` crate, this logic is re-vendored into moq-vaapi's `src/encode.rs`,
  not lifted across; the anchors below are the behavior to reproduce there.
- Carries over:
  - DMA-BUF surface input: `build_prime_descriptor` builds a
    `VADRMPRIMESurfaceDescriptor` from the DMA-BUF handle (`encoder.rs:91`), and
    `push_frame` selects the zero-copy path when the frame carries a DMA-BUF handle
    (`encoder.rs:1268-1289`; the `native_handle()` check and import at
    `encoder.rs:1273-1280`).
  - VPP on mismatch, staying on the GPU: `vpp_scale_or_cpu` for wrong dimensions
    (`encoder.rs:1069`) and `vpp_convert_or_cpu` for non-NV12 formats
    (`encoder.rs:1009`), each with a permanent CPU fallback after a failure so a broken
    VPP never loops.
  - Rate-control quality clamping (CBR, QP 18-36 to avoid keyframe compression bursts)
    and the `LowDelay` prediction structure with the H.264 `max_frame_num` floor
    (`comparisons/codecs.md:148-152`), and avcC-free avc3 in-band output (moq emits
    Annex-B in-band only; drop our avcC priming-encoder path,
    `comparisons/codecs.md:114-118,463`).
  - Per-frame forced IDR: our `push_frame` currently hardcodes `force_keyframe: false`
    (`encoder.rs:1352`); cros-codecs `FrameMetadata` supports it one plumbing step
    away, so wire the trait's `keyframe` argument through to the metadata. (The
    encoder also constructs a priming encoder with `force_keyframe: true` at
    `encoder.rs:928`; that is separate and stays internal.)
  - An honest `set_bitrate`: cros-codecs `Tunings` carries `rate_control`
    (`comparisons/codecs.md:160-162`), so implement a real retune where the driver
    supports it and return `Error::BitrateUnsupported` only where it genuinely cannot,
    never a silent no-op (our encoder inherits the no-op default today,
    `comparisons/codecs.md:156-159`).
- Dropped:
  - Our `VideoEncoderFactory`/`VideoEncoder` push/pop trait glue
    (`encoder.rs:1230,1251`); moq's synchronous `encode(...) -> Vec<Packet>` is the
    target. The backend returns zero packets while a frame is still queued and one on
    drain (VAAPI drains per call, so it returns one).
  - Our `config.rs` catalog mirror and `DmaBufInfo` type; use hang catalog types and
    moq's `dmabuf::Frame`.
  - `anyhow` returns; use moq's `Error`.

## Target in moq

- Two edit sites. In the external `moq-vaapi` crate (`moq-dev/vaapi`), extend
  `src/encode.rs` so `Encoder` gains the DMA-BUF PRIME import, VPP scale/convert, honest
  bitrate retune, and forced-IDR behavior, expressed in moq-vaapi's own bindings and
  reusing the crate's existing `surface.rs:341` `export_prime()` and
  `buffer/proc_pipeline.rs` VPP wrapper. In moq-video,
  `rs/moq-video/src/encode/backend/vaapi.rs` keeps consuming `moq_vaapi::encode` (it
  does not switch to a direct `cros-codecs` dependency) and is edited only to feed it
  DMA-BUF frames and thread the new `set_bitrate`/`keyframe` calls through. Keep
  `pub(crate) const NAME: &str = "vaapi";`, the `open(config: &Config)` shape, and the
  existing `unsafe impl Send for Vaapi` (`encode/backend/vaapi.rs:37-40`).
- No new `const Candidate` is needed: the encode candidate table already lists a VAAPI
  H.264 entry (`comparisons/maps/moq-video.md:145`), so this plan changes the backend
  behind the existing row and does not touch the table (unlike vaapi-decode, which
  adds a new row).
- The `i420_to_nv12` CPU helper (`encode/backend/vaapi.rs:97-111`) gives way to the
  DMA-BUF import plus VPP path in moq-vaapi, with a borrowing CPU upload retained only
  as the non-DMA-BUF fallback (see the `to_i420` note under Implementation).

## Implementation steps

1. Confirm B1 (`Frame::DmaBuf`) and B2 (the `timestamp` argument plus `Packet`) are in
   the base branch, and settle the shared moq-vaapi API with vaapi-decode: both leaves
   reuse the crate's existing `export_prime()` and `proc_pipeline` VPP wrapper, so the
   coordination is on the encode-module additions and any shared VPP execution path, not
   on re-adding export (coordination point 3).
2. Extend `moq-vaapi`'s `src/encode.rs` `Encoder` (do not swap moq-video onto a direct
   `cros-codecs` dependency): re-vendor the `StatelessEncoder` setup into moq-vaapi's
   bindings, creating or reusing the shared `Display` and configuring H.264 CBR with the
   QP 18-36 clamp, the `LowDelay` prediction structure, and the `max_frame_num` floor.
   The moq-video backend keeps its `moq_vaapi::encode::{Config, Encoder}` field.
3. Implement the new `encode(frame, timestamp, keyframe) -> Result<Vec<Packet>,
   Error>`: match the frame variant. The trait takes `frame: &Frame`, and the DMA-BUF
   path imports the surface by borrow, so this step does not consume the frame. For
   `Frame::DmaBuf`, import via `build_prime_descriptor` and the `push_frame` dispatch
   (`encoder.rs:91,1268-1289`). For any other variant, download through a *borrowing*
   `to_i420(&self)` accessor. Flag that moq's `decode::Frame::into_i420(self)` consumes
   and its `to_i420` is `pub(crate)` today (`rs/moq-video/src/decode/mod.rs:92`), so a
   public borrowing `to_i420(&self)` must exist; coordinate that with B1/B3 rather than
   cloning the frame. This borrow gap chiefly bites the decode-to-render path, but the
   encoder's CPU fallback needs the same borrowing accessor.
4. Route mismatches through VPP on the GPU: wrong dimensions to `vpp_scale_or_cpu`
   (`encoder.rs:1069`), non-NV12 to `vpp_convert_or_cpu` (`encoder.rs:1009`), each with
   the permanent-CPU-fallback-after-failure guard preserved.
5. Wire `keyframe` through cros-codecs `FrameMetadata::force_keyframe` (replacing the
   hardcoded `false` at `encoder.rs:1352`), and stamp each returned `Packet` with the
   incoming `timestamp`.
6. Implement `set_bitrate` against cros-codecs `Tunings.rate_control`: retune where the
   driver accepts it, return `Error::BitrateUnsupported(NAME)` only where it truly
   cannot. Do not force an IDR on retune (moq's rate-control contract,
   `rs/moq-video/src/encode/backend/mod.rs` `set_bitrate` doc).
7. Delete `i420_to_nv12` and keep the whole thing gated
   `cfg(all(target_os = "linux", feature = "vaapi"))`; confirm non-Linux and
   `vaapi`-off builds compile.

## Tests

- A hardware round-trip test in moq's VideoToolbox/NVENC style: feed a DMA-BUF NV12
  surface (or a synthetic I420 frame through the CPU path), encode, decode the emitted
  Annex-B, and assert the result matches within tolerance and that keyframes appear
  when requested. Mark `#[ignore]` with a reason where no Intel/AMD VA-API device is
  present; validate locally on Meteor Lake.
- A forced-IDR test: `encode(frame, ts, keyframe: true)` yields an access unit the
  decoder treats as random-access.
- A `set_bitrate` test: assert it either applies (readback where feasible) or returns
  `Error::BitrateUnsupported`, never silently succeeds, matching moq's honest-contract
  rule.
- A VPP-fallback assertion: a mismatched-dimension input still encodes (GPU VPP or the
  permanent CPU fallback), and a forced VPP failure does not loop.

## Adaptation notes

- Timestamps are `moq_net::Timestamp` on `Packet`; echo the input timestamp since VAAPI
  is zero-frame-delay.
- Errors are moq's `Error`; reuse the `DmaBufImport` variant (shared with vaapi-decode)
  and add nothing that vaapi-decode already added.
- No ffmpeg. Prefer dlopen for libva so a libva-less host falls back (#1837); at
  minimum do not regress the current behavior.
- The `Packet`-per-frame return expresses the streaming backend without changing moq's
  one-shot public `Encoder` front end (`comparisons/moq-changes.md:471-478`): VAAPI
  returns exactly one packet per input.

## Coordination

- Coordination point 1 (base API freeze): if B2's `Packet`/timestamp shape or B1's
  `Frame::DmaBuf` cannot express the encoder's needs, stop and file against B1/B2.
- Coordination point 2 (shared candidate table): no table edit needed; this plan
  changes the backend behind the existing VAAPI row. Do not refactor the table.
- Coordination point 3 (shared moq-vaapi): surface export (`surface.rs:341`) and the
  VPP parameter-buffer wrapper (`buffer/proc_pipeline.rs`) already exist in moq-vaapi
  and are reused by both leaves, not authored here. This leaf owns the encode-module
  logic; vaapi-decode owns the decode stack. The two agents agree the shared moq-vaapi
  API (encode extensions, any shared VPP execution path) before either lands.
- Coordination point 11 (external moq-vaapi repo, overview): the VA-layer encode work
  is a PR to `github.com/moq-dev/vaapi`, a separate repository under the moq-dev
  org with its own review path and release cadence. The moq-video backend change is a
  separate monorepo PR that bumps the pin. We do not hold publish rights to the crate
  name; the critical path runs through the upstream release.
- Licensing and provenance: moq-vaapi already carries `LICENSE.libva` and
  `LICENSE.cros-codecs` alongside its BSD-3-Clause `LICENSE`, and is self-described as
  derived from discord/cros-libva and discord/cros-codecs. Our re-vendored encode logic
  originates from the same cros-codecs BSD-3-Clause source, so the contribution matches
  the crate's existing provenance and license; carry the same attribution.

## Transcode and rate control (overview coordination point 7)

moq-transcode drives this backend through the public `encode::{Kind, Config,
Encoder}` front end: it selects by `Kind`, sets a per-rung CBR target through
`Config.bitrate` at construction, and forces an IDR per group. This backend must
therefore honor `Config.bitrate` as a per-encoder CBR target, expose a QP or
target-quality knob for custom per-GOP rate control, and keep `set_bitrate`
honest and free of any forced-IDR side effect. Add an encoder-session-reuse path
so a per-group transcode loop does not pay a fresh VA-context open per group.
Scope this precisely: neither cros-codecs' `H264StatelessEncoder` nor moq-vaapi's
`encode.rs` `Encoder` has an in-place resolution or GOP reconfigure, and our
encoder constructs via `new` with no `reset`
(`rusty-codecs/src/codec/vaapi/encoder.rs`, `grep 'fn reset'` empty; contrast the
decoder's `reset()@731`). So "reuse" means holding the VA `Display` and the
encoder session across groups and resetting the rate-control state and forcing a
fresh IDR, rather than reopening a VA context per group. If a same-resolution
same-GOP session cannot re-arm rate control without a rebuild, the fallback is a
controlled teardown and rebuild of the encoder session over a retained `Display`.
This is new work on moq-vaapi (or cros-codecs), coordinated with moq's
per-segment-transcode goal, not a one-liner. Defer the rate-control policy to
moq-transcode; do not embed a streaming controller.

## Acceptance checklist

- `encode/backend/vaapi.rs` consumes `Frame::DmaBuf` zero-copy, routes mismatches
  through VPP, and falls back to CPU I420 upload for other variants.
- `set_bitrate` is honest (real retune or `BitrateUnsupported`, never a no-op); no IDR
  forced on retune.
- Per-frame forced IDR works through the trait; each `Packet` carries the frame's
  timestamp.
- The "NOT YET VALIDATED ON HARDWARE" caveat is gone, replaced by a passing (or
  `#[ignore]`d-with-reason) hardware round-trip test.
- No new candidate-table row. Encode logic authored into moq-vaapi's `src/encode.rs`
  (landed in `moq-dev/vaapi`, released, pinned); moq-video keeps consuming
  `moq_vaapi::encode` with no direct `cros-codecs` dependency. Existing moq-vaapi export
  and VPP wrapper reused, not duplicated.
- Non-Linux and `vaapi`-off builds compile; conventional commit(s) with `moq-video`
  (and `moq-vaapi`) scope.
