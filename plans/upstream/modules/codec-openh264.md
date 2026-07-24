# openh264 H.264 software codec

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path. openh264 takes and produces CPU I420, so
> removing our version regresses no GPU path (see ../zerocopy.md, section
> "Decode to render: verdict ours").

## What it is

The openh264 software H.264 encode and decode backends are the universal
compile-everywhere fallback on every platform. Both sides wrap the same vendored
Cisco encoder with the same core tuning, so the codec engines are identical; the
wrappers differ in runtime adaptability and framing. moq's version wins on the
capabilities that matter for live publishing (tested live bitrate retune and
per-frame forced IDR), and its avc1 front-end layering is cleaner, so we adopt
moq's and delete ours.

## iroh-live vs moq

Both wrap the Cisco encoder with `UsageType::CameraVideoRealTime`,
`RateControlMode::Bitrate`, and GOP via `IntraFramePeriod` (ours
`codec/h264/encoder.rs:118-123`, theirs `moq:encode/backend/openh264.rs:35-41`),
and both default bitrate from the same 0.07 bits-per-pixel heuristic
(`codec/h264/encoder.rs:106`, `moq:encode/encoder.rs:94-100`). The real
differences all favor moq or are presentation logic on our side:

- Dynamic bitrate. Theirs implements live retune through raw `openh264-sys2` FFI
  (`ENCODER_OPTION_BITRATE` + `SBitrateInfo`), including deferred application
  before the first frame (openh264 rejects `SetOption` with `cmInitExpected`
  until the encoder lazily exists) and supersede-the-deferred-value semantics,
  pinned by four regression tests including read-back verification (the
  `pending`/`started` fields and the `a_live_set_supersedes_a_deferred_one`
  test). Ours has no `set_bitrate` at all; it inherits the silent no-op default
  (`traits.rs:352`), so software-path congestion adaptation is impossible today.
- Forced IDR. Theirs forces an IDR on any frame via `encode(frame, keyframe:
  true)` calling `force_intra_frame()`. Ours calls `force_intra_frame()` only
  once, internally, after consuming the priming frame in avcC mode
  (`codec/h264/encoder.rs:136-137`); the trait offers no way for a caller to
  request a keyframe.
- Framing modes. Ours supports both Annex-B in-band output and avcC
  (length-prefixed) output, extracting SPS/PPS by encoding a black priming frame
  (`codec/h264/encoder.rs:128-142,274-284`). Theirs emits Annex-B in-band only
  (avc3 shape); no avc1 output exists anywhere in moq-video.
- Decode layering. Theirs is a minimal Annex-B-in, tight-I420-out shim
  (`moq:decode/backend/openh264.rs:44-80`); avc1 handling, parameter-set
  injection, and keyframe gating live in the shared front end
  (`moq:decode/decoder.rs:94-160`). Ours handles avc3 and avc1 in the backend
  (`codec/h264/decoder.rs:62-67`), converts to RGBA or BGRA with a reused pixel
  buffer (`:113-147`), and applies viewport downscaling after decode
  (`:83-85,163-166`). Their per-backend-free avc1 conversion is cleaner; our
  RGBA and viewport work is presentation logic that belongs in a render layer.

## What to do

Adopt moq-video openh264 encode and decode, and delete ours.

- Removal (adopt-theirs, release bump): delete
  `rusty-codecs/src/codec/h264/encoder.rs` and
  `rusty-codecs/src/codec/h264/decoder.rs` (~906 LOC combined). The replacement,
  moq-video's openh264 backends plus the `moq_mux::codec` avc1 front end, is
  already on moq main, so this lands with the release bump and no gated upstream
  contribution.
- Sequencing (from the codec removal sequencing, step 2): adopt behind a
  `moq-native-codecs` cargo feature, run both paths through the conformance
  harness, flip the default, then delete in a deletion-only commit. openh264 is
  the software fallback on every platform and regresses no zero-copy path
  (openh264 output is CPU I420).
- Nothing to port upstream. The avcC (avc1) output mode is the only capability
  we hold that moq lacks; it is parked, not lost, recorded in the parity-ports
  register, because avc1 output only matters for fMP4 export, which moq-mux
  containers cover differently. See the shared bitstream handling in
  codec-annexb.md.

## Tests

The rusty-codecs conformance harness (`rusty-codecs/src/codec/tests/`) and
`moq-media/tests/pipeline_integration.rs` must pass with the adopted decoder in
place, and the latency tests must not regress, before the deletion commit
(proof-before-deletion, the proof-before-deletion rule). moq's own openh264 tests, including the
four bitrate regression tests, cover the adopted backend; our conformance
vectors for openh264 retire as those cover them. CI verifies this fully: openh264
is pure software with no hardware gate.

## Evidence

- ../comparison/codecs.md, section 1 (H.264), subsection "openh264 vs openh264"
  and the verdict "cut and replace with theirs".
- ../comparison/codecs.md, section 9 (rate control) and section 10 (verdict
  table, H.264 encode/decode openh264 rows).
- ../comparison/maps/moq-video.md and ../comparison/maps/rusty-codecs.md for the
  per-side backend inventory.

## Coordination

- Release gate: waits only on the pin bump to the moq release carrying the merged
  media stack (already on main); no gated upstream leaf.
- Proof-before-deletion (the proof-before-deletion rule): feature flag, both paths tested, default
  flipped, deletion-only commit.
- Sibling dependency: the shared bitstream front end that replaces our decode
  parameter-set handling is covered in codec-annexb.md; the last codec cut that
  removes the dispatcher naming this backend is codec-dispatch.md.
</content>
</invoke>
