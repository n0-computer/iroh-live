# SPS VUI low-latency patcher

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path. The patcher rewrites SPS NAL bytes; it moves
> no pixel surfaces. See ../zerocopy.md for the paths that do.

## What it is

Our `codec/h264/sps.rs` is an exp-golomb SPS VUI patcher that rewrites
`max_num_reorder_frames = 0` and `max_dec_frame_buffering = 1`, telling a
conforming decoder that no DPB reordering delay is required on Baseline streams
and stripping the reordering latency. moq's bitstream handling covers parameter-
set parsing and injection but has no VUI patcher of any kind, so this is offered
upstream as an optional, opt-in decode pass. The local file is `#[allow(dead_code)]`
today and is deleted ungated; the leaf ports the patcher logic from git history,
so nothing gates the local deletion on the upstream offer.

## iroh-live vs moq

Ours: `codec/h264/sps.rs` (586 L) patches SPS NALs in Annex-B data to insert VUI
with `max_num_reorder_frames = 0` and `max_dec_frame_buffering = 1` (module doc
`sps.rs:1-13`). It carries an exp-golomb bit reader and writer (`read_ue` at
`:17`), VUI-flag location (`find_vui_flag_offset` at `:105`), and the patch entry
point (`patch_sps_nal_low_latency` at `:294`). It returns `None` when VUI is
already present or parsing fails (`sps.rs:292-294`), so it is a no-op where it does
not apply. It is `#[allow(dead_code, reason = "SPS patcher kept for potential
future use")]` at `rusty-codecs/src/codec/h264.rs:4-5`.

Theirs: nothing. moq's `moq_mux::codec` is more general than our annexb helpers on
the overlapping paths (variable length size, multiple parameter sets, hvcC/VPS;
see codec-annexb.md), but it has no VUI patcher at all. The patcher is the one
useful trick moq lacks entirely.

## What to do

Offer the patcher to moq as an optional pass (branch `up/bitstream-sps-vui`), and
delete the local dead code independently.

Depends on nothing from the frozen base contract: this is an optional pass over
Annex-B NAL data, not a backend, and touches no frame vocabulary. It is
independent and must block nothing.

Source to port:

- `rusty-codecs/src/codec/h264/sps.rs` (586 LOC). Carries over: the exp-golomb
  parse and rewrite, the VUI-flag location, the VUI insertion, and the
  `patch_sps_nal_low_latency` entry point. Dropped: our `annexb.rs` conversion
  helpers, since `moq_mux::codec` already handles variable `lengthSizeMinusOne`,
  multiple parameter sets, and hvcC (see codec-annexb.md). Since the local file is
  dead code, the leaf ports from git history rather than from a live call site.

Target in moq:

- Most likely `rs/moq-mux/src/codec` beside `h264::Avcc::parse`,
  `annexb::build_prefix`, and `annexb::from_length_prefixed`, which already own
  parameter-set parsing and in-band injection (moq's decode front end at
  `decode/decoder.rs:94-140,163-176`). The patcher would be an optional pass
  invoked by an H.264 encoder backend or the container producer to force a
  low-latency VUI on emitted SPS.
- Alternatively an encoder-side pass, next to the VideoToolbox backend's existing
  AVCC-to-Annex-B rewrite (`encode/backend/videotoolbox.rs:1-14`). Open question:
  where the pass lives (a bitstream utility module or an encoder-side pass);
  current proposal: offer it as an optional bitstream pass and let upstream review
  settle the placement.

Implementation steps:

1. Port `patch_sps_nal_low_latency` and its exp-golomb reader and writer into
   moq's bitstream module, adapting to moq's NAL types rather than our `annexb.rs`.
2. Expose it as an optional, opt-in pass, off by default, so existing encoder
   output is unchanged unless a caller asks for the low-latency VUI. Do not make it
   automatic; the reorder assumptions only hold for Baseline streams with
   reordering disabled.
3. Guard against streams that already carry VUI (our patcher returns `None` when
   VUI is present or parsing fails, `sps.rs:292-294`), so the pass is a no-op where
   it does not apply.

iroh-live removal (local dead-code delete): delete
`rusty-codecs/src/codec/h264/sps.rs` (586 LOC) and its `#[allow(dead_code)]` gate.
It is dead code today, deletable now with the independent local deletions
(cut-plan rusty-codecs table), and the upstream offer does not gate this deletion.

## Tests

- A unit test that feeds a known Baseline SPS lacking VUI, applies the pass, and
  asserts the rewritten SPS parses back with `max_num_reorder_frames = 0` and
  `max_dec_frame_buffering = 1`.
- A no-op test that an SPS already carrying VUI is returned unchanged.
- Both are pure bit manipulation, hardware-free, and run in CI everywhere.
- The patcher is dead code in our tree today, so it carries no production track
  record; present it as a reviewed, tested offer, not a battle-hardened path.

## Adaptation notes

- No dependencies added; the patcher is pure bit manipulation.
- Errors and option types adopt moq's conventions.

## Evidence

- ../comparison/codecs.md, section 7 (bitstream helpers), the note that the VUI
  patcher is the one useful trick moq lacks and the verdict to offer it upstream as
  an optional pass that is dead code today and should block nothing.
- ../comparison/maps/rusty-codecs.md, section 2 (H.264 bitstream helpers), `sps.rs`
  as the exp-golomb SPS VUI patcher, currently `#[allow(dead_code)]`.

## Coordination

- No coordination-list dependency: independent and optional. It must not block any
  other leaf. If moq prefers to fold the logic into moq-mux itself or to decline
  it, that is an acceptable outcome; present the optional, may-be-declined framing
  in the PR.
- Local deletion is ungated: the dead-code file is removed with the independent
  local deletions regardless of whether the upstream offer is accepted.
- Sibling cross-reference: codec-annexb.md (the annexb helpers the patcher sits
  beside, which are adopt-theirs from `moq_mux::codec` while the patcher is the one
  piece worth offering).
</content>
