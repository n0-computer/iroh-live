# Audio Backend (Playback Sink and AEC)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

The full-duplex audio device engine: a playback sink (device output, mixing,
declicker fades, per-stream volume, peak metering, device switching, and
restart-with-backoff) and the sonora acoustic echo cancellation engine wired
between the capture and playback callbacks. moq-audio today is capture-and-publish
only, so a moq-audio application cannot build a full-duplex call without writing
its own device engine. The verdict is move because audio and video device I/O are
the same problem for moq and this layer is in scope for the campaign; it unifies
into moq-audio behind non-default `playback` and `aec` features, mirroring
moq-audio's existing non-default `capture` gate. This is the strongest asset of
the iroh-live audio stack and it must not be cut before the unification lands.

## iroh-live vs moq

The two stacks are not comparable in scope. moq-audio capture is capture only;
iroh-live's `audio_backend` is a full-duplex engine.

iroh-live's `moq-media/src/audio_backend.rs` (2,445 LOC) plus
`audio_backend/aec.rs` (392 LOC), 2,837 LOC total, does everything below, none of
which has any moq counterpart:

- Device management: host selection, input/output enumeration (`available_hosts`,
  `list_inputs`, `list_outputs`, `audio_backend.rs:147-165`), device resolution
  with fallback-to-default (`AudioBackendOpts`, `:84-109`), and live device
  switching without recreating caller-side streams (`switch_input`/`switch_output`
  and `switch_devices_internal`, `:224-283,1720-1755`).
- Stream lifecycle and recovery: a dedicated `AudioDriver` OS thread owns cpal
  streams, driven by a bounded `mpsc::channel(32)` of `DriverMessage`
  (`:167-176,674`). Stream error callbacks and excessive underruns feed
  `attempt_restart` with exponential backoff, 500 ms doubling to a 4 s cap
  (`:1204-1206,1381-1419`).
- Config negotiation: `negotiate_stream_config` prefers 48 kHz, then 44.1 kHz,
  then highest, always f32 (`:802`).
- Playback with mixing: the output callback drains a command channel, mixes all
  registered `OutputStream` producers with per-stream volume, applies a 4-state
  declicker fade machine (playing/fade-out/paused/fade-in, 144 samples, roughly
  3 ms) on pause and resume, clamps the mix, and records peaks
  (`:65-76,942-1050`). Multiple decoded remote tracks mix into one device stream;
  moq-audio has nothing here.
- Capture with fan-out: the input callback channel-maps mono or N-channel devices
  to stereo, runs AEC in place, and distributes the processed buffer to every
  registered `InputStream` producer (`:1094-1168`), so one microphone feeds any
  number of encoders.
- Real-time-safe buffering: every stream endpoint is a
  `fixed_resample::resampling_channel` (output 50 ms latency, 3 s capacity; input
  30 ms, 1 s), with under/overflow autocorrection (`:1769-1818`). Callbacks are
  allocation-free and lock-free; commands cross via bounded SPSC channels.
- Echo cancellation: `audio_backend/aec.rs` wraps the `sonora` crate (a pure-Rust
  WebRTC audio-processing port). The output callback writes the post-mix render
  reference into a 100 ms lock-free SPSC ring buffer; the input callback drains it
  and runs render and capture 10 ms frames serialized on the input thread,
  avoiding cross-callback locking (`aec.rs:199-278`, `audio_backend.rs:68`). AEC
  toggles at runtime with no stream restart (`audio_backend.rs:180-189`).
- Metering and control: smoothed peak levels (`PeakState`, `:595-640`), per-stream
  volume, and the pause/resume/toggle surface exposed through `AudioSinkHandle`.

moq-audio capture (`3a3e0ea8:rs/moq-audio/src/capture.rs` plus submodules) does
several things iroh-live's engine lacks, which the capture-remove path adopts
separately and which this module reads onto its own buffers rather than
duplicating:

- System audio: `Source::System` is macOS desktop audio via ScreenCaptureKit with
  `setExcludesCurrentProcessAudio(true)` so playback does not loop back
  (`capture/screencapture.rs:81-98`). iroh-live has no system-audio capture.
