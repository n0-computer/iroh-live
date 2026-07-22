# opus-improvements. Merge Opus improvements into moq-audio

> Campaign: upstream | Kind: leaf plan | Branch: up/opus-improvements |
> PR target: base branch, then moq main | Read ../0-overview.md first.

Depends on: independent of the base (no frame vocabulary, no PTS change)
Path: independent (merge into moq-audio's existing in-tree Opus)
Size: S-M

## Goal

Improve moq-audio's existing Opus wrapper rather than adding a codec. Both stacks
already encode Opus with the same backend, `unsafe-libopus 0.2`, chosen on both
sides for the same RUSTSEC-2026-0150 reason, so the engines are identical and
this is a merge, not a new backend. Contribute from our wrapper the control
surface moq lacks: runtime `set_bitrate`, correct lookahead-derived pre-skip
(moq hardcodes the OpusHead pre-skip to 0), FEC and DTX ctl plumbing, and
decoder-side channel remix. Adopt from moq, and leave in place, its rate
snapping, validation, and variable frame durations. Frame the FEC and PLC work
as groundwork and API reservation, not a delivered feature, because neither side
conceals loss today. This is a Path A in-tree merge into moq-audio with no
external-backend question, and it is independent of the base API, so it can slot
early as relationship-building.

## Evidence

- `comparisons/moq-changes.md` change 10 and section 3 item 5: runtime
  `set_bitrate` as the precondition for extending video-style congestion rate
  control to audio, lookahead-derived pre-skip as a genuine correctness fix
  touching moq-mux's opus `Config` and moq-audio, and FEC/PLC groundwork as an
  API reservation with the implementation deferred to phase 3c.
- `comparisons/codecs.md` section 5 (Opus): the feature-by-feature table and the
  "upstream three things" verdict.
- `comparisons/audio.md` section 1 (codec layer): the encoder and decoder
  feature tables, the pre-skip and application-mode quality differences, and the
  merged-wrapper conclusion in section 1.5.
- `comparisons/maps/moq-audio-nvenc.md` moq-audio section: the Opus-only
  `#[non_exhaustive]` `Codec` enum, the construction-only bitrate, the OpusHead
  built via `moq_mux::codec::opus::Config`, and the absent `decode_lost`.

## moq API consumed

None from the frozen base contract. This plan touches moq-audio and moq-mux
only, which carry no frame vocabulary and no encode `Packet` type.

## Source to port

- `rusty-codecs/src/codec/opus/encoder.rs` (345 LOC). Runtime `set_bitrate` via
  one `OPUS_SET_BITRATE` ctl (`opus/encoder.rs:206-215`); the FEC and DTX ctls,
  currently wired but explicitly disabled with phase-3 TODOs
  (`:80-82` FEC, `:84-87` DTX); the lookahead query `OPUS_GET_LOOKAHEAD`
  (`:91-105`) feeding the OpusHead pre-skip through `build_opus_head`
  (`:227-232`, pre-skip written at `:232`).
- `rusty-codecs/src/codec/opus/decoder.rs` (454 LOC). The channel remap:
  mono-to-stereo, stereo-to-mono, and general N-to-M through a mono intermediate
  (`opus/decoder.rs:100-111`, `convert_channels_into` at `:136-186`).

Carries over: the runtime bitrate ctl, the FEC/DTX ctl wiring (kept plumbed, not
enabled), the lookahead-to-pre-skip fix, and the remix helper. Dropped: our
`AudioEncoder`/`AudioDecoder` trait glue and our fixed-48-kHz, fixed-20-ms
assumptions, which moq's `pick_rate` and `frame_size` already generalize.

## Target in moq

- `rs/moq-audio/src/encode/encoder.rs`: add a runtime `set_bitrate` method
  beside the construction-only bitrate application at `encoder.rs:182-191`, and
  make the encoder query `OPUS_GET_LOOKAHEAD` so its `catalog()` output at
  `encoder.rs:263` carries the real pre-skip. Add FEC and DTX ctl calls behind
  config fields, defaulting off, so the plumbing exists without changing default
  behavior.
- `rs/moq-mux/src/codec/opus/mod.rs`: the OpusHead pre-skip site. `Config`
  currently holds only `sample_rate` and `channel_count` (`opus/mod.rs:35-37`),
  `parse()` skips pre-skip with `buf.advance(2)` (`:57`), and `encode()`
  hardcodes pre-skip 0 (`:86`) and gain 0 (`:88`). Add a pre-skip field to
  `Config`, populate it on parse, and emit it on encode, so a conforming decoder
  trims the encoder lookahead. Keep the `encode_rejects_multichannel` guard
  (`:175`) intact.
- `rs/moq-audio/src/decode/decoder.rs`: replace the `channels: None`-only policy
  that rejects remap "since remapping isn't implemented" (`decode/decoder.rs:16-46`)
  with the ported remix helper, or with an explicit resolved mono/stereo policy
  if upstream prefers to keep remap out of the decoder.
- `rs/moq-audio/src/opus.rs`: no change needed; its `RATES` (`opus.rs:12`),
  `FRAME_DURATIONS` (`:15`), `pick_rate` (`:19-21`), `validate_channels`
  (`:33-40`), and `frame_size` (`:43-51`) are the moq-side pieces we adopt.

## Implementation steps

1. Add runtime `set_bitrate` to moq-audio's `Encoder`, one `OPUS_SET_BITRATE`
   ctl. This is small and is the precondition for later extending moq's
   video-side congestion rate control to audio, so it stands on its own value.
2. Fix the pre-skip. Thread a pre-skip value from `OPUS_GET_LOOKAHEAD` at encode
   construction into the moq-mux OpusHead `Config`, replacing the hardcoded 0.
   This is a correctness fix independent of everything else and worth carrying on
   its own; make `Config::parse` read the field so round-trips preserve it.
3. Add FEC and DTX config fields to the encoder, wire the ctls, default both off.
   This is groundwork: the encoder can request in-band FEC, but until a decoder
   PLC path exists it changes nothing observable. State this framing explicitly.
4. Reserve, but do not implement, a `decode_lost(duration)`-shaped PLC entry
   point on the decoder. Neither side conceals loss today; moq's producer comment
   that "Opus PLC handles dropped groups" (`encode/producer.rs:219-233`) is
   unbacked, because the decoder only ever calls `opus_decode_float(..., 0)` with
   real data and has no null-data path. Frame the FEC and PLC as reserved surface
   for phase 3c, not a delivered feature.
5. Port the decoder-side channel remix, or land an explicit resolved policy for
   the mono/stereo mismatch that today errors, whichever upstream review accepts.
6. Decide the application mode explicitly. Ours selects
   `OPUS_APPLICATION_VOIP` (imported at `opus/encoder.rs:5`, passed to
   `opus_encoder_create` at `opus/encoder.rs:58`), theirs `OPUS_APPLICATION_AUDIO`
   (`rs/moq-audio/src/encode/encoder.rs:177`); make it a config field rather than
   inheriting either hardcoded choice.

## Tests

- A read-back test that `set_bitrate` takes effect at runtime, in the style of
  moq's openh264 `set_bitrate` regression tests.
- A pre-skip round-trip test: encode, read the catalog OpusHead, and assert
  pre-skip equals the queried lookahead and survives `Config::parse` (mirrors our
  assertion at `opus/encoder.rs:323-327`).
- A remix test covering mono-to-stereo and stereo-to-mono, if the remix path
  lands.

## Adaptation notes

- No new codec: this merges into the existing concrete `Encoder`/`Decoder`
  structs, not a trait, matching moq-audio's deliberate single-codec shape.
- FEC/PLC are reserved API, not implemented concealment; be explicit in docs and
  the PR so the reservation is not mistaken for a resilience feature.
- Errors adopt moq-audio's `Error`; new failure modes get additive variants on
  the `#[non_exhaustive]` enum.

## Coordination

None from the coordination list. This is independent of the base and of the
other leaves. The only cross-crate touch is the moq-mux opus `Config` pre-skip
field, which is additive.

## Acceptance checklist

- Runtime `set_bitrate` on moq-audio's `Encoder`, with a read-back test.
- OpusHead pre-skip populated from `OPUS_GET_LOOKAHEAD`, threaded through the
  moq-mux `Config`, and round-trip tested.
- FEC and DTX ctls plumbed behind config fields, defaulting off, documented as
  groundwork.
- A reserved `decode_lost`-shaped PLC entry point or an explicit note that it is
  deferred to phase 3c, with the unbacked moq PLC comment corrected.
- Channel remix or an explicit mono/stereo policy landed.
- Application mode is a config field.
