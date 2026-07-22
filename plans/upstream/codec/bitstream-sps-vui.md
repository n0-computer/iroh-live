# bitstream-sps-vui. Offer the SPS VUI low-latency patcher as an optional pass

> Campaign: upstream | Kind: leaf plan | Branch: up/bitstream-sps-vui |
> PR target: base branch, then moq main | Read ../0-overview.md first.

Depends on: independent
Path: independent (optional offer)
Size: S

## Goal

Offer moq our SPS VUI low-latency patcher as an optional pass for its H.264
encoders. The patcher rewrites SPS NAL units to set `max_num_reorder_frames = 0`
and `max_dec_frame_buffering = 1`, telling a conforming decoder that no DPB
reordering delay is required on Baseline streams and stripping the reordering
latency. moq's existing bitstream handling in `moq_mux::codec` and its decode
front end covers parameter-set parsing and in-band injection but has no VUI
patcher of any kind, and our patcher is currently dead code in our own tree. This
is a genuinely useful decoder-latency trick, offered as a nice-to-have that moq
may fold into moq-mux or decline. It is independent of the base API and should
block nothing.

## Evidence

- `comparisons/codecs.md` section 7 (bitstream helpers): the honest comparison
  where moq's `moq_mux::codec` is more general on the overlapping paths (variable
  length size, multiple parameter sets, hvcC and VPS), and the VUI patcher is the
  one useful trick moq lacks entirely, with the verdict to offer it upstream as an
  optional pass that is dead code today and should not block anything.
- `comparisons/maps/rusty-codecs.md` section 2 (H.264 bitstream helpers): `sps.rs`
  as the exp-golomb SPS VUI patcher, currently `#[allow(dead_code)]`.

## moq API consumed

None from the frozen base contract. This is an optional pass over Annex-B NAL
data, not a backend, and touches no frame vocabulary.

## Source to port

- `rusty-codecs/src/codec/h264/sps.rs` (586 LOC). The module patches SPS NALs in
  Annex-B data to insert VUI with `max_num_reorder_frames = 0` and
  `max_dec_frame_buffering = 1` (module doc `h264/sps.rs:1-13`). It carries an
  exp-golomb bit reader and writer (`read_ue` at `:17`), VUI-flag location
  (`find_vui_flag_offset` at `:105`), and the patch entry point
  (`patch_sps_nal_low_latency` at `:294`). It is `#[allow(dead_code, reason =
  "SPS patcher kept for potential future use")]` at `rusty-codecs/src/codec/h264.rs:4-5`.
- Context: `rusty-codecs/src/codec/h264/annexb.rs` (364 LOC) is the NAL tooling
  the patcher sits beside; moq's overlapping logic is more general, so only the
  VUI patcher is worth offering.

Carries over: the exp-golomb parse and rewrite and the VUI insertion. Dropped:
our `annexb.rs` conversion helpers, since `moq_mux::codec` already handles
variable `lengthSizeMinusOne`, multiple parameter sets, and hvcC, all of which
our helpers do not.

## Target in moq

- Most likely `rs/moq-mux/src/codec` beside `h264::Avcc::parse`,
  `annexb::build_prefix`, and `annexb::from_length_prefixed`, which already own
  parameter-set parsing and in-band injection (`comparisons/codecs.md` section 7,
  citing moq's decode front end at `decode/decoder.rs:94-140,163-176`). The
  patcher would be an optional pass invoked by an H.264 encoder backend or the
  container producer to force a low-latency VUI on emitted SPS.
- Alternatively an encoder-side pass, next to the VideoToolbox backend's existing
  AVCC-to-Annex-B rewrite with format-description splicing
  (`encode/backend/videotoolbox.rs:1-14`). Open question: where the pass lives
  (a bitstream utility module or an encoder-side pass), discussed in this
  section; current proposal: offer it as an optional bitstream pass and let
  upstream review settle the placement.

## Implementation steps

1. Port `patch_sps_nal_low_latency` and its exp-golomb reader and writer into
   moq's bitstream module, adapting to moq's NAL types rather than our
   `annexb.rs`.
2. Expose it as an optional, opt-in pass, off by default, so existing encoder
   output is unchanged unless a caller asks for the low-latency VUI. Do not make
   it automatic; the reorder assumptions only hold for Baseline streams with
   reordering disabled.
3. Guard against streams that already carry VUI (our patcher returns `None` when
   VUI is present or parsing fails, `sps.rs:292-294`), so the pass is a no-op
   where it does not apply.

## Tests

- A unit test that feeds a known Baseline SPS lacking VUI, applies the pass, and
  asserts the rewritten SPS parses back with `max_num_reorder_frames = 0` and
  `max_dec_frame_buffering = 1`.
- A no-op test that an SPS already carrying VUI is returned unchanged.

## Adaptation notes

- No dependencies added; the patcher is pure bit manipulation.
- Errors and option types adopt moq's conventions.
- This is dead code in our tree today, so it carries no production track record;
  present it as a reviewed, tested offer, not a battle-hardened path.

## Coordination

None from the coordination list. It is independent and optional. It must not
block any other leaf; if moq prefers to fold the logic into moq-mux itself or to
decline it, that is an acceptable outcome.

## Acceptance checklist

- The VUI patcher ported into moq's bitstream module as an opt-in, off-by-default
  pass.
- Applies only to Baseline SPS lacking VUI; a no-op otherwise.
- Unit tests for both the patch and the no-op cases.
- The optional, nice-to-have, may-be-declined framing stated in the PR.
