# Map: moq main — moq-audio + moq-nvenc (HEAD 3a3e0ea8, 2026-07-21)

SOURCE: moq main, HEAD 3a3e0ea8 (2026-07-21); dev merged into main.

Repo: `/home/bit/Code/rust/moq`. All citations reference blobs at
`3a3e0ea8`. Read via `git show 3a3e0ea8:<path>`.

Provenance: this map was first written on the pre-merge dev branch at SHA 261c2048.
Since the merge, `git diff 261c2048..main -- rs/moq-audio/src rs/moq-nvenc/src` shows
only **two trivial one-line changes**, both in moq-audio, neither structural:

- `encode/producer.rs:136` — `catalog.timeline_section(&name)` became
  `catalog.timeline(&name).section()`, following moq-mux #2420's shareable-timeline
  API (the old `timeline_section` helper was replaced by
  `catalog::Producer::timeline(name) -> timeline::Producer` with `.section()`; see the
  moq-mux map). Same catalog field is written; the rendition still advertises its
  per-track timeline.
- `resample.rs:39` — `f_cutoff: 0.95` became `f_cutoff: Some(0.95)`, a rubato API bump
  (the sinc cutoff field is now `Option`).

**moq-nvenc source is byte-identical since 261c2048** (zero diff). moq-audio is still
version 0.0.9, moq-nvenc still 0.0.1. Everything below is exact against main.

Layout history that produced the current shape (still current):

- `b9c539c0` — `refactor(audio)!: align the moq-audio capture/encode surface with moq-video (#2350)`. This produced the current layout: it deleted the flat `codec.rs` (534 lines) and split it into `encode/{encoder,producer,capture}.rs`, `decode/{decoder,consumer}.rs`, and `opus.rs`.
- `9090a68b` — `feat(capture): macOS window/app/system-audio sources and device enumeration (#2293)`: added `capture/screencapture.rs` (system audio) and `capture::devices`.
- `3479ecc2` — `fix(moq-video, moq-audio): threading/correctness fixes + dedicated capture encode thread (#2038)`. The moq-audio part: "await the mic-permission prompt via oneshot + timeout instead of blocking a tokio worker with recv_timeout(30s)". The "dedicated capture encode thread" part is moq-video only.
- `dbc589f4` — `refactor(moq-video): vendor NVENC fork in-tree as moq-nvenc (#2042)`; `5fa30c72` — `feat(moq-video): NVDEC hardware decode, zero-copy NVDEC -> NVENC transcode (#2145)`.

---

## moq-audio

Crate: `rs/moq-audio`, version 0.0.9, edition 2024 (`rs/moq-audio/Cargo.toml:8-10`).
Files: `capture.rs`, `capture/permission.rs`, `capture/screencapture.rs`,
`decode/{consumer,decoder,mod}.rs`, `encode/{capture,encoder,producer,mod}.rs`,
`error.rs`, `format.rs`, `frame.rs`, `lib.rs`, `opus.rs`, `resample.rs`,
`tests/roundtrip.rs`.

### Module shape (lib.rs)

`rs/moq-audio/src/lib.rs:32-45`:

```rust
mod error;
mod format;
mod frame;
mod opus;
mod resample;

#[cfg(feature = "capture")]
pub mod capture;
pub mod decode;
pub mod encode;

pub use error::Error;
pub use format::Format;
pub use frame::Frame;
pub use resample::Resampler;
```

The crate doc (`lib.rs:1-30`) states the design explicitly: "Counterpart to
`moq-video` for audio tracks, and shaped the same way. Sits on top of
`moq_mux` and `hang` ... a Rust-native Opus implementation that turns raw PCM
into the bitstreams `moq_mux::codec::opus` already knows how to ingest."
Layering per module doc: `capture` (feature-gated device I/O) → `encode`
(PCM → Opus → `moq_mux::container` + hang catalog) → `decode` (mirror).
"PCM layout lives on the producer / consumer via `encode::Input` /
`decode::Config`, not on each frame, so callers can't drift between calls"
(`lib.rs:28-30`).

### Dependencies / codec backend

`rs/moq-audio/Cargo.toml`:

- Opus via **`unsafe-libopus = "0.2"`** — a pure-Rust c2rust transpilation of libopus 1.3.1. Rationale in a long comment (`Cargo.toml:56-63`): "No CMake toolchain, no C linker hackery ... unlike the `opus`/`audiopus_sys` combo which is stuck on a 5-year-old vendored libopus that breaks under CMake 4 (RUSTSEC-2026-0150). Trade-off: ~20% slower than the C version".
- `cpal = "0.18"` (optional, `capture` feature) for mic; `rubato = "3.0"` (default-features off, sinc only) for resampling; `hang`, `moq-mux`, `moq-net` workspace deps; `tokio` optional (capture loop only).
- macOS-only, capture-gated objc2 stack: `objc2-av-foundation` (TCC mic permission pre-check), `objc2-screen-capture-kit` + `objc2-core-media` + `objc2-core-audio-types` (system audio), `block2`, `dispatch2` (`Cargo.toml:71-81`).
- The `capture` feature is **off by default** "so audio-only consumers (e.g. moq-ffi) don't pull cpal and, on Linux, the ALSA build dependency" (`Cargo.toml:19-24` comment).

### Codec coverage

**Opus only**, but with an explicit growth path. `encode/encoder.rs:24-42`:

