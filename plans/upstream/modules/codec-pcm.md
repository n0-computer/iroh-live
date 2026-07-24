# PCM Codec

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

An uncompressed PCM codec: raw interleaved little-endian f32 in fixed 20 ms
frames, deliberately matched to Opus framing so the pipeline behaves identically
without compression or lookahead confounding latency measurements. iroh-live has
a full `PcmEncoder`/`PcmAudioDecoder` pair; moq-audio has no uncompressed codec.
The verdict is move because PCM is a required capability (the uncompressed path
for latency isolation), it slots into moq-audio's `#[non_exhaustive]` codec enum
trivially, and hang lives in the same monorepo so the catalog variant lands in
the same branch. If moq declines the offer, iroh-live keeps its PCM codec and the
capability is never lost either way.

## iroh-live vs moq

iroh-live's PCM codec (`rusty-codecs/src/codec/pcm/`, 559 LOC) is a complete
pair: 20 ms frames of little-endian f32 chosen to match Opus framing
(`pcm/encoder.rs:11-16`, `:44-64`), a truthful computed bitrate of
`rate * channels * 32` (`pcm/encoder.rs:75`), and a decoder that reuses the same
resample-plus-remix path as Opus (`pcm/decoder.rs:36-49,68-96`). Bit-exact
roundtrip is asserted at `pcm/encoder.rs:213-233`. It exists for test pipelines
and for debugging codec-versus-transport problems, and it is used as the
uncompressed publish path behind the `pcm` feature in moq-media
(`moq-media/src/publish.rs:819-821`).

moq has nothing here. Its `Codec` enum is single-variant `Opus` with a comment
rejecting a trait until AAC lands
(`3a3e0ea8:rs/moq-audio/src/encode/encoder.rs:1-7,24-42`). More decisively, the
hang catalog has no PCM codec variant: `AudioCodec` is
`{AAC, Opus, Mp2, Ac3, Ec3, Unknown}`, so a PCM track would ride
`Unknown("pcm")` with no interop meaning, and browsers cannot consume it either.
This module closes that gap by adding both the codec and the catalog variant.

## What to do

MOVE. A Path A in-tree contribution to moq-audio plus a matching additive
catalog change to hang, both in the same branch since both live in the moq
monorepo. Independent of the base plans, size S.

### Contribute upstream (moq-audio + hang)

Source to port from iroh-live:

- `rusty-codecs/src/codec/pcm/encoder.rs` (234 LOC): raw interleaved
  little-endian f32 in fixed 20 ms frames, framing deliberately matched to Opus
  (`FRAME_DURATION_MS = 20` at `pcm/encoder.rs:12`, LE-f32 packing at `:15-16`,
  frame accumulation at `:46`), the truthful computed bitrate
  `rate * channels * 32` (`:75`), and the bit-exact round-trip assertion
  (`:213-233`).
- `rusty-codecs/src/codec/pcm/decoder.rs` (310 LOC): reuses the same
  resample-plus-remix path as Opus (`pcm/decoder.rs:51-85`).

Together 559 dependency-free LOC serving test and diagnostics paths. Carries
over: the framing, the truthful bitrate, and the round-trip test. Dropped: the
`AudioEncoder`/`AudioDecoder` trait glue; the moq side dispatches by
`match config.codec` on the concrete structs.

Targets in moq:

- `rs/moq-audio/src/encode/encoder.rs`: add a `Pcm` variant to the `Codec` enum
  (`encode/encoder.rs:24-42`), extend `Codec::as_str` to return `"pcm"`, and add
  a PCM arm to the `match config.codec` dispatch, keeping the concrete-struct
  shape moq chose over a trait.
- New PCM encode and decode paths in moq-audio, following the existing Opus
  module layout.
- hang's catalog: add an `AudioCodec::Pcm` variant beside
  `{AAC, Opus, Mp2, Ac3, Ec3, Unknown}`, in the same branch, so moq-audio's
  `Encoder::catalog()` can emit a first-class PCM `AudioConfig` instead of
  `Unknown("pcm")`.