- Device enumeration as data: `devices() -> Vec<Device { id, name, default }>`
  (`capture.rs:236-262`).
- `format(&Config)` without opening the device (`:99-116`), so the catalog can be
  registered before hardware is touched. iroh-live opens the stream to learn the
  negotiated format.
- Permission flow: AVFoundation TCC pre-check with the prompt bridged to a oneshot
  and a 30 s timeout (`capture/permission.rs:14-80`), plus a 5 s first-buffer
  timeout so a denied mic errors instead of hanging (`:50-54`).
- Demand gating: `encode::publish_capture` registers the catalog rendition up
  front, opens the device only while `track.used()`, and releases it on
  `track.unused()`, calling `reset_epoch` on resume so the idle gap lands in the
  PTS (`encode/capture.rs:13-100`). iroh-live's audio capture stream stays open
  once created.

The genuine deficiency on moq's side, beyond the missing playback and AEC, is
channel discipline. moq's microphone path forwards buffers from the real-time cpal
callback over an unbounded tokio mpsc channel, allocating a `Vec<f32>` per
callback (`3a3e0ea8:rs/moq-audio/src/capture.rs:140,162,230`). If the consumer
stalls, memory grows without bound, and the per-buffer allocation happens on the
real-time thread. iroh-live moves samples through preallocated lock-free
fixed_resample ring buffers with explicit push/read status handling
(`audio_backend.rs:430-455,511-540`), and every control channel is bounded. That
discipline is the one genuine parity port (P1 below).

The A/V sync touchpoint reinforces the asset. On iroh-live, audio playback is the
pacing master: `AudioSink::occupied_seconds` reports the buffered duration between
`push_samples` and the hardware callback (`rusty-codecs/src/traits.rs:38-45`,
`moq-media/src/audio_backend.rs:413-418`), which the decode loop uses for
speaker-lag stats and silence insertion and which the sync clock assumes as the
audio anchor while video waits (`moq-media/src/sync.rs:36-40`). moq's
`decode::Consumer` exposes per-frame timestamps
(`3a3e0ea8:rs/moq-audio/src/decode/consumer.rs:94-113`) and a `latency_max`
group-skip bound, but with no sink there is no buffered-duration signal and no
pacing point; a sync clock built on their consumer must own its own output
buffering. Unifying the sink upstream closes that gap.

## What to do

MOVE, then adopt-and-remove. A Path A in-tree, feature-gated contribution inside
moq-audio, independent of B1 through B4, size L. The cpal git pin is a hard
prerequisite (see below); do not open the moq PR while any git dependency remains
in the ported code.

### Contribute upstream (moq-audio)

Source to port from iroh-live:

- `moq-media/src/audio_backend.rs` (2,445 LOC), the whole device engine:
  `AudioBackend` handle and the `AudioDriver` OS thread driven by a bounded
  `mpsc::channel(32)` (`:167-176`); device enumeration and `DeviceId`
  (`:109-131,147-165`); live device switching (`switch_devices_internal`,
  `:1720`); config negotiation preferring 48 kHz f32 (`negotiate_stream_config`,
  `:802`); the mixing output callback with per-stream volume, the 4-state
  declicker fade machine (constants `:66-76`), clamping, and peak recording
  (`output_callback`, `:942`); the input callback with channel mapping, in-place
  AEC, and fan-out to every registered producer (`InputCallbackState`, `:1094`);
  restart with exponential backoff on stream errors and excessive underruns
  (`:1381-1385`); and the fixed_resample ring-buffer endpoints
  (`create_output_channel`, `:1769`). `OutputStream` (`:316`) with
  `occupied_seconds` (`:416`) and `InputStream` (`:490`) are the caller-facing
  handles.
