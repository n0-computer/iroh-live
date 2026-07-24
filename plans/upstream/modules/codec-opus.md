# Opus Codec

VERDICT: apply improvements from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

The Opus encode and decode wrapper. Both stacks already encode Opus with the
same backend, `unsafe-libopus 0.2`, a pure-Rust c2rust transpilation of libopus
1.3.1 chosen on both sides for the same RUSTSEC-2026-0150 reason (no CMake, no C
linker hackery). Because the engines are identical, this is a merge into
moq-audio's existing wrapper rather than a new backend. iroh-live contributes the
control surface moq lacks (runtime `set_bitrate`, correct lookahead-derived
pre-skip, FEC and DTX ctl plumbing, and decoder-side channel remix), adopts and
leaves in place moq's rate snapping, validation, and variable frame durations,
then deletes the local wrapper. The verdict is apply-improvements because moq
already has a mergeable Opus wrapper on the same libopus base.

## iroh-live vs moq

Neither wrapper dominates; they are strong in complementary ways.

iroh-live's `OpusEncoder`/`OpusAudioDecoder` (`rusty-codecs/src/codec/opus/`,
804 LOC) is the more complete wrapper on control surface:

- Runtime bitrate change: `set_bitrate` re-issues the `OPUS_SET_BITRATE` ctl
  (`opus/encoder.rs:206-219`); moq applies bitrate at construction only
  (`3a3e0ea8:rs/moq-audio/src/encode/encoder.rs:182-188`), with no runtime path.
  The moq-audio-nvenc map records the contrast explicitly: video got
  `Session::reconfigure`, audio did not.
- Correct OpusHead pre-skip: built locally as 19 bytes per RFC 7845 5.1 with
  pre-skip queried from `OPUS_GET_LOOKAHEAD` (312 for VOIP at 48 kHz)
  (`opus/encoder.rs:91-108,222-237`). moq builds the OpusHead via
  `moq_mux::codec::opus::Config::encode`, which hardcodes pre-skip 0 and gain 0,
  and its parser ignores pre-skip on read
  (`3a3e0ea8:rs/moq-mux/src/codec/opus/mod.rs`). A spec-conforming decoder
  consuming moq's stream does not trim the encoder lookahead; one consuming
  iroh-live's does.
- FEC and DTX ctls wired but explicitly disabled with phase-3 TODOs
  (`opus/encoder.rs:76-83` FEC, `:84-88` DTX). moq touches neither; there is no
  `OPUS_SET_INBAND_FEC` anywhere in the crate.
- Decoder-side channel remix: mono-to-stereo, stereo-to-mono, and general N-to-M
  through a mono intermediate (`opus/decoder.rs:100-111`, `convert_channels_into`
  at `:136-186`). moq rejects remap: `channels: None` matches the codec, anything
  else errors "since remapping isn't implemented"
  (`3a3e0ea8:rs/moq-audio/src/decode/decoder.rs:16-46`).
- Integrated decoder resampling to the sink target rate inside `push_packet`
  (`opus/decoder.rs:61-62,98`); moq resamples externally at the `decode::Consumer`
  layer (`decode/consumer.rs:8-22,94-113`). The resampler convergence is owned by
  audio-resample.md; the remix helper travels with this module.

moq's `encode::Encoder`/`decode::Decoder` is the more complete wrapper on
codec-parameter flexibility and layering:

- Input rate snapped up to the nearest of 8/12/16/24/48 kHz via `pick_rate`, or
  pinned by `Config::sample_rate` (`opus.rs:13,20-22`), versus iroh-live's fixed
  48 kHz with the requested rate ignored (`opus/encoder.rs:15,160-164`).
- Configurable frame durations 2.5/5/10/20/40/60 ms, validated in
  `opus::frame_size` (`opus.rs:16,43-51`), default 20 ms, versus iroh-live's fixed
  20 ms `FRAME_SIZE = 960` (`opus/encoder.rs:16-17`).