```rust
/// Output audio codec. `#[non_exhaustive]` so new codecs can be added without
/// breaking external `match`es.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	/// Opus (RFC 6716). The only codec today, and the default.
	#[default]
	Opus,
}

impl Codec {
	/// Canonical lowercase identifier, matching the WebCodecs / RFC catalog
	/// string. Used as the wire/FFI codec name everywhere.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Opus => "opus",
		}
	}
}
```

`Codec` also implements `Display` and `FromStr` (`encode/encoder.rs:44-61`).
The module doc rejects a trait for now: "Single-codec implementation today ...
When AAC or other codecs land we'll factor out a backend dispatch behind
`Codec`; introducing a trait now would be premature"
(`encode/encoder.rs:1-7`). No codec trait exists anywhere in the crate — the
encoder/decoder are concrete structs with `match config.codec` dispatch
(`encode/encoder.rs:152-155`).

`opus.rs` centralizes libopus constraints shared by both sides
(`opus.rs:1-5`): `RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000]`
(`opus.rs:13`), `FRAME_DURATIONS` = 2.5/5/10/20/40/60 ms (`opus.rs:16`),
`pick_rate` (snap up, fallback 48 kHz, `opus.rs:20-22`), `validate_rate`,
`validate_channels` (1 or 2 only, `opus.rs:33-40`), `frame_size(sample_rate,
duration)` (`opus.rs:43-51`). All `pub(crate)`.

### Frame and Format (WebCodecs-shaped)

`rs/moq-audio/src/frame.rs:4-18` (complete file body):

```rust
/// One unit of raw PCM crossing the codec boundary: what
/// [`encode::Producer::write`](crate::encode::Producer::write) takes and what
/// [`decode::Consumer::read`](crate::decode::Consumer::read) returns.
///
/// Just a payload and a presentation timestamp. PCM layout (format / sample rate
/// / channel count) is fixed by the producer or consumer at construction time,
/// never per frame, so callers can't accidentally drift the format mid-stream.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp of the first sample.
	pub timestamp: Timestamp,
	/// The samples, in the layout the producer or consumer was built with.
	pub data: Bytes,
}
```

(`Timestamp` is `moq_net::Timestamp`, microsecond-resolution.)

`rs/moq-audio/src/format.rs:5-35` — explicitly mirrors WebCodecs
`AudioData.format`, named `Format`:

```rust
/// Raw PCM sample format.
///
/// Mirrors the WebCodecs `AudioData.format` enum so callers can pass
/// microphone or speaker buffers across the FFI boundary unchanged.
///
/// Interleaved variants pack samples as `[c0_s0, c1_s0, c0_s1, c1_s1, ...]`.
/// Planar variants pack as `[c0_s0, c0_s1, ..., c1_s0, c1_s1, ...]`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
	/// Interleaved unsigned 8-bit, silence at 128.
	U8,
	/// Interleaved signed 16-bit little-endian.
	S16,
	/// Interleaved signed 32-bit little-endian.
	S32,
	/// Interleaved 32-bit float in `[-1.0, 1.0]`. The default: libopus's native
	/// layout, so it needs no conversion.
	#[default]
	F32,
	U8Planar,
	S16Planar,
	S32Planar,
	F32Planar,
}
```

Methods: `bytes_per_sample` (`format.rs:39`), `is_planar` (`format.rs:48`),
`is_float` (`format.rs:56`), `as_interleaved_f32(&[u8], channels) ->
Cow<[f32]>` with a zero-copy fast path when input is aligned interleaved F32
(`format.rs:65-93`, uses `align_to::<f32>`), and the reverse
`from_interleaved_f32(&[f32], channels) -> Vec<u8>` (`format.rs:160`).

### Encoder (encode/encoder.rs + opus.rs)

Input-layout struct (`encode/encoder.rs:62-91`):

```rust
/// The PCM layout of the buffers handed to [`Encoder::encode`] /
/// [`Producer::write`](super::Producer::write).
#[derive(Clone, Debug)]
pub struct Input {
	/// How samples are packed in each buffer.
	pub format: Format,
	/// Samples per second per channel. Resampled to the codec rate if they differ.
	pub sample_rate: u32,
	/// Channels per frame.
	pub channels: u32,
}

impl Default for Input {
	fn default() -> Self {
		Self { format: Format::F32, sample_rate: 48_000, channels: 2 }
	}
}
```

Config (`encode/encoder.rs:90-127`):

```rust
/// Encoder configuration: the input PCM layout plus the codec knobs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The PCM layout fed to the encoder.
	pub input: Input,
	/// Output codec. Defaults to [`Codec::Opus`].
	pub codec: Codec,
	/// Sample rate the codec runs at. `None` snaps [`Input::sample_rate`] up to
	/// the nearest rate the codec supports, resampling if that moved it.
	pub sample_rate: Option<u32>,
	/// Channel count the codec runs at. `None` matches [`Input::channels`];
	/// anything else is rejected, since remapping isn't implemented.
	pub channels: Option<u32>,
	/// Bitrate in bits per second. `None` lets the codec pick.
	pub bitrate: Option<u32>,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	pub frame_duration: Duration,
}
```

`Config::new(input)` fills defaults, frame_duration 20 ms
(`encode/encoder.rs:115-126`).

The encoder itself (`encode/encoder.rs:129-160`):

```rust
/// Audio encoder over the PCM layout declared in [`Config::input`].
pub struct Encoder {
	inner: *mut OpusEncoder,
	config: Config,
	/// Resolved codec sample rate (from `config.sample_rate`, else the input rate
	/// snapped up to a supported one).
	codec_rate: u32,
	/// Resolved codec channel count (currently always the input's).
	codec_channels: u32,
	frame_size: usize,
	scratch: Vec<u8>,
}

