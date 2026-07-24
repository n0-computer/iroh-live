# Codec backend dispatch and selection

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path. Dispatch selects a backend; it moves no
> pixels. See ../zerocopy.md for the paths that do.

## What it is

The dispatch layer picks which concrete encoder or decoder backend to construct.
Ours is a two-layer enum-plus-probe design; moq's is a data-driven Candidate
table filtered by a `Kind` selector. moq's is better engineering and more capable
at the selection level (backend pinning, tried-list errors, one table row per
backend), so we adopt theirs and delete ours. The decoder-lifecycle needs our
dispatcher forwards (`reset()`, `burst_size()`) are carried into moq's decode
trait, not lost. This is the last codec cut, because the dispatcher must not name
a backend that still exists locally.

## iroh-live vs moq

Ours: two layers. `codec.rs` enumerates concrete encoder backends as a strum enum
(`"h264-vaapi"`, `"h264-vtb"`, ...) with `available()`, `best_available()`
(hardware preferred), and `create_encoder()` (`codec.rs:97-216`).
`codec/dynamic.rs` holds `DynamicVideoDecoder`, whose `new()` hardcodes the probe
order VAAPI, V4L2, VideoToolbox, Android HW, Android ByteBuffer, then falls back
to openh264 (`dynamic.rs:83-134`), governed only by `DecoderBackend::Auto |
Software` (`format.rs:905-916`). The decoder trait carries `reset()`,
`set_viewport()`, and `burst_size()` (`traits.rs:379-410`), which the dispatcher
forwards.

Theirs: a data-driven `Candidate` table per direction (name, supported codecs,
`open` fn pointer), filtered by codec, ordered by `Kind::{Auto, Hardware,
Software, Named(String)}`, trying each in order and returning `NoEncoder`/
`NoDecoder` errors that list what was tried (`moq:encode/backend/mod.rs:60-133`,
`moq:decode/backend/mod.rs:89-145`). The decode config adds `latency_max` and
best-effort `resize` (`moq:decode/decoder.rs:43-58`).

Honest comparison: `Auto` behaves the same on both sides (attempt hardware, fall
through on error, land on software). Theirs is stronger at selection: `Named`
enables pinning a backend (their tests depend on it, ours cannot express it),
`Hardware` enables fail-fast policies, adding a backend is one table row instead
of edits to an enum, a macro, and a probe chain, and the tried-list errors are
worth real debugging time. Ours is stronger at the decoder-lifecycle level: their
trait has no `reset()` for post-loss hardware reinit and no `burst_size()`, which
our hardware decoders need. Their `resize` is honored by NVDEC for free; our
`set_viewport` is a CPU post-scale, which is presentation logic and should die
with the rest of it (moq uses `Config::resize`).

## What to do

Adopt moq-video's Candidate/Kind dispatch and delete ours.

- Removal (adopt-theirs, after the backend leaves): delete `rusty-codecs/src/
  codec.rs` and `rusty-codecs/src/codec/dynamic.rs` (522 LOC combined). Replace
  with moq-video's Candidate/Kind model.
- Sequencing (the codec removal sequencing, step 9): dispatch is last, only after
  every held backend above is cut (VAAPI, V4L2, Android admitted upstream; the
  adopt-theirs backends already gone). It must not name a backend that still
  exists locally.
- Port upstream first, not lost: our `reset()` and `burst_size()` decoder needs
  are ported into moq's decode `Backend` trait via the VAAPI decode contribution
  (codec-vaapi-decode.md), because moq's stateful hardware decoders beyond NVDEC
  need post-loss reinit and a DPB-flush burst hint once they land. Those trait
  additions travel with the first stateful HW decoder upstreamed, not with this
  cut.
- Dropped, not ported: `set_viewport()` is presentation logic (a CPU post-scale).
  moq expresses the equivalent through `Config::resize` (honored by NVDEC in
  hardware), so `set_viewport` dies with the dispatch cut rather than being
  carried upstream.

## Tests

- The conformance harness (`rusty-codecs/src/codec/tests/`) and
  `moq-media/tests/pipeline_integration.rs` pass with moq's dispatch selecting the
  adopted backends before the deletion commit (the proof-before-deletion rule).
- moq's own dispatch tests, which depend on `Kind::Named` pinning, cover the
  Candidate model. Backend selection is pure software logic, so CI verifies it
  fully; the hardware-gated `zero_copy_pipeline.rs` covers the backends the
  dispatcher lands on, per platform.

## Evidence

- ../comparison/codecs.md, section 8 (dynamic dispatch and selection), verdict
  "cut and replace with their Candidate/Kind model".
- ../comparison/codecs.md, section 10 (verdict table, dispatch/selection row) and
  section 9 (rate control, for the `set_bitrate` trait contract the Candidate
  backends carry).
- ../comparison/maps/moq-video.md and ../comparison/maps/rusty-codecs.md.

## Coordination

- Release gate plus every held backend admitted: this is the last codec cut. It
  waits on the release bump and on VAAPI, V4L2, and Android all being upstreamed,
  with `reset()` and `burst_size()` carried into moq's decode trait.
- The trait additions (`reset`, burst hint) are created upstream by
  codec-vaapi-decode.md, the first stateful HW decoder to land; this module
  depends on that but does not itself author the trait change.
- Sibling cross-reference: codec-vaapi-decode.md (owns the `reset()`/
  `burst_size()` port into moq's decode trait that this cut relies on).
</content>
