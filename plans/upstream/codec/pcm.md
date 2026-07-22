# pcm. Offer Codec::Pcm to moq-audio

Branch: moq-upstream/pcm          PR target: base branch, then moq main
Depends on: independent
Path: independent (in-tree offer)
Size: S

## Goal

Offer moq-audio an uncompressed PCM codec as a new `Codec::Pcm` variant on its
`#[non_exhaustive]` codec enum, porting our dependency-free PCM encoder and
decoder. The value is low and honest: PCM is a test and diagnostics codec that
lets the pipeline behave identically to Opus without compression or lookahead
confounding latency measurements. moq has no PCM codec, and, more decisively, the
hang catalog has no PCM codec variant, so a PCM track would ride an
`Unknown("pcm")` string with no interop meaning and no browser consumer. This
plan is deliberately small, states the low value plainly, and expects that moq
may decline it and keep PCM local. It is independent of the base API.

## Evidence

- `comparisons/moq-changes.md` change 11 and section 4 (PCM recommendation):
  slots into moq-audio's `#[non_exhaustive]` codec enum, but the hang catalog has
  no PCM variant, so interop value is nil and a decline is the expected outcome.
- `comparisons/codecs.md` section 6 (PCM): the raw f32 20-ms framing, the 559
  dependency-free LOC, and the "keep local; do not upstream" verdict, with the
  offer framed as costless.
- `comparisons/audio.md` section 1.4 (PCM codec, ours only): moq-audio's
  single-variant `Codec` enum and our `PcmEncoder`/`PcmAudioDecoder` pair.

## moq API consumed

None from the frozen base contract. This touches moq-audio only.

## Source to port

- `rusty-codecs/src/codec/pcm/encoder.rs` (234 LOC). Raw interleaved
  little-endian f32 in fixed 20-ms frames, framing deliberately matched to Opus
  (`FRAME_DURATION_MS = 20` at `pcm/encoder.rs:12`, LE-f32 packing described at
  `:15-16`, frame accumulation at `:46`), a truthful computed bitrate of
  `rate * channels * 32` (`:75`), and a bit-exact round-trip assertion
  (`:213-233`).
- `rusty-codecs/src/codec/pcm/decoder.rs` (310 LOC). Reuses the same
  resample-plus-remix path as Opus.

Together 559 dependency-free LOC serving test and diagnostics paths. Carries
over: the framing, the truthful bitrate, and the round-trip test. Dropped: our
`AudioEncoder`/`AudioDecoder` trait glue; the moq side dispatches by `match
config.codec` on the concrete structs.

## Target in moq

- `rs/moq-audio/src/encode/encoder.rs`: add a `Pcm` variant to the `Codec` enum
  (`encode/encoder.rs:24-42`), extend `Codec::as_str` to return `"pcm"`, and add
  a PCM arm to the `match config.codec` dispatch, keeping the concrete-struct
  shape moq chose over a trait.
- New PCM encode and decode paths in moq-audio, following the existing Opus
  module layout.
- The catalog output is the open question: moq-audio's `Encoder::catalog()`
  returns a `hang::catalog::AudioConfig`, and hang's `AudioCodec` is
  `{AAC, Opus, Mp2, Ac3, Ec3, Unknown}` with no PCM member, so a PCM rendition
  would have to advertise `Unknown("pcm")`. This likely needs a hang catalog PCM
  variant to carry real interop meaning; propose it, but expect the offer to be
  declined or kept behind a test-only feature.

## Implementation steps

1. Add `Codec::Pcm` and its `as_str` mapping, additively, on the
   `#[non_exhaustive]` enum.
2. Port the PCM encoder and decoder, keeping the 20-ms LE-f32 framing so the PCM
   path behaves identically to Opus for pipeline and latency tests.
3. Wire the truthful `rate * channels * 32` bitrate into the catalog output.
4. Handle the catalog gap honestly: either advertise `Unknown("pcm")` and
   document the lack of interop, or, if the maintainer is open to it, propose a
   hang `AudioCodec::Pcm` variant in the same conversation. Do not force the
   hang change; it is out of moq-audio's scope.
5. Keep the whole contribution small and gated so audio-only consumers do not pay
   for it if it lands.

## Tests

- Port the bit-exact round-trip assertion (`pcm/encoder.rs:213-233`) into a
  moq-audio round-trip test: encode a known f32 buffer, decode it, and assert the
  samples come back identical, which is the property that makes PCM useful for
  isolating codec-versus-transport problems.

## Adaptation notes

- No dependencies added: PCM is raw passthrough.
- Errors adopt moq-audio's `Error`.
- Frame this as low value in the PR. It earns its keep locally as a test and
  diagnostics codec; upstream, its worth is contingent on a hang catalog variant
  that does not exist, so a decline is a reasonable outcome and the PR should say
  so.

## Coordination

None from the coordination list. The only external dependency is a hang catalog
PCM variant, which is a separate project's decision and the likely reason to
decline; do not treat it as a blocker to keep the offer small.

## Acceptance checklist

- `Codec::Pcm` added additively with `as_str` and dispatch.
- PCM encode and decode ported with 20-ms LE-f32 framing.
- Truthful bitrate in the catalog output.
- Bit-exact round-trip test passes.
- The catalog interop gap and the low-value, likely-declined framing are stated
  plainly in the PR.