- Channel validation to 1 or 2 in `opus::validate_channels` (`opus.rs:33-40`).
- Optional bitrate: `None` lets libopus pick (`encoder.rs:182-188`), versus
  iroh-live always setting it.
- Clean separation of layout (`Input`) from codec knobs (`Config`), and a
  one-shot `encode(&[f32])` that demands exactly `frame_size() * codec_channels()`
  samples or returns `Error::Misaligned` (`encoder.rs:237`), with accumulation
  living one layer up in `Producer::pending`. iroh-live's `push_samples` +
  `pop_packet` buffers internally and is friendlier to arbitrary-size sources.
- WebCodecs-shaped PCM format flexibility (U8/S16/S32/F32, interleaved and
  planar, with the zero-copy `as_interleaved_f32` `Cow` path, `format.rs:65-93`),
  versus iroh-live's interleaved f32 only.
- `catalog() -> hang::catalog::AudioConfig` returned directly (`encoder.rs:263`),
  which retires the iroh-live-side `config.rs` catalog mirror on the audio side
  rather than chasing hang's field renames by hand (hang is now 0.19.5; #2420
  renamed `displayRatio*` to `displayAspect*`, so the mirror no longer compiles).

The application modes differ and neither should be inherited hardcoded:
iroh-live uses `OPUS_APPLICATION_VOIP` (`opus/encoder.rs:58`), biasing libopus
toward speech intelligibility and loss robustness; moq uses
`OPUS_APPLICATION_AUDIO` (`encode/encoder.rs:177`), biasing toward music
fidelity. A merged wrapper makes application mode a config field.

The wire shape is already compatible. Both put the Opus config in the catalog
`description` as an OpusHead and ship one Opus packet per moq-lite group. moq
writes each packet `keyframe: true, duration: None` then `track.cut(None)`
(`3a3e0ea8:rs/moq-audio/src/encode/producer.rs:219-233`); iroh-live reaches the
same shape implicitly, every audio `EncodedFrame` carrying `is_keyframe: true`
(`opus/encoder.rs:144`) mapped onto the container producer's group cut in
`MoqPacketSink` (`moq-media/src/transport.rs:85-116`). The OpusHead pre-skip is
the only payload-level difference.

The Opus PLC and FEC story is open on both sides. Neither performs loss
concealment: both call `opus_decode_float(..., 0)` with real packet data only
(`opus/decoder.rs:80-89`, the literal `0, // no FEC` at `:87`;
`3a3e0ea8:rs/moq-audio/src/decode/decoder.rs:116-135`), and moq has no
`decode_lost` entry point. moq's producer comment "Opus PLC handles dropped
groups" (`encode/producer.rs:219-233`) is unbacked, because the consume side
never invokes PLC; on iroh-live the decode pipeline papers over gaps by pushing
raw silence into the sink (`moq-media/src/pipeline/audio_decode.rs:236-246`). A
merged wrapper needs a `decode_lost(duration)` entry point and an FEC request
flag that neither has today. The packet-per-group transport makes this more
valuable, not less. Concealment is the phase-3c design
(`plans/media-pipeline/phase-3c-fec.md`) and applies to both stacks.

## What to do

APPLY, then adopt, then remove. The moq-side merge is a Path A in-tree change
into moq-audio and moq-mux with no external-backend question, independent of the
base API, so it can slot early as relationship-building.

### Contribute upstream (moq-audio + moq-mux)

Source to port from iroh-live:

- `rusty-codecs/src/codec/opus/encoder.rs` (345 LOC): runtime `set_bitrate` via
  one `OPUS_SET_BITRATE` ctl (`opus/encoder.rs:206-215`); the FEC and DTX ctls,
  currently wired but explicitly disabled with phase-3 TODOs (`:80-82` FEC,
  `:84-87` DTX); the lookahead query `OPUS_GET_LOOKAHEAD` (`:91-105`) feeding the
  OpusHead pre-skip through `build_opus_head` (`:227-232`, pre-skip written at
  `:232`).
