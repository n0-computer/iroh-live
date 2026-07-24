# H.264 Annex-B and avcC bitstream helpers

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path. Bitstream helpers manipulate compressed NAL
> byte streams, not pixel surfaces. See ../zerocopy.md for the paths that do.

## What it is

Our `codec/h264/annexb.rs` provides Annex-B NAL iteration, SPS/PPS extraction,
avcC construction and parsing, and both directions of Annex-B and
length-prefixed conversion. moq's equivalent lives in `moq_mux::codec` and its
decode front end and is more general on every overlapping path (variable length
size, multiple parameter sets, and hvcC/VPS for H.265, none of which ours
handles). We adopt `moq_mux::codec` and delete ours. The one direction ours has
that moq lacks entirely, avcC production, is parked because moq never emits avc1.

## iroh-live vs moq

Ours: `codec/h264/annexb.rs` (364 L) provides a lazy Annex-B NAL iterator
(`:1-61`), SPS/PPS extraction (`:72-87`), avcC construction (`build_avcc`,
`:90-111`), avcC parsing back to Annex-B (`:115-161`), and both directions of
Annex-B and length-prefixed conversion (`:164-192`).

Theirs: the equivalent logic lives in `moq_mux::codec` and the decode front end.
`h264::Avcc::parse` and `h265::Hvcc::parse` handle the description records,
`annexb::build_prefix` assembles parameter-set prefixes, and
`annexb::from_length_prefixed(payload, length_size, prefix)` converts with the
actual length size from the record while injecting parameter sets ahead of
keyframes (`moq:decode/decoder.rs:94-140,163-176`). On the encode side the
VideoToolbox backend does its own AVCC-to-Annex-B rewrite with
format-description splicing (`moq:encode/backend/videotoolbox.rs:1-14`).

Honest comparison: theirs is more general where the two overlap. Our
`length_prefixed_to_annex_b` hardcodes 4-byte lengths (`annexb.rs:164-178`) where
theirs honors `lengthSizeMinusOne`; our `build_avcc` emits exactly one SPS and one
PPS where their parser accepts several; and theirs covers H.265 (hvcC, VPS) which
we do not touch. Ours is more general only in producing avcC (moq never emits
avc1), and the SPS VUI patcher (a separate module, codec-bitstream-sps-vui.md) has
no counterpart anywhere in moq.

## What to do

Adopt `moq_mux::codec` and delete our annexb helpers.

- Removal (adopt-theirs, release bump): delete
  `rusty-codecs/src/codec/h264/annexb.rs` (364 LOC). The replacement,
  `moq_mux::codec` plus the decode front end that injects parameter sets, is
  already on moq main, so this lands with the release bump. It is deleted together
  with the openh264 backends it sits beside (the codec removal sequencing, step 2).
- Parked, not lost: `build_avcc` (avc1 production) is the one residual piece. It
  is parked, recorded in the parity-ports register, and only matters if an avc1
  output mode is ever wanted upstream. moq never emits avc1, so nothing depends
  on it.
- Nothing else to port. The overlapping conversion helpers are strictly less
  general than `moq_mux::codec`, so they are dropped, not upstreamed.

## Tests

- The conformance harness and `moq-media/tests/pipeline_integration.rs` pass with
  the `moq_mux::codec` front end handling parameter-set parsing and injection
  before deletion (the proof-before-deletion rule). Bitstream manipulation is pure software, so CI
  verifies it fully.
- moq's own decode-decoder tests cover the variable-length-size and multi
  parameter-set paths that our helpers did not.

## Evidence

- ../comparison/codecs.md, section 7 (bitstream helpers), verdict "cut and replace
  with `moq_mux::codec`".
- ../comparison/codecs.md, section 10 (verdict table, bitstream helpers row).
- ../comparison/maps/rusty-codecs.md, section 2 (H.264 bitstream helpers).

## Coordination

- Release gate: waits only on the pin bump (already on moq main); no gated
  upstream leaf.
- Proof-before-deletion (the proof-before-deletion rule): the `moq_mux::codec` path is tested in the
  harness before deletion.
- Sibling cross-references: codec-openh264.md (deleted alongside these helpers,
  since the openh264 decoder's parameter-set handling moves to the shared front
  end), codec-bitstream-sps-vui.md (the SPS VUI patcher that sits beside annexb
  and is offered upstream separately rather than dropped).
</content>
