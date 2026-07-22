# audio-device-unify. Playback sink and AEC engine into moq-audio behind features

> Campaign: upstream | Kind: leaf plan | Branch: up/audio-device |
> PR target: moq monorepo | Read ../0-overview.md first.
Depends on: no base plan. Prerequisite: the cpal git pin must resolve to a
crates.io release before any of this code can enter moq (details under
Adaptation notes).
Path: A (in-tree, feature-gated inside moq-audio)
Size: L

## Goal

Unify iroh-live's audio device layer into moq-audio behind non-default Cargo
features: a playback sink (device output with mixing, declicker fades,
per-stream volume, peak metering, device switching, and restart-with-backoff)
and the AEC engine (sonora-based acoustic echo cancellation wired between the
capture and playback callbacks). moq-audio today is capture-and-publish only:
it has microphone and macOS system-audio capture behind its non-default
`capture` feature, but no playback, sink, or output-stream code anywhere in the
crate (`comparisons/audio.md` section 3.4), and no echo cancellation, which
means a moq-audio application cannot build a full-duplex call without writing
its own device engine. Audio and video device I/O are the same problem for moq,
and the audio device layer is in scope for this campaign, not a separate effort
(0-overview.md, Goal and scope). The contribution keeps moq-audio's default and
relay-adjacent builds light: everything lands behind non-default `playback` and
`aec` features, matching how `capture` is already gated off by default.

## Evidence

- `comparisons/audio.md` section 3.1: the full inventory of our
  `audio_backend` (2,445 + 392 lines), with device management, stream
  lifecycle and recovery, config negotiation, playback with mixing, capture
  with fan-out, real-time-safe buffering, echo cancellation, and metering,
  each with file:line anchors into `moq-media/src/audio_backend.rs`.
- `comparisons/audio.md` section 3.4: verified by search that moq-audio
  contains no playback, sink, speaker, or output-stream code; the only match
  is a doc sentence in `format.rs` about speaker buffers crossing FFI
  (`3a3e0ea8:rs/moq-audio/src/format.rs:8`).
- `comparisons/audio.md` section 3.3: their capture path forwards real-time
  callback buffers over an unbounded tokio mpsc with a per-callback `Vec`
  allocation (`3a3e0ea8:rs/moq-audio/src/capture.rs:140,162,230`); ours uses
  preallocated lock-free fixed_resample ring buffers and bounded control
  channels, the discipline this plan carries upstream.
- `comparisons/audio.md` section 5: our `AudioSink::occupied_seconds` is the
  buffered-duration signal a playout clock steers by; their `decode::Consumer`
  has timestamps and `latency_max` but no pacing surface because it has no
  sink.
- `comparisons/audio.md` section 7, verdict "audio_backend + AEC: keep; no moq
  equivalent exists": the strongest asset of our audio stack and the candidate
  to upstream.

## moq API consumed

None of the frozen base contract; this leaf is independent of B1 through B4.
It builds on moq-audio's existing surface: the `Format` PCM vocabulary
(`rs/moq-audio/src/format.rs`), `decode::Consumer` as the natural sample
source for a playback stream, the `capture` module it integrates with for the
AEC path, and moq-audio's `Error` with additive variants (B5 conventions).
Timestamps at any public boundary are `moq_net::Timestamp`.

## Source to port

- `moq-media/src/audio_backend.rs` (2,445 LOC). The whole device engine:
  `AudioBackend` handle and the `AudioDriver` OS thread driven by a bounded
  `mpsc::channel(32)` (`audio_backend.rs:167-176`); device enumeration and
  `DeviceId` (`:109-131,147-165`); live device switching
  (`switch_devices_internal`, `:1720`); config negotiation preferring 48 kHz
  f32 (`negotiate_stream_config`, `:802`); the mixing output callback with
  per-stream volume, the 4-state declicker fade machine (constants `:66-76`),
  clamping, and peak recording (`output_callback`, `:942`); the input callback
  with channel mapping, in-place AEC, and fan-out to every registered
  producer (`InputCallbackState`, `:1094`); restart with exponential backoff
  on stream errors and excessive underruns (`:1381-1385`); and the
  fixed_resample ring-buffer endpoints (`create_output_channel`, `:1769`).
  `OutputStream` (`:316`) with `occupied_seconds` (`:416`) and `InputStream`
  (`:490`) are the caller-facing handles.