- `rusty-codecs/src/codec/opus/decoder.rs` (454 LOC): the channel remap,
  mono-to-stereo, stereo-to-mono, and general N-to-M through a mono intermediate
  (`opus/decoder.rs:100-111`, `convert_channels_into` at `:136-186`).

Carries over: the runtime bitrate ctl, the FEC/DTX ctl wiring (kept plumbed, not
enabled), the lookahead-to-pre-skip fix, and the remix helper. Dropped: the
`AudioEncoder`/`AudioDecoder` trait glue and the fixed-48-kHz, fixed-20-ms
assumptions, which moq's `pick_rate` and `frame_size` already generalize.

Targets in moq:

- `rs/moq-audio/src/encode/encoder.rs`: add a runtime `set_bitrate` method beside
  the construction-only bitrate application at `encoder.rs:182-191`, and make the
  encoder query `OPUS_GET_LOOKAHEAD` so its `catalog()` output at `encoder.rs:263`
  carries the real pre-skip. Add FEC and DTX ctl calls behind config fields,
  defaulting off, so the plumbing exists without changing default behavior.
- `rs/moq-mux/src/codec/opus/mod.rs`: the OpusHead pre-skip site. `Config`
  currently holds only `sample_rate` and `channel_count` (`opus/mod.rs:35-37`),
  `parse()` skips pre-skip with `buf.advance(2)` (`:57`), and `encode()` hardcodes
  pre-skip 0 (`:86`) and gain 0 (`:88`). Add a pre-skip field to `Config`,
  populate it on parse, and emit it on encode, so a conforming decoder trims the
  encoder lookahead. Keep the `encode_rejects_multichannel` guard (`:175`) intact.
  This is the one cross-crate touch and it is additive.
- `rs/moq-audio/src/decode/decoder.rs`: replace the `channels: None`-only policy
  that rejects remap "since remapping isn't implemented" (`decode/decoder.rs:16-46`)
  with the ported remix helper, or with an explicit resolved mono/stereo policy
  if upstream prefers to keep remap out of the decoder.
- `rs/moq-audio/src/opus.rs`: no change needed; its `RATES` (`opus.rs:12`),
  `FRAME_DURATIONS` (`:15`), `pick_rate` (`:19-21`), `validate_channels`
  (`:33-40`), and `frame_size` (`:43-51`) are the moq-side pieces adopted.

Ordered implementation steps:

1. Add runtime `set_bitrate` to moq-audio's `Encoder`, one `OPUS_SET_BITRATE`
   ctl. Small and self-standing; it is the precondition for later extending
   moq's video-side congestion rate control to audio.
2. Fix the pre-skip. Thread a pre-skip value from `OPUS_GET_LOOKAHEAD` at encode
   construction into the moq-mux OpusHead `Config`, replacing the hardcoded 0.
   Make `Config::parse` read the field so round-trips preserve it. A correctness
   fix independent of everything else and worth carrying on its own.
3. Add FEC and DTX config fields to the encoder, wire the ctls, default both off.
   This is groundwork: the encoder can request in-band FEC, but until a decoder
   PLC path exists it changes nothing observable. State this framing explicitly.
4. Reserve, but do not implement, a `decode_lost(duration)`-shaped PLC entry
   point on the decoder, and correct the unbacked moq PLC producer comment. Frame
   FEC and PLC as reserved surface for phase 3c, not a delivered feature.
5. Port the decoder-side channel remix, or land an explicit resolved policy for
   the mono/stereo mismatch that today errors, whichever upstream review accepts.
6. Decide the application mode explicitly. Make it a config field rather than
   inheriting either hardcoded choice (VOIP at `opus/encoder.rs:5,58`, AUDIO at
   `rs/moq-audio/src/encode/encoder.rs:177`).

Adaptation notes: no new codec, this merges into moq-audio's concrete
`Encoder`/`Decoder` structs, not a trait, matching its deliberate single-codec
shape. FEC and PLC are reserved API, not implemented concealment; be explicit in
docs and the PR so the reservation is not mistaken for a resilience feature. New
failure modes get additive variants on moq-audio's `#[non_exhaustive]` `Error`.

