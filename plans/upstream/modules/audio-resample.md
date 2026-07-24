# Audio Resampler

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

The codec-facing sample-rate resampler that runs inside the audio decoders to
convert decoded PCM to a target rate. Both stacks wrap rubato's sinc async
resampler. The verdict is use-moq because moq-audio's `Resampler` is the leaner
implementation (sinc-128 with preallocated planar scratch and explicit
partial-input handling) where iroh-live's allocates per call, and the crate
already ships on moq main. iroh-live converges on moq's `Resampler` and deletes
its own; the channel remix helper that iroh-live's resampler pairs with is not
lost, because it is ported through codec-opus.md rather than with the resampler.

## iroh-live vs moq

iroh-live has three rubato instances. The codec-facing one is
`rusty_codecs::processing::resample::Resampler` (rubato `Async::new_sinc`, sinc_len
256, oversampling 256, f_cutoff 0.95, linear interpolation, BlackmanHarris2, chunk
1024, `FixedAsync::Input`, max ratio 1.1, identity passthrough when rates match),
used inside the Opus and PCM decoders
(`rusty-codecs/src/processing/resample.rs:19-60`), 123 LOC. The device boundary
does not use rubato at all: `audio_backend` uses `fixed_resample::resampling_channel`,
a real-time-safe combined ring buffer and resampler with drift autocorrection
(`moq-media/src/audio_backend.rs:1769-1818`). The symphonia file importer carries a
third rubato instance with the same 256/256 sinc parameters
(`moq-media/src/audio_file_symphonia.rs:266-284`).

moq has one type: `moq_audio::Resampler` wrapping rubato `Async::new_sinc` with
sinc_len 128, oversampling 128, f_cutoff `Some(0.95)`, linear interpolation,
BlackmanHarris2, max ratio 1.0, caller-chosen `chunk_frames`, and explicit
pending-sample buffering with preallocated planar scratch buffers reused across
calls (`3a3e0ea8:rs/moq-audio/src/resample.rs:14-70` and the `process` body). It is
sample-rate only; channel remix is rejected upstream by both the encoder and
decoder configs, and there is no channel-mixing code anywhere in the crate.

Comparison: iroh-live's sinc_len 256 is the higher-quality and roughly 2x more
expensive filter; moq's deliberately trims to 128 with rubato default features off.
moq handles partial input explicitly via its `pending` buffer and reuses planar
scratch across calls; iroh-live hands arbitrary-length interleaved input straight
to rubato's adapter API and allocates the output `Vec` per call, plus a full input
copy on the passthrough path (`resample.rs:47-50`). moq is the leaner, lower-alloc
implementation and wins for a shared crate. The real split is channel handling:
iroh-live remixes at the decoder (see codec-opus.md), moq refuses. For a shared
crate, moq's leaner wrapper plus iroh-live's remix helper cover both needs. The
device-boundary `fixed_resample` channel is not replaceable by either rubato
wrapper because it must be lock-free in the cpal callback; it is orthogonal and
stays with the audio-backend engine.

## What to do

USE-MOQ. Nothing to port upstream first: `moq_audio::Resampler` is already on moq
main and is the target. This is a convergence-and-delete on the iroh-live side,
with the remix helper carried separately.

### Adopt and remove (iroh-live)

Disposition adopt-theirs. Converge iroh-live's codec-facing resampling onto
`moq_audio::Resampler` and delete `rusty-codecs/src/processing/resample.rs`
(123 LOC). The decoders that used the local resampler consume moq's instead, once
the Opus wrapper adoption (codec-opus.md) and the PCM adoption (codec-pcm.md) have
moved decoding onto moq-audio, since moq applies resampling at its own
`decode::Consumer`/decoder layer rather than inside a separate helper. The channel
remix helper that iroh-live's resampler paired with is ported through
codec-opus.md (into moq-audio's decoder or as an explicit mono/stereo policy), not
through this module, so converging the resampler loses no capability.

The removal is part of the codec-remove counterpart's processing row: the resample
half converges on `moq_audio::Resampler`, already on main, and the remix helper
stays (travels via `opus-improvements`). Per the proof-before-deletion rule, the
decode path must pass an end-to-end test against moq's resampler before the local
module is deleted.

Explicitly not touched: the device-boundary `fixed_resample::resampling_channel`
in `audio_backend` and the symphonia importer's rubato instance. The former is
lock-free device machinery owned by audio-backend.md; the latter travels with the
symphonia file source, whose disposition is an open question in audio-backend.md
(current proposal: stays in iroh-live).

## Tests

The decode-path tests (`moq-media/tests/pipeline_integration.rs` and the latency
tests) must pass with moq's `Resampler` in place before
`rusty-codecs/src/processing/resample.rs` is deleted. No hardware gating; rubato is
pure Rust, so CI verifies the convergence fully. There is no separate upstream test
obligation here because moq's resampler already ships with its own coverage.

## Evidence

- ../comparison/audio.md#2-resampling: the three iroh-live rubato instances versus
  moq's single leaner type, the sinc-256-versus-128 quality-and-cost trade, moq's
  preallocated scratch and pending-buffer partial-input handling against
  iroh-live's per-call allocation, the channel-remix split, and the conclusion
  that moq's wrapper plus iroh-live's remix helper covers both needs. Section 7's
  "Resampler: converge on theirs, keep our remix" verdict.
- the codec removal sequencing, resample row: the resample half converges on
  `moq_audio::Resampler` (already on main); the remix helper stays.
- DISPOSITION.md, Audio resampler row: adopt-theirs, converges on
  `moq_audio::Resampler` (leaner, preallocated scratch), with the channel remix
  helper ported via `opus-improvements`, not lost.

## Coordination

- Base plans: none. The resampler is pure sample-rate conversion with no frame
  vocabulary or PTS involvement.
- Release gate: no upstream contribution to release, so the iroh-live convergence
  is gated only on the moq release bump that also carries the adopted Opus and PCM
  decoders (codec-opus.md, codec-pcm.md), because the local resampler is only
  reachable through those decoders. The deletion is part of the codec-remove
  processing row and lands with that convergence.
- Cross-module: the channel remix helper is ported through codec-opus.md, so this
  module must not be read as dropping remix. The device-boundary `fixed_resample`
  channel stays with audio-backend.md. The symphonia importer's rubato instance is
  covered by the symphonia file-source open question in audio-backend.md.
- Semver: no moq API change; iroh-live-local deletion only.