// SAFETY: OpusEncoder is heap-allocated state owned exclusively by this
// struct; libopus encoder methods take a single &mut, so a unique
// owner is allowed to move it across threads.
unsafe impl Send for Encoder {}
```

API: `new(&Config)` (`:152`), `config()` (`:204`), `codec()` (`:210`),
`codec_rate()` (`:216`), `codec_channels()` (`:222`), `frame_size()` (`:228`),
`encode(&mut self, pcm: &[f32]) -> Result<Bytes, Error>` (`:237` — one frame
of interleaved f32 at exactly `frame_size() * codec_channels()` samples, else
`Error::Misaligned`), and `catalog() -> hang::catalog::AudioConfig` (`:263`)
which builds a hang catalog entry with an OpusHead description
(`moq_mux::codec::opus::Config { sample_rate, channel_count }.encode()`),
bitrate, and `Container::Legacy`. `MAX_PACKET_BYTES: usize = 4_000` scratch
per RFC 6716 §3.4 (`encode/encoder.rs:22`). `Drop` calls
`opus_encoder_destroy`. Bitrate is applied at construction via
`opus_encoder_ctl_impl(inner, OPUS_SET_BITRATE_REQUEST, ...)`
(`encode/encoder.rs:180-188`) — **no runtime bitrate change** on the audio
side (unlike moq-nvenc's `Session::reconfigure`).

### Producer (encode/producer.rs)

`Options` — the layout-agnostic counterpart of `Config`
(`encode/producer.rs:15-56`):

```rust
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
	/// Track name to publish under. `None` derives a unique one from the codec
	/// (`0.opus`, then `1.opus`, ...), matching how the video side names its
	/// track. Subscribers find it through the catalog either way.
	pub track: Option<String>,
	/// Output codec. Defaults to [`Codec::Opus`].
	pub codec: Codec,
	pub sample_rate: Option<u32>,
	pub channels: Option<u32>,
	pub bitrate: Option<u32>,
	pub frame_duration: Duration,
}
```

`Producer` (`encode/producer.rs:71-96`):

```rust
/// Encode raw PCM and publish it as a moq-mux audio track.
///
/// The catalog rendition is registered at construction (not on first write), so
/// a subscriber that opens the catalog before any frames arrive still sees the
/// track.
pub struct Producer<E: CatalogExt = ()> {
	encoder: Encoder,
	resampler: Option<Resampler>,
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire>,
	track_name: String,
	catalog: moq_mux::catalog::Producer<E>,
	pending: Vec<f32>,
	/// Samples emitted since the current epoch (reset by [`reset_epoch`](Self::reset_epoch)).
	frames_produced: u64,
	/// Wall-clock anchor in microseconds, taken from the first frame after each
	/// (re)start. Emitted PTS = `epoch + frames_produced / codec_rate`. `None`
	/// until the first write so the next frame re-anchors to its timestamp.
	epoch_us: Option<u64>,
}
```

API: `new(&mut moq_net::broadcast::Producer, moq_mux::catalog::Producer<E>,
Input, &Options)` (`:98`) — creates the Encoder, an optional `Resampler` if
input rate ≠ codec rate, creates the track (named or
`moq_mux::import::unique_track` derived from codec), wraps it in
`catalog.media_producer(track, moq_mux::container::legacy::Wire)`, inserts
the rendition into `catalog.lock().audio` including a per-track timeline
(`config.timeline = Some(catalog.timeline(&name).section())` at
`producer.rs:136` — the shareable-timeline API from moq-mux #2420; previously
`catalog.timeline_section(&name)`). Other methods: `track_name()` (`:152`),
`track() -> &moq_net::track::Producer` (`:158`, for `used()`/`unused()` demand
watching), `reset_epoch()` (`:168` — re-anchor PTS after idle gap), `write(&Frame)`
(`:185` — format-convert, resample, accumulate `pending`, drain full Opus frames,
PTS = epoch + running sample count), `finish()` (`:237` — zero-pads the trailing
partial frame, finalizes the track), `abort(err)` (`:252`). `Drop` removes the
catalog rendition (`:258-262`). Publishing detail (`:219-233`): "Each audio packet is
its own moq-lite group, matching `moq_mux::codec::opus::Import`. Opus PLC handles
dropped groups" — `keyframe: true`, `duration: None`, followed by `track.cut(None)`.

### Decoder (decode/decoder.rs)

`Config` (`decode/decoder.rs:16-46`):

```rust
/// Decoder configuration: the PCM layout to emit, plus the subscription's
/// latency budget.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// How to pack samples in each emitted frame.
	pub format: Format,
	/// Sample rate to emit at. `None` uses the codec's native rate from the
	/// catalog; anything else resamples.
	pub sample_rate: Option<u32>,
	/// Channel count to emit. `None` uses the codec's native count; anything
	/// else is rejected, since remapping isn't implemented.
	pub channels: Option<u32>,
	/// Upper bound on buffering before skipping a stalled group.
	///
	/// Forwarded to [`moq_mux::container::Consumer::with_latency`]: ...
	/// The `_max` suffix is a reminder that we never *add* latency here ...
	/// A companion `latency_min` for jitter-buffer padding will land in a follow-up.
	pub latency_max: Option<Duration>,
}
```

`Decoder` (`decode/decoder.rs:56-70`):

```rust
/// Decodes codec packets into interleaved `f32` PCM.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the packets don't come from a plain track subscription.
pub struct Decoder {
	inner: *mut OpusDecoder,
	sample_rate: u32,
	channel_count: u32,
	max_frame_size: usize,
}

