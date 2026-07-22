# pcm. Codec::Pcm in moq-audio plus the hang catalog variant, one branch

> Campaign: upstream | Kind: leaf plan | Branch: up/pcm |
> PR target: moq monorepo | Read ../0-overview.md first.
Depends on: independent of the base plans
Path: independent (in-tree)
Size: S

## Goal

Add an uncompressed PCM codec to moq-audio as a new `Codec::Pcm` variant on its
`#[non_exhaustive]` codec enum, porting our dependency-free PCM encoder and
decoder, and add the matching `AudioCodec::Pcm` variant to hang's catalog in the
same branch, so a PCM track advertises a first-class codec rather than an
`Unknown("pcm")` string. hang lives in the same moq monorepo, so the catalog
variant is a small additive change in the same PR series, not a separate ask.
PCM is a required capability for iroh-live: the uncompressed path lets the
pipeline behave identically to Opus without compression or lookahead
confounding latency measurements. If moq declines the offer, iroh-live keeps
`rusty-codecs/pcm` as a supported local codec, so the capability is never lost
either way.

## Evidence

- `../comparisons/moq-changes.md` change 11 and section 4 (PCM): slots into
  moq-audio's `#[non_exhaustive]` codec enum; the comparison notes the hang
  catalog has no PCM variant, which this plan closes.
- `../comparisons/codecs.md` section 6 (PCM): the raw f32 20-ms framing and the
  559 dependency-free LOC. The comparison's keep-local verdict is the analysis
  of record; the current decision is to offer it upstream and keep it locally
  only if the offer is declined.
- `../comparisons/audio.md` section 1.4 (PCM codec, ours only): moq-audio's
  single-variant `Codec` enum and our `PcmEncoder`/`PcmAudioDecoder` pair.

## moq API consumed

None from the frozen base contract. This touches moq-audio and hang only.

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
  (`encode/encoder.rs:24-42`), extend `Codec::as_str` to return `"pcm"`, and
  add a PCM arm to the `match config.codec` dispatch, keeping the
  concrete-struct shape moq chose over a trait.
- New PCM encode and decode paths in moq-audio, following the existing Opus
  module layout.
- hang's catalog: add an `AudioCodec::Pcm` variant beside
  `{AAC, Opus, Mp2, Ac3, Ec3, Unknown}`, in the same branch, so moq-audio's
  `Encoder::catalog()` can emit a first-class PCM `AudioConfig` instead of
  `Unknown("pcm")`.

## Implementation steps

1. Add `Codec::Pcm` and its `as_str` mapping, additively, on the
   `#[non_exhaustive]` enum.
2. Add hang's `AudioCodec::Pcm` catalog variant, additively, with its string
   mapping.
3. Port the PCM encoder and decoder, keeping the 20-ms LE-f32 framing so the
   PCM path behaves identically to Opus for pipeline and latency tests.
4. Wire the truthful `rate * channels * 32` bitrate into the catalog output,
   emitting the new catalog variant.
5. Keep the whole contribution small and additive so audio consumers that never
   select PCM pay nothing for it.

## Tests

- Port the bit-exact round-trip assertion (`pcm/encoder.rs:213-233`) into a
  moq-audio round-trip test: encode a known f32 buffer, decode it, and assert
  the samples come back identical, which is the property that makes PCM useful
  for isolating codec-versus-transport problems.
- A catalog test asserting a PCM rendition advertises `AudioCodec::Pcm`, not
  `Unknown`.

## Adaptation notes

- No dependencies added: PCM is raw passthrough.
- Errors adopt moq-audio's `Error`.
- Frame the PR around the diagnostic value: a bit-exact uncompressed path that
  isolates codec problems from transport problems, with the catalog variant
  giving it real interop meaning. State plainly that if moq declines, iroh-live
  keeps its own PCM codec.

## Counterpart

The paired iroh-live `up/pcm` branch deletes `rusty-codecs/src/codec/pcm/`
(559 LOC) once a moq release carries `Codec::Pcm` and the hang variant, per the
PCM rows of `../counterpart/codec-remove.md`. If the offer is declined, the
counterpart does not run and the local codec stays.

## Coordination

None from the coordination list. hang and moq-audio are both in the monorepo,
so the enum and catalog changes land together in one branch.

## Acceptance checklist

- `Codec::Pcm` added additively with `as_str` and dispatch.
- hang `AudioCodec::Pcm` catalog variant added in the same branch.
- PCM encode and decode ported with 20-ms LE-f32 framing.
- Truthful bitrate in the catalog output, advertised as `AudioCodec::Pcm`.
- Bit-exact round-trip and catalog tests pass.