- `moq-media/src/audio_backend/aec.rs` (392 LOC), the AEC engine in two parts.
  `AecProcessor` (`aec.rs:67`) wraps `sonora::AudioProcessing` (the pure-Rust
  WebRTC audio-processing port; crates.io 0.1, resolving to the
  sonora-aec3/agc2/ns/common-audio/simd subcrates per Cargo.lock) with echo
  cancellation plus high noise suppression configured at build (`aec.rs:83-104`)
  and a runtime enable flag. `AecState` (`aec.rs:201`) accumulates callback
  samples into 10 ms frames and serializes all processing on the input callback
  thread: the output callback writes its post-mix stereo render reference into a
  100 ms lock-free SPSC ring buffer (`RENDER_REF_CAPACITY`,
  `audio_backend.rs:68`, wired at `:1467`), and the input callback drains it,
  processes the render frames, then the capture frames, with no Mutex contention
  between the two cpal callbacks (`aec.rs:1-13`). AEC toggles at runtime with no
  stream restart (`AudioBackend::set_aec_enabled`, `audio_backend.rs:182-190`).

Carries over: the driver-thread model, both callbacks, the resampling ring
buffers, the declicker, metering, device switching, restart backoff, and the
whole AEC engine. Dropped: the `AudioSink`/`AudioSource`/`AudioStreamFactory`
trait glue (`rusty-codecs/src/traits.rs:22-45,88-94`), since moq-audio dispatches
on concrete types, and the `anyhow` errors, replaced by moq-audio's `Error`.

Targets in moq, all in `rs/moq-audio`, additively:

- `rs/moq-audio/Cargo.toml`: two new non-default features.
  `playback = ["dep:cpal", "dep:fixed-resample", "dep:ringbuf", "dep:tokio", "dep:tracing"]`
  and `aec = ["playback", "capture", "dep:sonora"]` (AEC is meaningless without
  both directions, since it needs the render reference from the output callback
  and the microphone frames from the input callback). This follows the existing
  precedent exactly: `capture` is already non-default so audio-only consumers do
  not pull cpal and, on Linux, the ALSA build dependency. `fixed-resample`
  (crates.io 0.9), `ringbuf` (crates.io 0.4), and `sonora` (crates.io 0.1) are
  all registry dependencies; none is git-pinned.
- `rs/moq-audio/src/playback.rs` (new module, gated `feature = "playback"`): the
  device engine (driver thread, output stream with mixing and declicker, input
  fan-out, device enumeration and switching, restart backoff) and the public sink
  handle with `occupied_seconds` as the pacing signal.
- `rs/moq-audio/src/aec.rs` (new module, gated `feature = "aec"`): the ported
  `AecProcessor` and `AecState`, crate-private except for the enable toggle on the
  engine handle.
- `rs/moq-audio/src/lib.rs`: the two cfg-gated module declarations and re-exports.
- `rs/moq-audio/src/capture.rs`: the integration seam. When `aec` is enabled and
  an engine exists, microphone frames route through the engine's input path
  (channel mapping, AEC, fan-out) instead of a private cpal stream, so
  `publish_capture` publishes echo-cancelled audio without changing its
  signature. Without `aec`, the existing capture path is untouched.

Ordered implementation steps:

1. Land the cpal prerequisite: port against crates.io cpal 0.18 (moq's Cargo.lock
   already resolves cpal 0.18.1 for the `capture` feature) and verify every API
   the backend uses (`cpal::DeviceId` and its `FromStr`, host enumeration, device
   switching) exists in the release. No git dependency enters moq.
2. Add the `playback` feature and the ported device engine: driver thread, output
   stream, mixing, declicker, volume, metering, fixed_resample ring buffers,
   config negotiation. Public API kept small and `#[non_exhaustive]` where structs
   are exposed.
3. Add device switching and restart-with-backoff, ported as-is; these are the
   recovery behaviors a long-lived call needs and the part hardest to get right
   after the fact.
4. Add the `aec` feature: the sonora processor, the render-reference SPSC ring
   from the output callback, the 10 ms accumulation state on the input callback,
   and the runtime toggle.
5. Wire capture through the engine under `aec` (the fan-out input path), and while
   in that file, replace the unbounded realtime-to-async mpsc in the existing
   capture path with a bounded or preallocated channel; this is a correctness fix
   for the real-time thread, not a style choice.
6. Document the pacing contract: `occupied_seconds` on the sink is the signal a
   playout clock steers by, closing the sync-touchpoint gap.

### The bounded-channel parity port (P1)