// SAFETY: see Encoder.
unsafe impl Send for Decoder {}
```

API: `new(&hang::catalog::AudioConfig)` (`:75`) — parses the OpusHead
`description` via `moq_mux::codec::opus::Config::parse`, falling back to the
catalog's declared rate/channels; `sample_rate()` (`:107`),
`channel_count()` (`:112`), `decode(&mut self, packet: &[u8]) ->
Result<Vec<f32>, Error>` (`:117`) — allocates for the 120 ms max Opus packet
(`MAX_FRAME_MS = 120`, `:14`) and truncates to the decoded sample count.
Notably **no PLC/FEC entry point**: `opus_decode_float(..., 0)` is always
called with real packet data; there is no `decode_lost()`.

### Consumer (decode/consumer.rs)

`decode/consumer.rs:8-22`:

```rust
/// Subscribe to a moq-mux audio track and emit decoded PCM in the layout
/// declared by [`Config`].
///
/// The mirror of [`encode::Producer`](crate::encode::Producer): output format /
/// sample rate / channel count are fixed at construction, and
/// [`read`](Self::read) returns plain [`Frame`]s.
pub struct Consumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	resampler: Option<Resampler>,
	config: Config,
	resolved_sample_rate: u32,
	resolved_channels: u32,
}
```

`Consumer::new(&moq_net::broadcast::Consumer, &hang::catalog::AudioConfig,
name, Config)` is async (`decode/consumer.rs:27-75`): builds the Decoder,
optional Resampler, subscribes with
`Subscription::default().with_priority(hang::catalog::PRIORITY.audio)`, and
applies `config.latency_max` via `Consumer::with_latency`. `read()`
(`:94-113`) decodes a mux frame, resamples if configured, converts with
`Format::from_interleaved_f32`, and returns `Frame { timestamp:
mux_frame.timestamp, data }`. Accessors: `config()` (`:77`), `sample_rate()`
(`:83`), `channels()` (`:89`).

### Capture (capture.rs, capture/permission.rs, capture/screencapture.rs)

Public capture surface (`capture.rs`):

```rust
// capture.rs:26-48
/// Where the audio comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// An audio input device, by the id [`devices`] reports. `None` opens the
	/// system default input.
	Microphone(Option<String>),

	/// System (desktop) audio: everything the machine is playing, minus this
	/// process. macOS only, and it needs the Screen Recording permission, since
	/// that's the API Apple exposes it through.
	System,
}

// capture.rs:58-72
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// What to capture.
	pub source: Source,
	/// Samples per second to ask the device for. `None` takes its default.
	pub sample_rate: Option<u32>,
	/// Channels to ask the device for. `None` takes its default.
	pub channels: Option<u32>,
}
```

Device enumeration (`capture.rs:236-262`):

```rust
/// An audio input reported by [`devices`].
#[derive(Clone, Debug)]
pub struct Device {
	/// Opaque identifier: pass to [`Source::Microphone`].
	pub id: String,
	/// Human-readable name, e.g. "MacBook Pro Microphone".
	pub name: String,
	/// Whether this is the system default input.
	pub default: bool,
}

impl Device {
	/// The [`Source`] that captures this device.
	pub fn source(&self) -> Source { Source::Microphone(Some(self.id.clone())) }
}

