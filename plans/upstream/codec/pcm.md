# pcm. Offer Codec::Pcm to moq-audio

Branch: moq-upstream/pcm          PR target: base branch, then moq main
Depends on: independent
Path: independent (in-tree offer)
Size: S

## Goal

Offer moq-audio an uncompressed PCM codec as a new `Codec::Pcm` variant on its
`#[non_exhaustive]` codec enum, porting our dependency-free PCM encoder and
decoder, and propose the matching hang catalog PCM variant so a PCM track carries
real interop meaning. PCM is a required capability for iroh-live (overview
revision 3): the uncompressed path lets the pipeline behave identically to Opus
without compression or lookahead confounding latency measurements, and iroh-live
depends on having it. This plan contributes `Codec::Pcm` to moq-audio together
with a hang catalog PCM variant, so a PCM track advertises a first-class codec
rather than an `Unknown("pcm")` string with no interop meaning and no browser
consumer. The hang catalog variant is a genuine ask on a separate project, and
the plan is honest that moq may not take the whole offer; the load-bearing point
is that the capability is never lost. If moq declines the upstream, iroh-live
keeps `rusty-codecs/pcm` as a supported local codec. This plan is independent of
the base API.

## Evidence

- `comparisons/moq-changes.md` change 11 and section 4 (PCM recommendation):
  slots into moq-audio's `#[non_exhaustive]` codec enum; the comparison notes the
  hang catalog has no PCM variant, which this plan addresses by proposing one so
  the track carries interop meaning. Per overview revision 3 the comparison's
  low-value framing is superseded: PCM is a required iroh-live capability.
- `comparisons/codecs.md` section 6 (PCM): the raw f32 20-ms framing and the 559
  dependency-free LOC. The comparison's "keep local; do not upstream" verdict is
  the analysis of record, and overview revision 3 is the decision that overrides
  it: the offer stands, and PCM is kept locally if the offer is declined.
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
- The catalog output is the substantive ask: moq-audio's `Encoder::catalog()`
  returns a `hang::catalog::AudioConfig`, and hang's `AudioCodec` is
  `{AAC, Opus, Mp2, Ac3, Ec3, Unknown}` with no PCM member, so absent a new
  variant a PCM rendition would advertise `Unknown("pcm")`. Propose a hang catalog
  PCM variant so the track carries real interop meaning rather than an opaque
  string. This is a genuine request on a separate project, so it may not land as
  asked; if it does not, the `Codec::Pcm` offer can still ship advertising
  `Unknown("pcm")`, and if moq declines the whole offer, iroh-live keeps PCM
  locally.

## Implementation steps

1. Add `Codec::Pcm` and its `as_str` mapping, additively, on the
   `#[non_exhaustive]` enum.
2. Port the PCM encoder and decoder, keeping the 20-ms LE-f32 framing so the PCM
   path behaves identically to Opus for pipeline and latency tests.
3. Wire the truthful `rate * channels * 32` bitrate into the catalog output.
4. Close the catalog gap: propose a hang `AudioCodec::Pcm` variant so the
   rendition advertises a first-class codec, and lead with that as the interop
   ask. It lives in a separate project and is outside moq-audio's scope, so do not
   force it; if it cannot land in this conversation, advertise `Unknown("pcm")`
   and document the interim, and keep pursuing the hang variant.
5. Keep the whole contribution small and gated so audio-only consumers do not pay
   for it if it lands. If moq declines the offer entirely, iroh-live keeps
   `rusty-codecs/pcm` so the capability is retained.

## Tests

- Port the bit-exact round-trip assertion (`pcm/encoder.rs:213-233`) into a
  moq-audio round-trip test: encode a known f32 buffer, decode it, and assert the
  samples come back identical, which is the property that makes PCM useful for
  isolating codec-versus-transport problems.

## Adaptation notes

- No dependencies added: PCM is raw passthrough.
- Errors adopt moq-audio's `Error`.
- Frame this in the PR as a required iroh-live capability (overview revision 3),
  not a costless nicety. The substantive ask is the hang catalog PCM variant that
  gives the track interop meaning; be honest that it lives in a separate project
  and may not land as asked. State plainly that if moq declines the offer,
  iroh-live keeps its own PCM codec, so the capability is not lost either way.

## Coordination

None from the coordination list. The one external ask is a hang catalog PCM
variant, which is a separate project's decision; lead with it as the interop
value, but do not treat it as a blocker to shipping the moq-audio offer. If moq
declines the offer, iroh-live keeps `rusty-codecs/pcm` locally
(`plans/align-to-moq/tasks/codec-remove.md`), so the capability is retained
regardless.

## Acceptance checklist

- `Codec::Pcm` added additively with `as_str` and dispatch.
- PCM encode and decode ported with 20-ms LE-f32 framing.
- Truthful bitrate in the catalog output.
- Bit-exact round-trip test passes.
- The hang catalog PCM variant is proposed as the interop ask, and the PR states
  plainly that PCM is a required iroh-live capability kept locally if the offer is
  declined (overview revision 3).