### Adopt and remove (iroh-live)

Disposition adopt-theirs, after `opus-improvements` lands. Once a moq release
carries the merged wrapper with runtime `set_bitrate`, the lookahead pre-skip
fix, the FEC/DTX ctl plumbing, and a channel-remap policy (ledger item D11), pin
that release and delete `rusty-codecs/src/codec/opus/` (804 LOC) on the paired
iroh-live `up/opus` branch. Per the proof-before-deletion rule, adopt moq-audio
Opus behind a feature flag, run both paths through the pipeline, flip the
default, then delete in a deletion-only commit. Opus is not a zero-copy path, so
its deletion is gated only on the upstream merge releasing, not on `render-adopt`.

## Tests

- A read-back test that `set_bitrate` takes effect at runtime, in the style of
  moq's openh264 `set_bitrate` regression tests.
- A pre-skip round-trip test: encode, read the catalog OpusHead, and assert
  pre-skip equals the queried lookahead and survives `Config::parse` (mirrors the
  iroh-live assertion at `opus/encoder.rs:323-327`).
- A remix test covering mono-to-stereo and stereo-to-mono, if the remix path
  lands.

CI verifies the merge is pure Rust with no hardware gating; libopus is the same
transpiled engine on both sides, so there is no on-hardware round-trip
requirement here. The iroh-live deletion is proven by
`moq-media/tests/pipeline_integration.rs` and the latency tests passing against
the adopted decoder before the module is removed.

## Evidence

- ../comparison/codecs.md#5-opus: the feature-by-feature table and the
  "cut and replace the wrapper with moq-audio, and upstream three things" verdict,
  plus the section 10 verdict-table row.
- ../comparison/audio.md#1-codec-layer: sections 1.1 (encoder table), 1.2
  (decoder table, PLC and remix rows), 1.3 (wire shape, pre-skip as the only
  payload difference), and 1.5 (merged-wrapper conclusion). Section 7's
  "Opus wrapper: merge" verdict.
- ../comparison/moq-changes.md change 10 and section 3 item 5: runtime
  `set_bitrate` as the precondition for extending video-style rate control to
  audio, the lookahead-derived pre-skip fix touching moq-mux and moq-audio, and
  FEC/PLC groundwork as an API reservation deferred to phase 3c.
- ../comparison/maps/moq-audio-nvenc.md: the moq-audio section, the Opus-only
  `#[non_exhaustive]` `Codec` enum, construction-only bitrate at
  `encode/encoder.rs:180-188`, the OpusHead built via
  `moq_mux::codec::opus::Config`, and the absent `decode_lost`.
- DISPOSITION.md, Opus wrapper row: adopt-theirs after `opus-improvements`; the
  wrapper shell is adopted from moq-audio, the control surface (runtime
  `set_bitrate`, lookahead pre-skip, FEC/DTX ctl plumbing, decoder remix) merges
  upstream first, and the local module is deleted after the merge releases.

## Coordination

- Base plans: none. This leaf is independent of B1 through B5; it carries no
  frame vocabulary and no encode `Packet` type.
- Release gate: the paired iroh-live `up/opus` deletion lands only against a
  pinned moq release containing the merged wrapper, per the upstream gating rule
  (coordination point 12). The deletion commit and the version bump travel
  together.
- Cross-module: the resampler convergence onto `moq_audio::Resampler` is
  audio-resample.md; the decoder-side channel remix helper is ported here, not
  there, so that when the resampler converges the remix is not lost. The playback
  sink and AEC that consume decoded PCM are audio-backend.md.
- The FEC/PLC decode path is the phase-3c work
  (`plans/media-pipeline/phase-3c-fec.md`); this module only reserves the API
  surface and corrects the unbacked moq PLC comment.
- Licensing and semver: additive `#[non_exhaustive]` enum variants and additive
  config fields only; no breaking change to moq-audio or moq-mux public API.