Ordered implementation steps:

1. Add `Codec::Pcm` and its `as_str` mapping, additively, on the
   `#[non_exhaustive]` enum.
2. Add hang's `AudioCodec::Pcm` catalog variant, additively, with its string
   mapping.
3. Port the PCM encoder and decoder, keeping the 20 ms LE-f32 framing so the PCM
   path behaves identically to Opus for pipeline and latency tests.
4. Wire the truthful `rate * channels * 32` bitrate into the catalog output,
   emitting the new catalog variant.
5. Keep the whole contribution small and additive so audio consumers that never
   select PCM pay nothing for it.

Adaptation notes: no dependencies added, PCM is raw passthrough. Errors adopt
moq-audio's `Error`. Frame the PR around the diagnostic value: a bit-exact
uncompressed path that isolates codec problems from transport problems, with the
catalog variant giving it real interop meaning. State plainly that if moq
declines, iroh-live keeps its own PCM codec.

### Adopt and remove (iroh-live)

Disposition upstream-ours (this supersedes the earlier keep-local verdict in the
codecs comparison, because the pcm leaf is marked required in the overview). The
paired iroh-live `up/pcm` branch deletes `rusty-codecs/src/codec/pcm/` (559 LOC)
once a moq release carries `Codec::Pcm` and the hang variant, per the PCM rows of
the codec-remove counterpart. Until that release is pinned, and in the event the
offer stalls or is declined, the local codec stays supported so the capability is
never lost, and the counterpart does not run. PCM decodes to CPU f32 and feeds no
zero-copy render input, so its removal regresses no held frame model and needs no
platform-atomic hold; the deletion is gated only on the upstream release.

## Tests

- Port the bit-exact round-trip assertion (`pcm/encoder.rs:213-233`) into a
  moq-audio round-trip test: encode a known f32 buffer, decode it, and assert the
  samples come back identical, which is the property that makes PCM useful for
  isolating codec-versus-transport problems.
- A catalog test asserting a PCM rendition advertises `AudioCodec::Pcm`, not
  `Unknown`.

No hardware gating; PCM is raw passthrough, so CI verifies the round-trip and
catalog tests fully. The iroh-live deletion is proven by the pipeline and latency
tests passing against the adopted PCM path before the local module is removed.

## Evidence

- ../comparison/codecs.md#6-pcm: the raw f32 20 ms framing, the 559
  dependency-free LOC, the absent hang catalog variant, and the analysis of
  record. The current decision is to offer it upstream and keep it locally only
  if the offer is declined; the section-10 verdict-table row records "keep local"
  as the fallback.
- ../comparison/audio.md#1-4-pcm-codec-ours-only: moq-audio's single-variant
  `Codec` enum and the `PcmEncoder`/`PcmAudioDecoder` pair, with the
  same-resample-path detail and the 559-line cost.
- DISPOSITION.md, PCM codec row: upstream-ours, `Codec::Pcm` in moq-audio plus
  the hang catalog variant in the same branch, superseding the earlier keep-local
  verdict because the pcm leaf is marked required in the overview.
- the removal ledger: the PCM row is kept-and-upstreamed; the local codec remains until
  a moq release carries `Codec::Pcm`, at which point the `up/pcm` pair branch
  adopts moq-audio's PCM and deletes the local module.

## Coordination

- Base plans: none. The contribution touches moq-audio and hang only and consumes
  nothing from the frozen base contract.
- Release gate: the paired iroh-live `up/pcm` deletion lands only against a
  pinned moq release carrying both `Codec::Pcm` and the hang `AudioCodec::Pcm`
  variant, per the upstream gating rule (coordination point 12). Because hang and
  moq-audio are both in the monorepo, the enum and catalog changes land together
  in one branch, so a single release satisfies the gate.
- Decline path: if moq declines the offer, the counterpart does not run and the
  local codec stays. The capability is required, so it is never dropped on the
  iroh-live side regardless of the upstream outcome.
- Semver: additive `#[non_exhaustive]` enum variants on both `Codec` and
  `AudioCodec`; no breaking change.