- `moq-media/src/audio_backend/aec.rs` (392 LOC). The AEC engine, two parts.
  `AecProcessor` (`aec.rs:67`) wraps `sonora::AudioProcessing` (the pure-Rust
  port of WebRTC audio processing; crates.io 0.1, resolving to the
  sonora-aec3/agc2/ns/common-audio/simd subcrates per our Cargo.lock) with
  echo cancellation plus high noise suppression configured at build
  (`aec.rs:83-104`) and a runtime enable flag. `AecState` (`aec.rs:201`)
  accumulates callback samples into 10 ms frames and serializes all
  processing on the input callback thread: the output callback writes its
  post-mix stereo render reference into a 100 ms lock-free SPSC ring buffer
  (`RENDER_REF_CAPACITY`, `audio_backend.rs:68`, wired at `:1467`), and the
  input callback drains it, processes the render frames, then the capture
  frames, with no Mutex contention between the two cpal callbacks
  (`aec.rs:1-13`). AEC toggles at runtime with no stream restart
  (`AudioBackend::set_aec_enabled`, `audio_backend.rs:182-190`).

Carries over: the driver-thread model, both callbacks, the resampling ring
buffers, the declicker, metering, device switching, restart backoff, and the
whole AEC engine. Dropped: our `AudioSink`/`AudioSource`/`AudioStreamFactory`
trait glue (`rusty-codecs/src/traits.rs:22-45,88-94`), since moq-audio
dispatches on concrete types, and our `anyhow` errors, replaced by moq-audio's
`Error`.

Explicitly not ported by this plan: the symphonia file playback source
(`moq-media/src/audio_file_symphonia.rs`, 368 LOC, plus the
`audio_file_source.rs` facade, 104 LOC). Open question: whether the
symphonia-based file source belongs in moq-audio as another feature-gated
source or stays in iroh-live; discussed here and in `comparisons/audio.md`
section 4. Current proposal: it stays in iroh-live for now. It is an app-level
source (decode a file to PCM and feed an encoder), not device I/O, and moq's
own file story is the moq-mux container importers, which republish compressed
bitstreams and serve a different need. If moq later wants a decoded-PCM file
source, it slots in as a `file` feature using the same feature discipline as
this plan.

## Target in moq

All in `rs/moq-audio`, additively:

- `rs/moq-audio/Cargo.toml`: two new non-default features.
  `playback = ["dep:cpal", "dep:fixed-resample", "dep:ringbuf", "dep:tokio", "dep:tracing"]`
  and `aec = ["playback", "capture", "dep:sonora"]` (AEC is meaningless
  without both directions, since it needs the render reference from the
  output callback and the microphone frames from the input callback). This
  follows the existing precedent exactly: `capture` is already a non-default
  feature whose comment explains it exists so audio-only consumers do not
  pull cpal and the ALSA build dependency. `fixed-resample` (crates.io 0.9),
  `ringbuf` (crates.io 0.4), and `sonora` (crates.io 0.1) are all
  registry dependencies; none is git-pinned.
- `rs/moq-audio/src/playback.rs` (new module, gated `feature = "playback"`):
  the device engine (driver thread, output stream with mixing and declicker,
  input fan-out, device enumeration and switching, restart backoff) and the
  public sink handle with `occupied_seconds` as the pacing signal.
- `rs/moq-audio/src/aec.rs` (new module, gated `feature = "aec"`): the ported
  `AecProcessor` and `AecState`, crate-private except for the enable toggle
  on the engine handle.
- `rs/moq-audio/src/lib.rs`: the two cfg-gated module declarations and
  re-exports.
- `rs/moq-audio/src/capture.rs`: the integration seam. When the `aec` feature
  is enabled and an engine exists, microphone frames route through the
  engine's input path (channel mapping, AEC, fan-out) instead of a private
  cpal stream, so `publish_capture` publishes echo-cancelled audio without
  changing its signature. Without `aec`, the existing capture path is
  untouched.

## Implementation steps

1. Land the cpal prerequisite: port against crates.io cpal 0.18 (moq's
   Cargo.lock already resolves cpal 0.18.1 for the `capture` feature) and
   verify every API our backend uses (`cpal::DeviceId` and its `FromStr`,
   host enumeration, device switching) exists in the release. No git
   dependency enters moq.
2. Add the `playback` feature and the ported device engine: driver thread,
   output stream, mixing, declicker, volume, metering, fixed_resample ring
   buffers, config negotiation. Public API kept small and `#[non_exhaustive]`
   where structs are exposed.
3. Add device switching and restart-with-backoff, ported as-is; these are the
   recovery behaviors a long-lived call needs and the part hardest to get
   right after the fact.
4. Add the `aec` feature: the sonora processor, the render-reference SPSC
   ring from the output callback, the 10 ms accumulation state on the input
   callback, and the runtime toggle.
5. Wire capture through the engine under `aec` (the fan-out input path), and
   while in that file, replace the unbounded realtime-to-async mpsc in the
   existing capture path with a bounded or preallocated channel
   (`comparisons/audio.md` section 3.3); this is a correctness fix for the
   real-time thread, not a style choice.
6. Document the pacing contract: `occupied_seconds` on the sink is the signal
   a playout clock steers by, closing the gap named in `comparisons/audio.md`
   section 5.