pub async fn devices() -> Result<Vec<Device>, Error> { ... }
```

Internals are deliberately `pub(crate)`: "`encode::publish_capture` is the
entry point, so the per-source backends stay an implementation detail"
(`capture.rs:74-77`). `Stream` enum dispatches `Microphone(Microphone)` vs
`System(screencapture::SystemAudio)` (`capture.rs:78-96`) with a single
`async fn read(&mut self) -> Option<Vec<f32>>`. `format(&Config)`
(`capture.rs:99-116`) reports the capture format **without opening the
device**, so the catalog can be populated up front. The mic path
(`Microphone::open`, `capture.rs:151-215`): cpal stream built inline (cpal
`Stream` is `!Send`), realtime callback converts F32/I16/U16 to f32 and
forwards over an **unbounded** tokio mpsc channel, first buffer awaited with
`FIRST_BUFFER_TIMEOUT` = 5 s so a TCC-denied mic errors instead of hanging
(`capture.rs:50-54`). Blocking cpal host I/O goes through `spawn_blocking`
(`capture.rs:266-275`).

`capture/permission.rs` (macOS): `ensure_microphone_access()` queries
AVFoundation TCC status and, if `NotDetermined`, triggers the system prompt
bridged to a oneshot with `PROMPT_TIMEOUT` = 30 s
(`capture/permission.rs:14-80`) — the #2034/#2038 "blocking mic prompt" fix
(await via oneshot + timeout instead of parking a tokio worker). No-op on
other platforms.

`capture/screencapture.rs` (379 lines, macOS only): `SystemAudio` captures
desktop audio via ScreenCaptureKit, since "macOS has no 'loopback' input
device" (`screencapture.rs:1-13`). SCK "has no audio-only mode", so the video
side is pinned to a 2x2 frame at 1 fps and dropped
(`screencapture.rs:38-41`). Config: `setCapturesAudio(true)`,
`setExcludesCurrentProcessAudio(true)` (no feedback loop), display-wide
`SCContentFilter` (`screencapture.rs:81-98`). Buffers arrive on a dispatch
queue and forward over an mpsc channel; first-buffer timeout surfaces a
missing Screen Recording grant. Defaults 48 kHz stereo; SCK resamples to
whatever is requested (`screencapture.rs:34-36,60-67`).

### Turnkey publish (encode/capture.rs)

`encode/capture.rs:23-30`:

```rust
pub async fn publish_capture(
	mut broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	capture: capture::Config,
	encode: Options,
	clock: moq_mux::Clock,
) -> Result<(), Error> {
```

Semantics (`encode/capture.rs:13-27` doc + `capture_loop`, `:52-100`):
the catalog rendition is registered up front from `capture::format` (no
device open); the device opens **only while a subscriber is listening**
(`track.used().await`) and is released when the last one leaves
(`tokio::select! { biased; res = track.unused() => ..., samples =
input.read() => ... }`). On resume it calls `producer.reset_epoch()` so the
idle gap lands in the PTS, "keeping audio aligned with a wall-clock video
track". Frames are stamped from the shared `moq_mux::Clock`. Everything is
cancel-safe: dropping the future drops the cpal/SCK stream. It is an async
loop on the runtime — no dedicated encode thread on the audio side (Opus
encode of 20 ms frames is cheap; contrast moq-video's dedicated capture
encode thread from #2038).

### Error and resampler

`error.rs:1-55` — single crate-wide `#[non_exhaustive] enum Error`:
`Unsupported(String)`, `Device(String)`, `Capture(String)`, `Misaligned {
got, expected }`, `ResamplerConstruction(rubato::...)`,
`Resample(rubato::...)`, `Hang(hang::Error)`, `Mux(moq_mux::Error)`,
`Net(moq_net::Error)`, `TimeOverflow(moq_net::TimeOverflow)` — each variant
documented with actionable recovery guidance.

`resample.rs:14-70` — `pub struct Resampler` wraps rubato's
`Async::<f32>::new_sinc` (`FixedAsync::Input`, sinc_len 128, `f_cutoff:
Some(0.95)`, BlackmanHarris2) behind an interleaved-f32 `process(&mut self,
&[f32]) -> Result<Vec<f32>>` API with internal chunk buffering; sample-rate
only, channel remap rejected upstream. `new(input_rate, output_rate,
channels, chunk_frames)` (`resample.rs:32`).

### Comparison hooks vs iroh-live

For the audio comparison against iroh-live's `rusty-codecs` opus +
`moq-media` audio_backend: moq-audio's split is Encoder/Decoder as
**sans-I/O concrete structs** (sync, `&mut self`, `Bytes`/`Vec<f32>` in/out,
`unsafe impl Send`, no trait), with Producer/Consumer as the transport-bound
layer owning resample + format conversion + PTS bookkeeping, and
publish_capture as demand-driven turnkey glue. Latency control is a single
`latency_max: Option<Duration>` forwarded to the container consumer (no
jitter buffer; `latency_min` explicitly deferred). Timestamps are wall-clock
epoch + sample-count, with explicit `reset_epoch` gap semantics. The API type
names (`Producer`, `Consumer`, `Encoder`, `Decoder`, `Config`, `Options`,
`publish_capture`) mirror moq-video one-for-one, per #2350.

---

## moq-nvenc

Crate: `rs/moq-nvenc`, version 0.0.1, edition 2021, MSRV 1.85, MIT
(`rs/moq-nvenc/Cargo.toml:1-14`). Vendored by `dbc589f4`
`refactor(moq-video): vendor NVENC fork in-tree as moq-nvenc (#2042)`.
Source byte-identical since 261c2048.

### What it is

Per `README.md:1-27`: "Safe-ish Rust bindings for the NVIDIA Video Codec SDK
(NVENC + NVDEC), vendored for the MoQ workspace ... a fork of
`nvidia-video-codec-sdk` (MIT, Copyright Viliam Vadocz), trimmed to a single
mode: it always dlopens the driver libraries at runtime (`libnvidia-encode`
for NVENC, `libnvcuvid` for NVDEC) rather than linking them." Consequences:
links on GPU-less builders, starts on driverless hosts (falls back), no CUDA
toolkit needed at build time, and "compiles on any platform, macOS included
... elsewhere it is a compile-only stub."

Layout: `lib.rs` (re-exports `safe::*`; `#![allow(clippy::all,
clippy::pedantic, rustdoc::all)]` to keep vendored code out of workspace lint
churn, `lib.rs:30-34`), `safe/` (encode wrapper), `cuvid.rs` (NVDEC function
table), `sys/` (bindgen output).

### Dependencies (Cargo.toml:16-24)

```toml
[dependencies]
# Only cudarc's `driver` API is used, always dlopen'd at runtime:
# `fallback-dynamic-loading` loads libcuda, and `cuda-12020` pins the CUDA API
# version so the build needs no CUDA toolkit or driver present.
cudarc = { version = "0.19", default-features = false, features = ["driver", "fallback-dynamic-loading", "cuda-12020"] }
lazy_static = "1.5.0"
# dlopen libnvidia-encode at runtime (the two NVENC bootstrap symbols). Pinned to
# 0.8 (0.9 requires rustc 1.88, above our MSRV).
libloading = "0.8"
```

**No build.rs, no bindgen at build time.** The `sys` bindings are
pre-generated and committed: `sys/headers/{nvEncodeAPI.h, nvcuvid.h,
cuviddec.h}` are the vendored headers, `sys/linux_sys/` and
`sys/windows_sys/` the bindgen outputs (`nvEncodeAPI.rs` alone is 11,708
lines). `sys/mod.rs:10-25` selects: non-Windows targets reuse the "linux"
bindings ("plain C-ABI type/enum/fn-pointer definitions, so the 'linux'
bindings compile on any non-Windows target. On macOS the crate only needs to
compile"). `sys/guid.rs` and `sys/version.rs` supply codec/preset GUIDs and
`NVENCAPI_VERSION`.

### Safe API surface (safe/mod.rs:6-18)

```rust
mod api;
mod buffer;
mod builders;
mod encoder;
mod result;
mod session;

pub use api::{EncodeAPI, ENCODE_API};
pub use buffer::{Bitstream, BitstreamLock, Buffer, BufferLock, EncoderInput, EncoderOutput, RegisteredResource};
pub use encoder::{Encoder, EncoderInitParams};
pub use result::{EncodeError, ErrorKind};
pub use session::{CodecPictureParams, EncodePictureParams, Session};
```

**safe/api.rs** — `ENCODE_API: EncodeAPI` lazy static (`api.rs:16-23`): a
struct of ~40 raw function pointers mirroring `NV_ENCODE_API_FUNCTION_LIST`
(`open_encode_session_ex`, `initialize_encoder`, `reconfigure_encoder`,
`create_input_buffer`, `create_bitstream_buffer`, `encode_picture`,
`lock_bitstream`, `map_input_resource`, `register_resource`,
`set_io_cuda_streams`, etc., `api.rs:78-160+`), populated by dlopening
`libnvidia-encode` and calling the two NVENC bootstrap symbols. Note the
asymmetry called out in `cuvid.rs:8-10`: the encode-side lazy static
**panics** when the driver is absent, while the decode-side `cuvid::Api::get`
returns a `Result` — which is why moq-video probes `driver_libs_present()`
before touching either (`rs/moq-video/src/encode/backend/nvenc.rs:72-82`).

**safe/encoder.rs** — the entrypoint (`encoder.rs:38-44`):

```rust
#[derive(Debug)]
pub struct Encoder {
	pub(crate) ptr: *mut c_void,
	// Used to fetch the device pointer for an externally allocated buffer
	pub(crate) ctx: Arc<CudaContext>,
}
```

- `Encoder::initialize_with_cuda(cuda_ctx: Arc<CudaContext>) -> Result<Self, EncodeError>` (`encoder.rs:83`) — opens an NVENC session with the CUDA context as the device (`NV_ENC_DEVICE_TYPE_CUDA`).
- Capability queries: `get_encode_guids()` (`:139`), `get_preset_guids(GUID)` (`:184`), `get_profile_guids`, `get_supported_input_formats`, `get_preset_config(encode_guid, preset_guid, tuning)`.
- `start_session(self, buffer_format: NV_ENC_BUFFER_FORMAT, initialize_params: EncoderInitParams<'_>) -> Result<Session, EncodeError>` (`encoder.rs:424-427`). `EncoderInitParams<'a>` (`encoder.rs:464-467`) is a lifetime-tagged builder over `NV_ENC_INITIALIZE_PARAMS` (`new(encode_guid, width, height)`, `preset_guid`, `encode_config`, `display_aspect_ratio`, `framerate`, `enable_picture_type_decision` — `builders.rs:17-75`). `start_session` copies the caller's `NV_ENC_CONFIG` into a `Box` and re-points `init.encodeConfig` at the owned copy so `reconfigure` can resubmit it later (`encoder.rs:433-456`).
- `Drop` destroys the encoder (`encoder.rs:56-62`).

**safe/session.rs** — the moq-relevant additions to the upstream fork live
here (`session.rs:30-49`):

```rust
pub struct Session {
	pub(crate) encoder: Encoder,
	pub(crate) width: u32,
	pub(crate) height: u32,
	pub(crate) buffer_format: NV_ENC_BUFFER_FORMAT,
	pub(crate) encode_guid: GUID,

	/// The parameters the session was initialized with, retained so
	/// [`reconfigure`](Self::reconfigure) can resubmit them with one field
	/// changed: `NvEncReconfigureEncoder` takes the *whole* init params, not a
	/// delta.
	pub(crate) init: NV_ENC_INITIALIZE_PARAMS,

	/// Owned copy of the encode config `init.encodeConfig` points at. ...
	pub(crate) config: Option<Box<NV_ENC_CONFIG>>,
}
```

- **`reconfigure(&mut self, bitrate: u32)`** (`session.rs:123-146`) — moq's fork addition: changes `rcParams.averageBitRate` on the running session with `resetEncoder = 0`, `forceIDR = 0`: "safe to call as often as a congestion controller updates ... no IDR is forced and no state is reset" (`session.rs:107-121`). moq-video's `Backend::set_bitrate` maps straight onto it (`rs/moq-video/src/encode/backend/nvenc.rs:253-259`).
- `encode_picture<I: EncoderInput, O: EncoderOutput>(&self, input, output, params: EncodePictureParams)` (`session.rs:235-273`). Keyframes use the `NV_ENC_PIC_FLAG_FORCEIDR` flag rather than `pictureType` (honored even with picture-type decision enabled — comment at `session.rs:260-268`; hardware-verified fix `3b87a3dd`).
- `end_of_stream()` (`session.rs:287`); `Drop` sends EOS (`session.rs:296-301`).

`EncodePictureParams` / `CodecPictureParams` (`session.rs:302-341`):

```rust
pub struct EncodePictureParams {
	/// Opaque data used for identifying the corresponding encoded frame
	pub input_timestamp: u64,
	/// The picture type to use, if picture type decision is disabled in the encoder
	pub picture_type: NV_ENC_PIC_TYPE,
	/// Force this frame to be an IDR (`NV_ENC_PIC_FLAG_FORCEIDR`). Works with
	/// picture-type decision enabled, so it is the way to request an
	/// out-of-cadence keyframe.
	pub force_idr: bool,
	/// Codec-specific parameters
	pub codec_params: Option<CodecPictureParams>,
}

pub enum CodecPictureParams {
	H264(NV_ENC_PIC_PARAMS_H264),
	Hevc(NV_ENC_PIC_PARAMS_HEVC),
	Av1(NV_ENC_PIC_PARAMS_AV1),
}
```

**safe/buffer.rs** — I/O buffer types, all RAII with `Drop` cleanup. The
core traits (`buffer.rs:15-31`):

```rust
/// If a type implements this trait it means it is a valid input buffer
/// for the encoding API.
pub trait EncoderInput {
	/// Get the pitch (AKA stride) of the input resource.
	fn pitch(&self) -> u32;

	/// Get the handle of the input resource.
	fn handle(&mut self) -> *mut c_void;
}

/// If a type implements this trait it means it is a valid output buffer
/// for the encoding API.
pub trait EncoderOutput {
	/// Get the handle of the output resource.
	fn handle(&mut self) -> *mut c_void;
}
```

Concrete types:

- `Buffer<'a>` (`buffer.rs:255-259`, `pub(crate) ptr, pitch, encoder: &'a Encoder`; `unsafe impl Send`): NVENC-allocated CPU-writable input buffer, created by `Session::create_input_buffer` (`buffer.rs:85`). `lock()`/`try_lock()` return `BufferLock` (`buffer.rs:321,342,396-400`) whose `unsafe fn write(&mut self, data: &[u8])` copies into the driver's mapping, and which exposes `pitch()` — NVENC's chosen row stride, which "may exceed the visible width" (`buffer.rs:423-424`; moq-video writes each plane pitched via a `write_rows` helper).
- `Bitstream<'a>` / `BitstreamLock` (`buffer.rs:471-604`): output buffer from `Session::create_output_bitstream` (`buffer.rs:153`); `lock()` blocks until the frame is encoded and exposes `data() -> &[u8]`, `frame_index()`, `timestamp()`, `duration()`, `picture_type()` (`buffer.rs:577-601`).
- **`RegisteredResource<'a, T>` — the zero-copy hook** (`buffer.rs:614-657`):

```rust
/// Abstraction for a registered and mapped external resource.
///
/// The Encoder API exposes a way to use input buffers allocated externally,
/// for example through CUDA or OpenGL.
///
/// The buffer is automatically unmapped and unregistered when dropped.
/// The external buffer memory should still be properly destroyed by the client.
#[derive(Debug)]
pub struct RegisteredResource<'a, T> {
	pub(crate) reg_ptr: *mut c_void,
	pub(crate) map_ptr: *mut c_void,
	pitch: u32,
	encoder: &'a Encoder,
	// A generic marker to make sure the external resources are dropped
	// after the resource is unregistered.
	_marker: T,
}
```

Created by `Session::register_cuda_resource(pitch, MappedBuffer)`
(`buffer.rs:179-198`, resolves the device pointer via cudarc's
`DevicePtr::device_ptr` on the default stream) or the generic
`Session::register_generic_resource<T>(marker, NV_ENC_INPUT_RESOURCE_TYPE,
*mut c_void, pitch)` (`buffer.rs:208-249`), which calls
`NvEncRegisterResource` + `NvEncMapInputResource` and unwinds both in `Drop`.
It implements `EncoderInput` (`buffer.rs:649+`) so a mapped GPU buffer plugs
straight into `encode_picture` — this is what makes GPU-resident frames
encodable without a host copy.

**safe/result.rs** — `EncodeError { kind: ErrorKind, string: Option<String>
}` mapping `NVENCSTATUS`, with recoverable kinds `EncoderBusy`,
`NeedMoreInput`, `LockBusy` documented on the calling methods.

### cuvid.rs (NVDEC decode surface)

Not a safe wrapper — a runtime-resolved function table
(`cuvid.rs:1-11`): "The raw bindings in `sys` declare the functions in an
`extern "C"` block, but calling those directly would make the *linker*
require `libnvcuvid` ... everything is resolved at runtime with dlopen.
Unlike the encode side's lazy static (which panics when the driver is
absent), `Api::get` returns an error so callers can fall back to another
decoder." `lib.rs:26-28` confirms: "there is no higher-level safe wrapper
yet."

`cuvid.rs:32-59`:

```rust
/// The NVDEC entry points, resolved from `libnvcuvid` at runtime.
///
/// A caller drives the usual CUVID flow: create a parser
/// ([`create_video_parser`](Self::create_video_parser)), feed it bitstream
/// packets, and inside the parser callbacks create a decoder, decode pictures,
/// and map/unmap the decoded frames. All calls require a current CUDA context.
pub struct Api {
	pub create_video_parser: CreateVideoParser,
	pub parse_video_data: ParseVideoData,
	pub destroy_video_parser: DestroyVideoParser,
	pub get_decoder_caps: GetDecoderCaps,
	pub create_decoder: CreateDecoder,
	pub destroy_decoder: DestroyDecoder,
	pub decode_picture: DecodePicture,
	pub map_video_frame: MapVideoFrame64,
	pub unmap_video_frame: UnmapVideoFrame64,
}
```

`Api::get() -> Result<&'static Api, &'static str>` (`cuvid.rs:64-68`,
`OnceLock`-cached). Load candidates: `libnvcuvid.so.1` / `libnvcuvid.so` on
Linux, `nvcuvid.dll` on Windows, empty elsewhere (`cuvid.rs:73-81`); the
library handle is deliberately leaked so function pointers live for the
process (`cuvid.rs:84-92`).

### How moq-video consumes it

Behind opt-out default-on features (`rs/moq-video/Cargo.toml:24-33`): `nvenc
= ["dep:cudarc", "dep:moq-nvenc", "dep:libloading"]`, `nvdec = [...]` (same
deps).

Encode (`rs/moq-video/src/encode/backend/nvenc.rs`): `struct Nvenc {
session: Session, _cuda: Arc<CudaContext>, timestamp: u64 }` (`nvenc.rs:59-63`)
implements the crate's `Backend` trait. `open` probes `driver_libs_present()`
first (dlopen would otherwise panic-abort on driverless hosts,
`nvenc.rs:71-82`), starts from the low-latency P4 preset
(`NV_ENC_PRESET_P4_GUID` + `NV_ENC_TUNING_INFO_LOW_LATENCY`), sets CBR,
`gopLength`, `frameIntervalP = 1` (no B-frames), `repeatSPSPPS(1)` and
`idrPeriod == gopLength` so every IDR carries in-band SPS/PPS for late
joiners (`nvenc.rs:90-125`). H.264 + H.265 (`codec_guid`, `nvenc.rs:52-57`).
`set_bitrate` → `session.reconfigure` ("no IDR, no reset",
`nvenc.rs:253-259`).

**The zero-copy path** (`nvenc.rs:177-200`): a `Frame::Cuda` (NVDEC output,
NV12 in device memory) is registered directly:

```rust
// A CUDA frame is already NV12 in device memory (NVDEC output):
// register its buffer as an external NVENC resource and encode in
// place, no CPU round trip and no GPU copy.
#[cfg(feature = "nvdec")]
Frame::Cuda(cuda) => {
	let mut resource = self
		.session
		.register_generic_resource(
			(),
			NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
			cuda.device_ptr() as *mut std::ffi::c_void,
			cuda.pitch,
		)?;
	self.session.encode_picture(&mut resource, &mut output, params)?;
	drain_output(&mut output)?
}
```

CPU frames instead go through `create_input_buffer` + pitched row-by-row NV12
writes (`nvenc.rs:202-238`). The output is drained (synchronous bitstream
lock) before the input is released, since the lock returning is what
guarantees NVENC finished reading the input (`nvenc.rs:172-176,263+`).

Decode (`rs/moq-video/src/decode/backend/nvdec.rs`): drives `cuvid` with a
synchronous parser (each access unit pushed with `CUVID_PKT_ENDOFPICTURE`,
zero display delay, callbacks fire inline, `nvdec.rs:17-22`). "Decoded 8-bit
4:2:0 frames come back as NV12 in CUDA device memory (`Frame::Cuda`). Each
mapped cuvid surface is copied device-to-device into an owned buffer
(surfaces come from a small fixed pool ...) which the NVENC encode backend
then registers directly: the decode -> scale -> encode transcode path never
touches the CPU" (`nvdec.rs:8-14`; `memcpy_dtod_sync` at `nvdec.rs:378`,
`map_video_frame`/`unmap_video_frame` at `nvdec.rs:358,385`). Scaling rides
the decoder via cuvid target size (`Config::resize`). H.264 / H.265 / AV1
decode (`nvdec.rs:1`). A hardware test asserts the loop stays on GPU:
"NVDEC produced a non-CUDA frame; the zero-copy path is not exercised"
(`nvdec.rs:666-667`). So the "zero-copy" story is precisely: one D2D copy out
of the cuvid surface pool, then NVENC reads that CUDA buffer in place through
`RegisteredResource` — zero host copies end to end.

### Standalone / publishable?

Effectively yes, by design: it has crates.io-style metadata (description,
keywords, categories, MIT license file inherited from upstream,
`Cargo.toml:8-14`), only three external deps (cudarc / lazy_static /
libloading), no path-only deps, no build script, compiles on every platform,
and is consumed by moq-video purely as a workspace dependency (`moq-nvenc =
{ workspace = true, optional = true }`, `rs/moq-video/Cargo.toml:90`). The
0.0.1 version plus `db0b5564` ("let release-plz own the moq-ffi version; drop
AI marker from moq-nvenc") shows it is wired into the workspace release flow.
Caveats for reuse outside moq: the safe layer is self-described as "largely
unfinished" (`safe/mod.rs:3-4`), decode has no safe wrapper, the encode lazy
static panics without the driver (callers must probe first), and the fork
diverges from upstream (dlopen-only, `reconfigure`, `force_idr`,
doc-test crate rename `86ec663d`).