Adopting moq's capture surface (system audio, TCC prompt flow, `format()` without
open, demand gating) is right, but it must land on iroh-live's bounded buffers,
explicitly not moq's unbounded realtime-to-async mpsc. This is P1, the one genuine
parity port from the capture comparison, and the only place in the adopt-theirs
surface where iroh-live carries a robustness property moq lacks and no sibling
leaf owns the fix. moq-video already solves the same problem for video frames with
a bounded drop-oldest channel (`rs/moq-video/src/capture/channel.rs:19,43-51`,
DEPTH 4, oldest dropped to favor latency, pinned by `drops_oldest_when_full`), so
the port brings moq-audio capture to moq's own established discipline rather than
importing iroh-live's machinery wholesale.

Targets: `rs/moq-audio/src/capture.rs` at `:140` (the `UnboundedReceiver` field),
`:162` (`mpsc::unbounded_channel::<Vec<f32>>`), `:230` (the `forward` sender), and
`rs/moq-audio/src/capture/screencapture.rs:100` (the system-audio path's unbounded
channel). Steps: replace the microphone path's `mpsc::unbounded_channel::<Vec<f32>>`
with a bounded channel sized to roughly 500 ms of callback buffers, change
`forward` to `try_send` dropping the newest buffer on overflow and counting drops
(dropping beats blocking, since the sender is cpal's realtime thread and must
never park), apply the same change to the system-audio path, log the drop count
throttled in moq's tracing style, and keep the per-callback `Vec` allocation as is
for this PR (removing it needs a buffer pool out of proportion for a parity fix;
note the follow-up in the PR). The overflow policy is drop-newest with a counter
rather than moq-video's drop-oldest, because audio buffers are consumed in order
by an encoder and reordering the queue buys nothing; either policy satisfies
boundedness, so state the choice in the PR and defer to reviewer preference. Do
not import the `fixed_resample` ring-buffer machinery, which exists for the duplex
device engine and comes with this module's device port; P1 is the minimal
boundedness fix in moq's own idiom.

P1 shares `rs/moq-audio/src/capture.rs` with this device port, so it should land
first as a small self-contained goodwill PR; if the device port reaches that file
earlier, P1 folds into it.

### Adopt and remove (iroh-live)

The paired iroh-live `up/audio-device` branch: once a moq-audio release carries
`playback` and `aec`, iroh-live switches its dependency, deletes
`moq-media/src/audio_backend.rs` and `moq-media/src/audio_backend/aec.rs`
(2,837 LOC), drops the cpal git pin and the sonora, fixed-resample, and ringbuf
dependencies from moq-media's Cargo.toml, and rewires the pipeline's sink and
source construction onto moq-audio's playback types. Per the proof-before-deletion
rule, the deletion happens only on the paired branch with the counterpart proof
passing.

## Open questions

- The cpal git pin. iroh-live pins cpal to a git rev (`moq-media/Cargo.toml:16`,
  rev `e172208e`), introduced when the device-id APIs it uses were unreleased; the
  pinned rev self-identifies as version 0.18.0 in Cargo.lock, and crates.io now
  carries 0.18.x (moq's Cargo.lock resolves cpal 0.18.1). A git dependency
  violates moq's crates.io-only rule, so this is a prerequisite, not a
  nice-to-have. Current proposal: target crates.io cpal 0.18 and verify the
  release covers every API the ported code touches; if the pinned rev has
  something the release lacks, upstream it to cpal and wait for the next cpal
  release rather than carry any pin into moq. The iroh-live side drops its pin in
  the counterpart branch. The optional `jack = ["cpal/jack"]` passthrough is not
  ported.
- The symphonia file source (`moq-media/src/audio_file_symphonia.rs`, 368 LOC,
  plus the `audio_file_source.rs` facade, 104 LOC) is explicitly not ported by
  this module. Current proposal: it stays in iroh-live as an app-level source
  (decode a file to PCM and feed an encoder), not device I/O. moq's own file story
  is the moq-mux container importers, which republish compressed bitstreams and
  serve a different need. If moq later wants a decoded-PCM file source, it slots in
  as a `file` feature using the same feature discipline as this module.
- The integration shape between the engine and `publish_capture` (engine-owned
  streams versus their per-publish stream open). Current proposal: keep the
  demand-gated per-publish path as the default and route through the engine only
  when `aec` is enabled and an engine is running; the AEC input path participates
  in the same used/unused lifecycle so demand gating is not regressed.

## Tests

- Hardware-free unit tests in moq's style: mixing math (two streams, volume,
  clamp), the declicker state machine across pause/resume, ring-buffer
  under/overflow status handling, and AEC frame accumulation with synthetic render
  and capture buffers (assert the processor consumes 10 ms frames and produces
  output; assert the disabled path drains without calling sonora).