## Tests

- Hardware-free unit tests in moq's style: mixing math (two streams, volume,
  clamp), the declicker state machine across pause/resume, ring-buffer
  under/overflow status handling, and AEC frame accumulation with synthetic
  render and capture buffers (assert the processor consumes 10 ms frames and
  produces output; assert the disabled path drains without calling sonora).
- A cfg-gated device round-trip in the style of moq's hardware tests
  (coordination point 9): open the default output, play a short sine, assert
  the peak meter registers; open the default input and assert frames arrive.
  CI has no audio device, so the gate states plainly that CI verifies
  compilation only and the device test runs on developer hosts and the
  iroh-live device test runner.

## Adaptation notes

- The cpal pin, stated as the prerequisite it is. iroh-live pins cpal to a
  git rev (`moq-media/Cargo.toml:16`, rev `e172208e`), introduced when the
  device-id APIs we use were unreleased; the commit that added it (91dc0c4)
  replaced a local path with the then-current upstream master. The pinned rev
  self-identifies as version 0.18.0 in our Cargo.lock, and crates.io now
  carries 0.18.x (moq's Cargo.lock resolves cpal 0.18.1). A git dependency
  violates moq's crates.io-only rule (B5), so resolving the pin is a
  prerequisite, not a nice-to-have. Proposal: target crates.io cpal 0.18 and
  verify the release covers every API the ported code touches; if something
  the pinned rev has is missing from the release, upstream it to cpal and
  wait for the next cpal release rather than carry any pin into moq. The
  iroh-live side drops its pin in the counterpart branch. The optional
  `jack = ["cpal/jack"]` passthrough we carry is not ported.
- Errors: replace `anyhow` with moq-audio's `Error`, adding variants
  additively (device-not-found, stream-build, unsupported-config).
- Traits: our `AudioSink`/`AudioSource`/`AudioStreamFactory` object layer is
  iroh-live pipeline glue and does not move; moq-audio gets concrete
  `playback::Stream`-style types, consistent with its refusal of a codec
  trait until a second implementation forces one.
- Formats: the engine keeps its internal 48 kHz stereo processing rate;
  public boundaries speak moq-audio's `Format` vocabulary rather than our
  `AudioFormat`.
- Demand gating: their capture opens the device only while the track is used
  and releases it on unuse. The engine must not regress this; the AEC input
  path participates in the same used/unused lifecycle.
- Heavy dependencies (cpal, fixed-resample, ringbuf, sonora) all sit behind
  the non-default features per the dependency-weight constraint in
  0-overview.md; a default moq-audio build compiles none of them.

## Counterpart

The paired iroh-live `up/audio-device` branch (registry row in
`../branches.md`): once a moq-audio release carries `playback` and `aec`,
iroh-live switches its dependency, deletes
`moq-media/src/audio_backend.rs` and `moq-media/src/audio_backend/aec.rs`
(2,837 LOC), drops the cpal git pin and the sonora, fixed-resample, and
ringbuf dependencies from moq-media's Cargo.toml, and rewires the pipeline's
sink and source construction onto moq-audio's playback types. The symphonia
file source stays in iroh-live per the disposition above. The cut follows
`cut-plan.md` and coordination point 12.

## Coordination

- Coordination point 12: the iroh-live deletion happens only on the paired
  branch with the counterpart proof passing.
- Coordination point 9: CI cannot run device tests; the plan ships the
  cfg-gated round trip and says what CI does and does not verify.
- The cpal prerequisite above; do not open the moq PR while any git
  dependency remains in the ported code.
- Open question: the integration shape between the engine and
  `publish_capture` (engine-owned streams versus their per-publish stream
  open), discussed under Target in moq; current proposal: keep their
  demand-gated per-publish path as the default and route through the engine
  only when the `aec` feature is enabled and an engine is running.
- Open question: the symphonia file source disposition, discussed under
  Source to port; current proposal: stays in iroh-live.

## Acceptance checklist

- `cargo check -p moq-audio` with default features compiles no cpal,
  fixed-resample, ringbuf, or sonora; `--features playback` and
  `--features aec` each build and pass tests.
- No git dependency anywhere in the contribution; cpal comes from crates.io.
- Playback: mixing, declicker, volume, metering, device switching, and
  restart backoff ported and unit-tested; sink exposes `occupied_seconds`.
- AEC: render reference crosses via the lock-free SPSC ring, processing is
  serialized on the input callback thread, and the runtime toggle works
  without a stream restart.
- The existing capture path's unbounded realtime-to-async channel is replaced
  by a bounded or preallocated one.
- The cfg-gated device round-trip passes on a developer host with real audio
  hardware; the PR text states CI covers compilation only.
- The paired iroh-live branch builds against the moq branch with
  `audio_backend` and `aec.rs` deleted and the cpal pin gone.
