# vaapi-encode. VAAPI H.264 encode with DMA-BUF input, VPP, and honest set_bitrate

Branch: moq-upstream/vaapi-encode          PR target: base branch, then moq main
Depends on: B1 (frame vocabulary), B2 (PTS through encode); builds on vaapi-decode's moq-vaapi growth
Path: A (in-tree)
Size: L

## Goal

Replace moq's 111-line CPU-only VAAPI encode backend, whose own header says "NOT YET
VALIDATED ON HARDWARE", with our hardware-validated cros-codecs `StatelessEncoder`
integration: zero-copy DMA-BUF surface input imported directly as a VA surface, VPP
hardware scale and color conversion when the input dimensions or format do not match
the encode target, an honest `set_bitrate` that retunes where the hardware supports it
and returns `Error::BitrateUnsupported` where it does not, and per-frame forced IDR
through the trait. Today moq's backend interleaves every frame to NV12 on the CPU with
no GPU-surface path, cannot retune, and hard-links libva so a libva-less host fails to
load before fallback (`rs/moq-video/src/encode/backend/vaapi.rs:16-21,80-88`,
`comparisons/codecs.md:120-135`). This is the Linux capture-to-encode half of the
zero-copy story that pairs with vaapi-decode's decode-to-render half.

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

- `rusty-codecs/src/codec/vaapi/encoder.rs` (1,533 LOC) is the reference; the encoder
  is a cros-codecs `StatelessEncoder` integration validated on Intel Meteor Lake.
- Carries over:
  - DMA-BUF surface input: `build_prime_descriptor` builds a
    `VADRMPRIMESurfaceDescriptor` from the DMA-BUF handle (`encoder.rs:87-124`), and
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

- Rewrite `rs/moq-video/src/encode/backend/vaapi.rs` in place. Keep `pub(crate) const
  NAME: &str = "vaapi";` and the `open(config: &Config) -> Result<Box<dyn Backend>,
  Error>` constructor shape, replacing the CPU-only body. The existing `unsafe impl
  Send for Vaapi` justified by encode-thread confinement
  (`encode/backend/vaapi.rs:37-40`) is kept and extended to the cros-codecs objects.
- No new `const Candidate` is needed: the encode candidate table already lists a VAAPI
  H.264 entry (`comparisons/maps/moq-video.md:145`), so this plan changes the backend
  behind the existing row and does not touch the table (unlike vaapi-decode, which
  adds a new row).
- `moq-vaapi` surface-export and VPP additions are consumed, not authored, here: they
  are provided by the vaapi-decode branch (coordination point 3). The `i420_to_nv12`
  CPU helper (`encode/backend/vaapi.rs:97-111`) is deleted in favor of the DMA-BUF
  import plus VPP path, with `to_i420()` upload retained only as the non-DMA-BUF
  fallback.

## Implementation steps

1. Confirm B1 (`Frame::DmaBuf`) and B2 (the `timestamp` argument plus `Packet`) are in
   the base branch, and that the moq-vaapi VPP and surface-import additions from
   vaapi-decode are available at an agreed API shape (coordination point 3). If not,
   coordinate rather than duplicating them.
2. Replace the backend struct's `moq_vaapi::encode::Encoder` field with the cros-codecs
   `StatelessEncoder` setup: create the shared `Display`, configure H.264 CBR with the
   QP 18-36 clamp and the `LowDelay` prediction structure and `max_frame_num` floor.
3. Implement the new `encode(frame, timestamp, keyframe) -> Result<Vec<Packet>,
   Error>`: match the frame variant. For `Frame::DmaBuf`, import via
   `build_prime_descriptor` and the `push_frame` dispatch
   (`encoder.rs:87-124,1268-1289`). For any other variant, upload `frame.to_i420()`
   through the existing CPU path.
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
- Coordination point 3 (shared moq-vaapi): the surface-export and VPP additions are
  owned by vaapi-decode. Treat them as provided, coordinate on their shape, and do not
  duplicate them. If authored in parallel, the two agents agree the moq-vaapi API
  before either lands.

## Acceptance checklist

- `encode/backend/vaapi.rs` consumes `Frame::DmaBuf` zero-copy, routes mismatches
  through VPP, and falls back to CPU I420 upload for other variants.
- `set_bitrate` is honest (real retune or `BitrateUnsupported`, never a no-op); no IDR
  forced on retune.
- Per-frame forced IDR works through the trait; each `Packet` carries the frame's
  timestamp.
- The "NOT YET VALIDATED ON HARDWARE" caveat is gone, replaced by a passing (or
  `#[ignore]`d-with-reason) hardware round-trip test.
- No new candidate-table row; moq-vaapi additions consumed, not duplicated.
- Non-Linux and `vaapi`-off builds compile; conventional commit with `moq-video` scope.