- A cfg-gated device round-trip in the style of moq's hardware tests: open the
  default output, play a short sine, assert the peak meter registers; open the
  default input and assert frames arrive.
- A unit test per P1 path asserting a full channel drops rather than grows: push
  more buffers than the capacity with no reader and assert the receiver yields at
  most the capacity, mirroring moq-video's `drops_oldest_when_full`
  (`rs/moq-video/src/capture/channel.rs:110`). No hardware gating on P1.

CI has no audio device, so the device round-trip gate states plainly that CI
verifies compilation only; the device test runs on developer hosts and the
iroh-live device test runner. The iroh-live deletion is gated on the paired branch
building against the moq branch with `audio_backend` and `aec.rs` deleted and the
cpal pin gone.

## Evidence

- ../comparison/audio.md#3-device-i-o: section 3.1 (the full `audio_backend`
  inventory with file:line anchors), 3.2 (moq's capture, done better), 3.3 (the
  channel-discipline difference and P1), 3.4 (the missing half, verified by search
  that moq-audio has no playback, sink, speaker, or output-stream code except one
  doc sentence in `format.rs:8`), section 5 (the `occupied_seconds` A/V sync
  anchor), and section 7's "audio_backend + AEC: keep, and upstream" and
  "Capture: adopt theirs" verdicts.
- ../comparison/capture.md#4-audio-capture: the side-by-side of the duplex engine
  versus the capture pipe, the verified absence of AEC or echo in moq-audio, and
  the section-5 "keep our AEC engine, adopt their capture surface" verdict.
- ../comparison/maps/moq-media.md: the moq-media inventory placing `audio_backend`
  and `aec.rs` and their dependencies (cpal git pin, fixed-resample, ringbuf,
  sonora).
- DISPOSITION.md, Audio engine row: upstream-ours, playback sink, mixing,
  declicker fades, metering, device switching, recovery, and sonora AEC with zero
  moq counterpart, unifying into moq-audio behind features. The Audio file sources
  row records the symphonia keep-local proposal.
- ../notes-unstructured/parity-ports.md: P1 as the one real port item (bounded capture channels), the
  playback-sink-and-AEC capability owned by this leaf, and the moq-audio capture
  surface as nothing-to-port beyond P1.

## Coordination

- Base plans: none. This leaf is independent of B1 through B4. It builds on
  moq-audio's existing `Format` PCM vocabulary, `decode::Consumer` as the natural
  sample source for a playback stream, the `capture` module for the AEC seam, and
  moq-audio's `Error` with additive variants (B5 conventions). Timestamps at any
  public boundary are `moq_net::Timestamp`.
- Release gate: the iroh-live deletion happens only on the paired
  `up/audio-device` branch with the counterpart proof passing (coordination
  point 12).
- CI hardware gating: coordination point 9, CI cannot run device tests; the leaf
  ships the cfg-gated round trip and states what CI does and does not verify.
- The cpal prerequisite is a hard gate: do not open the moq PR while any git
  dependency remains in the ported code.
- Shared file with P1: both this port and the parity port touch
  `rs/moq-audio/src/capture.rs`; P1 lands first or folds in.
- The capture surface adopted onto these buffers is the audio side of the
  capture-remove path; the video capture backends are the sibling capture modules.
  The decoded PCM this sink plays comes through codec-opus.md (and codec-pcm.md);
  the resampler at the device boundary is the `fixed_resample` channel, which is
  orthogonal to the `moq_audio::Resampler` convergence in audio-resample.md and
  stays with this engine because it must be lock-free in the cpal callback.
- Dependency weight: cpal, fixed-resample, ringbuf, and sonora all sit behind the
  non-default features per the dependency-weight constraint; a default moq-audio
  build compiles none of them.
