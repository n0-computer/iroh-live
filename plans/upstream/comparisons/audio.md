# 6. Audio stack comparison: iroh-live vs moq

Compares iroh-live's audio stack (rusty-codecs `opus`/`pcm`, moq-media
`audio_backend` + `aec`, `pipeline/audio_{encode,decode}`, and the symphonia file
source) against moq's `moq-audio` at main HEAD `3a3e0ea8`. moq is a single branch
since the dev line merged into main on 2026-07-21, so the capture-and-publish
audio stack (the `b9c539c0` "align the moq-audio capture/encode surface with
moq-video" layout, macOS system audio, device enumeration, and the async
permission flow) is moq main. moq-audio is version 0.0.9 and essentially
unchanged since the pre-merge analysis: `git diff 261c2048..main -- rs/moq-audio`
is two trivial one-liners (`encode/producer.rs:136` moved to the shareable
`catalog.timeline(name).section()` API, and `resample.rs:39` bumped `f_cutoff` to
`Some(0.95)` for a rubato API change), neither structural, so every quote and
line citation below is exact against main. iroh-live paths are relative to this
repo; moq paths are prefixed `3a3e0ea8:` and read via `git show`. Trait and
API-shape questions are analyzed in
[3t-compare-traits-api.md](3t-compare-traits-api.md); concrete moq-side change
proposals live in [3u-moq-changes.md](3u-moq-changes.md).

Both stacks encode Opus with the same backend, `unsafe-libopus 0.2`
(rusty-codecs/src/codec/opus/encoder.rs:4, `3a3e0ea8:rs/moq-audio/Cargo.toml:56-63`,
chosen there for the same reason: no CMake, no RUSTSEC-2026-0150). That makes the
codec layer genuinely mergeable. Everything above it diverges sharply: theirs is
a capture-and-publish crate with no playback path at all, ours is a full-duplex
device stack with AEC, mixing, and a sink abstraction that doubles as the A/V
sync anchor.

---

## 1. Codec layer

### 1.1 Opus encoder, feature by feature

| Feature | iroh-live `OpusEncoder` | moq-audio `encode::Encoder` |
|---|---|---|
| Backend | unsafe-libopus 0.2 | unsafe-libopus 0.2 |
| Application mode | `OPUS_APPLICATION_VOIP` (rusty-codecs/src/codec/opus/encoder.rs:58) | `OPUS_APPLICATION_AUDIO` (`3a3e0ea8:rs/moq-audio/src/encode/encoder.rs:177`) |
| Codec sample rate | Fixed 48 kHz, requested rate ignored (encoder.rs:15, :160-164) | Input rate snapped up to nearest of 8/12/16/24/48 kHz via `pick_rate`, or pinned by `Config::sample_rate` (`opus.rs:13,20-22`, `encoder.rs` Config field) |
| Frame duration | Fixed 20 ms, `FRAME_SIZE = 960` (encoder.rs:16-17) | Configurable 2.5/5/10/20/40/60 ms, validated in `opus::frame_size` (`opus.rs:16,43-51`), default 20 ms |
| Channels | 1 or 2 implied by OpusHead mapping family 0; no explicit validation | Validated 1 or 2 in `opus::validate_channels` (`opus.rs:33-40`); remap rejected |
| Bitrate at construction | Always set via `OPUS_SET_BITRATE` (encoder.rs:68-75) | Optional; `None` lets libopus pick (`encoder.rs:182-188`) |
| Runtime bitrate change | Yes, `set_bitrate` re-issues the ctl (encoder.rs:206-219) | No; bitrate is construction-only. The map for moq-nvenc notes the contrast explicitly: video got `Session::reconfigure`, audio did not |
| FEC | Ctl wired, explicitly disabled with `TODO(phase3): enable FEC with adaptive loss percentage` (encoder.rs:76-83) | Not touched at all; no `OPUS_SET_INBAND_FEC` anywhere |
| DTX | Ctl wired, explicitly disabled with a phase-3 TODO (encoder.rs:84-88) | Not touched |
| Complexity | Not exposed | Not exposed |
| Input contract | Streaming: `push_samples` accumulates into `sample_buf`, `encode_pending` drains full 960-sample frames (encoder.rs:123-154, :194-204) | One-shot: `encode(&[f32])` demands exactly `frame_size() * codec_channels()` samples, else `Error::Misaligned` (`encoder.rs:237`); the accumulation buffer lives in `Producer::pending` one layer up (`encode/producer.rs:71-96`) |
| Output | `EncodedFrame { is_keyframe: true, timestamp, payload: Bytes }` queued in a `VecDeque`, drained by `pop_packet` (encoder.rs:143-147, :202-204) | `Bytes` returned directly from `encode` |
| Timestamps | Computed from `frame_count * 960 / rate` (encoder.rs:141-142), then overwritten by the pipeline with thread-relative wall time (moq-media/src/pipeline/audio_encode.rs:92) | Owned by `Producer`: `epoch_us` wall-clock anchor plus running sample count, with `reset_epoch()` to land idle gaps in the PTS (`encode/producer.rs:288-298,168`) |
| OpusHead | Built locally, 19 bytes per RFC 7845 5.1, with pre-skip queried from `OPUS_GET_LOOKAHEAD` (312 for VOIP at 48 kHz) (encoder.rs:91-108, :222-237) | Built by `moq_mux::codec::opus::Config::encode`, which hardcodes pre-skip 0 and gain 0 (`3a3e0ea8:rs/moq-mux/src/codec/opus/mod.rs`, `encode` fn); their parser also ignores pre-skip on read |
| Max packet | 4000 bytes (encoder.rs:19) | 4000 bytes, `MAX_PACKET_BYTES` per RFC 6716 3.4 (`encoder.rs:22`) |
| Catalog output | `AudioConfig` via `config()` and static `config_for` (encoder.rs:166-192) | `catalog() -> hang::catalog::AudioConfig` with OpusHead description, bitrate, and `Container::Legacy` (`encoder.rs:263`) |

Two substantive quality differences hide in that table. First, our pre-skip is
correct and theirs is zero, so a spec-conforming decoder consuming their stream
does not trim the encoder lookahead, and one consuming ours does. Second, the
application modes differ: VOIP biases libopus toward speech intelligibility and
loss robustness, AUDIO toward music fidelity. A merged wrapper should make the
application mode a config field rather than inheriting either hardcoded choice.

### 1.2 Opus decoder, feature by feature

| Feature | iroh-live `OpusAudioDecoder` | moq-audio `decode::Decoder` |
|---|---|---|
| Construction | `new(&AudioConfig, target_format)` from the catalog (rusty-codecs/src/codec/opus/decoder.rs:44-74) | `new(&hang::catalog::AudioConfig)`, parsing OpusHead from `description` with catalog-field fallback (`3a3e0ea8:rs/moq-audio/src/decode/decoder.rs:75`) |
| Max frame | 120 ms, 5760 samples (decoder.rs:11-12) | 120 ms, `MAX_FRAME_MS = 120` (`decoder.rs:14`) |
| PLC on loss | **No.** `opus_decode_float(..., 0)` always with real packet data (decoder.rs:80-89, the literal `0, // no FEC` at :87). There is no null-data decode call, so no concealment is generated on a gap. The pipeline papers over gaps by pushing raw silence into the sink instead (moq-media/src/pipeline/audio_decode.rs:236-246) | **Also no.** Same `opus_decode_float(..., 0)` shape (`decoder.rs:116-135`), and the map's finding stands: no `decode_lost` entry point exists. Their producer comment "Opus PLC handles dropped groups" (`encode/producer.rs:219-233`) is aspirational; nothing on the consume side invokes PLC |
| FEC decode | No (flag always 0) | No (flag always 0) |
| Resampling | Integrated: a `Resampler` to the sink's target rate runs inside `push_packet` (decoder.rs:61-62, :98) | External: `decode::Consumer` owns an optional `Resampler` applied after `decode` (`decode/consumer.rs:8-22,94-113`) |
| Channel remix | Integrated: mono to stereo, stereo to mono, and general N-to-M via mono intermediate (decoder.rs:100-111, `convert_channels_into` :136-186) | Rejected: `channels: None` matches the codec, anything else errors "since remapping isn't implemented" (`decode/decoder.rs:16-46`) |
| Output | Borrowed `&[f32]` via `pop_samples`, double-buffered with a swap so the borrow stays valid (decoder.rs:116-127) | Owned `Vec<f32>` allocated per call, truncated to the decoded count (`decoder.rs:117-134`) |
| Latency knob | None here; group skipping configured on the transport consumer by moq-media | `Config::latency_max` forwarded to `moq_mux::container::Consumer::with_latency`, with `latency_min` for jitter padding explicitly deferred (`decode/decoder.rs:33-43`) |

### 1.3 Wire shape and config transport

Both stacks put the Opus config in the catalog `description` as an OpusHead and
ship one Opus packet per moq-lite group. Theirs does it explicitly: every packet
is written `keyframe: true, duration: None` followed by `track.cut(None)`
(`3a3e0ea8:rs/moq-audio/src/encode/producer.rs:219-233`). Ours reaches the same
shape implicitly: every audio `EncodedFrame` carries `is_keyframe: true`
(rusty-codecs/src/codec/opus/encoder.rs:144), and `MoqPacketSink` maps the
keyframe flag onto the container producer's group cut
(moq-media/src/transport.rs:85-116). The streams are wire-compatible in group
structure; the OpusHead pre-skip is the only payload-level difference.

### 1.4 PCM codec (ours only)

moq-audio has no uncompressed codec; its `Codec` enum is single-variant `Opus`
with a comment rejecting a trait until AAC lands
(`3a3e0ea8:rs/moq-audio/src/encode/encoder.rs:1-7,24-42`). Ours has a full
`PcmEncoder`/`PcmAudioDecoder` pair: 20 ms frames of little-endian f32 chosen to
match Opus framing (rusty-codecs/src/codec/pcm/encoder.rs:11-16, :44-64), a
truthful computed bitrate of `rate * channels * 32` (pcm/encoder.rs:75), and a
decoder that reuses the same resample-plus-remix path as Opus
(pcm/decoder.rs:36-49, :68-96). It exists for test pipelines and for debugging
codec-versus-transport problems (bit-exact roundtrip is asserted at
pcm/encoder.rs:213-233), and it costs 559 lines total.

### 1.5 Verdict on the codec layer

Ours is the more complete Opus wrapper on control surface: runtime bitrate,
correct pre-skip, FEC/DTX ctls already plumbed (if disabled), decoder-side
resample and remix, and a second codec. Theirs is the more complete wrapper on
codec-parameter flexibility: variable frame durations, sub-48 kHz codec rates
with validation, optional bitrate, and clean separation of layout (Input) from
codec knobs (Config). Neither exposes complexity, and neither actually performs
PLC or FEC decode today, so the "resilience" story is open on both sides. A
merged wrapper needs: their rate/duration/validation layer, our runtime
`set_bitrate` and real pre-skip, a `decode_lost`-style PLC entry point plus FEC
request flag that neither has, and application mode as config.

---

## 2. Resampling

Ours lives in three places. The codec-facing instance is
`rusty_codecs::processing::resample::Resampler` (rubato `Async::new_sinc`,
sinc_len 256, oversampling 256, f_cutoff 0.95, linear interpolation,
BlackmanHarris2, chunk 1024, `FixedAsync::Input`, max ratio 1.1, identity
passthrough when rates match) used inside the Opus and PCM decoders
(rusty-codecs/src/processing/resample.rs:19-60). The device boundary does not use
rubato at all: `audio_backend` uses `fixed_resample::resampling_channel`, a
real-time-safe combined ring buffer and resampler with drift autocorrection
thresholds (moq-media/src/audio_backend.rs:1769-1818). The file importer carries
its own third rubato instance with the same 256/256 sinc parameters
(moq-media/src/audio_file_symphonia.rs:266-284).

Theirs is one type: `moq_audio::Resampler` wrapping rubato `Async::new_sinc`
with sinc_len 128, oversampling 128, f_cutoff 0.95, linear interpolation,
BlackmanHarris2, max ratio 1.0, caller-chosen `chunk_frames`, and explicit
pending-sample buffering with preallocated planar scratch buffers
(`3a3e0ea8:rs/moq-audio/src/resample.rs:32-70` and the `process` body). It is
sample-rate only; channel remix is rejected upstream by both encoder and decoder
configs. Verified: there is no channel mixing code anywhere in the crate.

Comparison: our sinc_len 256 is the higher-quality (and roughly 2x more
expensive) filter; theirs deliberately trims to 128 with default features off.
Theirs handles partial input explicitly via its `pending` buffer and reuses
planar scratch across calls; ours hands arbitrary-length interleaved input
straight to rubato's adapter API and allocates the output `Vec` per call,
plus a full input copy on the passthrough path (resample.rs:47-50). Ours also
does drift correction at the device boundary through fixed_resample, which
theirs has no equivalent for because it never touches a playback device.
Channel handling is the real split: we remix at the decoder (section 1.2),
they refuse. For a shared crate, their leaner wrapper plus our remix helper
would cover both needs; the fixed_resample device channel is not replaceable
by either rubato wrapper because it must be lock-free in the cpal callback.

---

## 3. Device I/O

This is where the stacks are not comparable in scope. Theirs is capture only.
Ours is a full-duplex audio engine.

### 3.1 What our audio_backend does (2,445 + 392 lines)

The module doc states the model: cpal streams connect directly to resampling
ring buffers, with AEC and peak metering applied inline in the callbacks, all
internal processing at 48 kHz stereo (moq-media/src/audio_backend.rs:1-23).
Enumerated responsibilities:

- **Device management.** Host selection, input/output enumeration
  (`available_hosts`, `list_inputs`, `list_outputs`, audio_backend.rs:147-165),
  device resolution with fallback-to-default (`AudioBackendOpts`, :84-109), and
  live device switching without recreating caller-side streams
  (`switch_input`/`switch_output` and `switch_devices_internal`, :224-283,
  :1720-1755).
- **Stream lifecycle and recovery.** A dedicated `AudioDriver` OS thread owns
  cpal streams and is driven by a **bounded** `mpsc::channel(32)` of
  `DriverMessage` (:167-176, :674). Stream error callbacks and excessive
  underruns feed `attempt_restart` with exponential backoff, 500 ms doubling to
  a 4 s cap (:1204-1206, :1381-1419).
- **Config negotiation.** `negotiate_stream_config` prefers 48 kHz, then
  44.1 kHz, then highest, always f32 (:802).
- **Playback with mixing.** The output callback drains a command channel,
  mixes all registered `OutputStream` producers with per-stream volume, applies
  a 4-state declicker fade machine (playing/fade-out/paused/fade-in, 144
  samples, ~3 ms) on pause and resume, clamps the mix, and records peaks
  (:65-76, :942-1050). Multiple decoded remote tracks mix into one device
  stream; moq-audio has nothing here at all.
- **Capture with fan-out.** The input callback channel-maps mono or N-channel
  devices to stereo, runs AEC in place, and distributes the processed buffer to
  every registered `InputStream` producer (:1094-1168), so one microphone feeds
  any number of encoders.
- **Real-time-safe buffering.** Every stream endpoint is a
  `fixed_resample::resampling_channel` (output 50 ms latency, 3 s capacity;
  input 30 ms, 1 s), with under/overflow autocorrection (:1769-1818). Callbacks
  are allocation-free and lock-free; commands cross via bounded SPSC channels
  (:16-23).
- **Echo cancellation.** `audio_backend/aec.rs` wraps the `sonora` crate
  (WebRTC AudioProcessing). The output callback writes the post-mix render
  reference into a 100 ms lock-free SPSC ring buffer; the input callback drains
  it and runs render and capture 10 ms frames serialized on the input thread,
  avoiding cross-callback locking (aec.rs:199-278, audio_backend.rs:68). AEC
  toggles at runtime with no stream restart (audio_backend.rs:180-189).
- **Metering and control.** Smoothed peak levels (`PeakState`, :595-640),
  per-stream volume, and the pause/resume/toggle surface exposed through
  `AudioSinkHandle`.

### 3.2 What their capture does (and does better)

moq-audio capture (`3a3e0ea8:rs/moq-audio/src/capture.rs` plus submodules):

- **Source enum** `Microphone(Option<String>) | System` (:26-48). `System` is
  macOS desktop audio via ScreenCaptureKit with
  `setExcludesCurrentProcessAudio(true)` so playback does not loop back
  (`capture/screencapture.rs:81-98`). We have no system-audio capture at all.
- **Device enumeration as data**: `devices() -> Vec<Device { id, name, default }>`
  (:236-262), similar to our `AudioDevice` (audio_backend.rs:124).
- **`format(&Config)` without opening the device** (:99-116), so the catalog can
  be registered before any hardware is touched. We open the stream to learn the
  negotiated format.
- **Permission flow**: AVFoundation TCC pre-check with the prompt bridged to a
  oneshot and a 30 s timeout (`capture/permission.rs:14-80`), plus a 5 s
  first-buffer timeout so a denied mic errors instead of hanging (:50-54). We
  have neither.
- **Demand gating**: `encode::publish_capture` registers the catalog rendition
  up front, then opens the device only while `track.used()` and releases it on
  `track.unused()`, calling `reset_epoch` on resume so the idle gap lands in
  the PTS (`encode/capture.rs:13-100`). Our publish layer has demand-gated
  track start (moq-media/src/publish.rs:333-363) and parking for the shared
  video source, but our audio capture stream stays open once created.
- **Cancel safety**: dropping the `publish_capture` future drops the cpal/SCK
  stream; the whole loop is an async task, no dedicated thread.

### 3.3 The channel discipline difference

Their microphone path forwards buffers from the real-time cpal callback over an
**unbounded** tokio mpsc channel, allocating a `Vec<f32>` per callback
(`3a3e0ea8:rs/moq-audio/src/capture.rs:140,162,230`). If the consumer stalls,
memory grows without bound, and the per-buffer allocation happens on the
real-time thread. Ours moves samples through preallocated lock-free
fixed_resample ring buffers with explicit push/read status handling
(audio_backend.rs:430-455, :511-540), and every control channel is bounded.
This matches the project's no-unbounded-channels rule and is the discipline any
merged capture layer should keep.

### 3.4 The missing half

Verified by search over the main tree: moq-audio contains no playback,
sink, speaker, or output-stream code; the only match for those terms is a doc
sentence in `format.rs` about "speaker buffers" crossing FFI
(`3a3e0ea8:rs/moq-audio/src/format.rs:8`). `decode::Consumer::read` hands you
PCM frames and stops there. Everything in section 3.1's playback, mixing,
fading, metering, AEC, and recovery list has **no moq equivalent** anywhere in
moq-audio.

---

## 4. File sources

Ours decodes files to PCM: `AudioFileSource` probes WAV/MP3/FLAC with
symphonia, decodes on a background thread, remixes to stereo, resamples to
48 kHz, and feeds a 2-second lock-free ring buffer behind the `AudioSource`
trait, with optional seamless looping
(moq-media/src/audio_file_symphonia.rs:36-123, :147-263). It exists so tests and
media playback drive the same encode pipeline as a microphone, with no ffmpeg
runtime dependency (:1-9).

moq has no decoded-PCM file source. Its file story is container importers in
moq-mux: `import::{Track, TrackStream, Container, ContainerStream}` pick a
concrete importer from a format string ("avc3", "fmp4", ...) and republish the
**compressed** bitstream into a broadcast, with container backends for flv,
fmp4, hls, legacy, loc, mkv, and ts (`3a3e0ea8:rs/moq-mux/src/import/mod.rs:1-26`).
The approaches are complementary rather than
competing: importers avoid a decode/re-encode cycle when the file already
contains a streamable codec, while our source feeds raw PCM into any encoder
(useful for PCM tracks, AEC experiments, and codec tests). Neither can replace
the other, and since we already consume moq-mux, adopting their importers for
pass-through file publishing costs nothing.

---

## 5. A/V sync touchpoint

On our side, audio playback is the pacing master. The decode loop pushes samples
into the sink as they arrive, and the sink's fixed_resample ring buffer provides
the smoothing; `AudioSink::occupied_seconds` reports the buffered duration
between `push_samples` and the hardware callback (rusty-codecs/src/traits.rs:38-45,
moq-media/src/audio_backend.rs:413-418), which the decode loop uses for
speaker-lag stats and silence insertion (pipeline/audio_decode.rs:186-206,
:236-246), and which the `Sync` clock's design assumes as the audio anchor while
video waits (moq-media/src/sync.rs:36-40). Their `decode::Consumer` exposes
exactly two of the inputs a playout clock needs, per-frame timestamps
(`Frame.timestamp`, `3a3e0ea8:rs/moq-audio/src/decode/consumer.rs:94-113`) and a
`latency_max` group-skip bound, but with no sink there is no buffered-duration
signal and no pacing point; a sync clock built on their consumer must own its
own output buffering to have anything to steer. The full sync analysis is in
5-compare-pubsub.

---

## 6. Traits and API shape

Ours, verbatim (rusty-codecs/src/traits.rs):

```rust
pub trait AudioEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> { Ok(()) }
}

pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self> where Self: Sized;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

pub trait AudioSource: Send + 'static {
    fn format(&self) -> AudioFormat;
    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>>;
}

pub trait AudioSink: AudioSinkHandle {
    fn format(&self) -> Result<AudioFormat>;
    fn push_samples(&mut self, buf: &[f32]) -> Result<()>;
    fn handle(&self) -> Box<dyn AudioSinkHandle>;
    fn occupied_seconds(&self) -> f64 { 0.0 }
}

pub trait AudioStreamFactory: Send + Sync + 'static {
    fn create_input(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSource>>>;
    fn create_output(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSink>>>;
}
```

(traits.rs:175-192, :217-227, :22-28, :31-45, :88-94; `AudioSinkHandle` adds
pause/resume/toggle, peak metering, and volume behind `Send + Sync` with
`Clone for Box<dyn AudioSinkHandle>`, :54-82. `AudioEncoderFactory` adds
`const ID`, `with_config`, `config_for`, and `with_preset`, :142-168.)

Theirs, verbatim (`3a3e0ea8:rs/moq-audio/src`):

```rust
// encode/encoder.rs:129-160, :237, :263
pub struct Encoder { inner: *mut OpusEncoder, config: Config, codec_rate: u32,
    codec_channels: u32, frame_size: usize, scratch: Vec<u8> }
unsafe impl Send for Encoder {}
impl Encoder {
    pub fn new(config: &Config) -> Result<Self, Error>;
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Bytes, Error>;
    pub fn catalog(&self) -> hang::catalog::AudioConfig;
}

// decode/decoder.rs:56-70, :117
pub struct Decoder { inner: *mut OpusDecoder, sample_rate: u32,
    channel_count: u32, max_frame_size: usize }
unsafe impl Send for Decoder {}
impl Decoder {
    pub fn new(config: &hang::catalog::AudioConfig) -> Result<Self, Error>;
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, Error>;
}

// encode/capture.rs:23-30
pub async fn publish_capture(
    broadcast: moq_net::broadcast::Producer,
    catalog: moq_mux::catalog::Producer,
    capture: capture::Config,
    encode: Options,
    clock: moq_mux::Clock,
) -> Result<(), Error>;
```

Same analysis lens as the video trait comparison:

- **Streaming push/pop vs one-shot.** Our encoder buffers internally and emits
  zero or more packets per push; theirs demands exact frame-sized input and
  returns exactly one packet, pushing accumulation up into `Producer::pending`.
  Their split is cleaner layering; ours is friendlier to arbitrary-size sources.
  Our decoder's borrowed `&[f32]` return avoids the per-packet `Vec` their
  `decode` allocates, at the cost of the double-buffer swap dance
  (opus/decoder.rs:116-127).
- **Traits vs concrete structs.** They deliberately refused a codec trait until
  a second codec exists (`encode/encoder.rs:1-7`); dispatch is `match
  config.codec` on a `#[non_exhaustive]` enum. We have trait objects, boxed
  forwarding impls, and `AudioEncoderFactory` for the rendition registry in
  publish.rs, which needs to construct encoders from closures per rendition
  (moq-media/src/publish.rs:721-728). With two codecs (Opus, PCM) our trait
  earns its keep; with one, theirs is honest.
- **Factory vs turnkey function.** Our `AudioStreamFactory` abstracts device
  I/O behind async constructors so pipelines are testable against fake
  backends; their `publish_capture` is a concrete five-argument async function
  wiring cpal to the wire. Ours composes (the same factory serves encode and
  decode pipelines); theirs is simpler to call but closed.
- **Frame types.** Their `Frame { timestamp: Timestamp, data: Bytes }` with
  layout fixed at construction (`frame.rs:4-18`) plus the WebCodecs `Format`
  enum with the `Cow` zero-copy `as_interleaved_f32` path (`format.rs:65-93`)
  is a genuinely nice FFI-facing PCM surface we lack; we deal only in
  interleaved f32 slices and `AudioFormat { sample_rate, channel_count }`.

---

## 7. Verdict

Per piece, in the cut/keep/upstream frame. Everything moq brings here is on moq
main today, so each verdict is actionable against the next moq release iroh-live
bumps to, with no dependency on an unmerged branch. One current-main delta
reinforces the codec-layer merge: `hang` is now 0.19.5 and #2420 renamed its
catalog `displayRatio*` fields to `displayAspect*`, so the `rusty-codecs`
`config.rs` mirror (which still carries `display_ratio_*`, `config.rs:11-33`) no
longer compiles against it. Building our audio catalog output on moq-audio's
`Encoder::catalog()` (which returns a `hang::catalog::AudioConfig` directly)
retires the part of that mirror we own on the audio side rather than chasing
`hang`'s field renames by hand.

- **Opus wrapper: merge.** Same libopus base makes this the easiest merge in
  the stack. Contribute from ours: runtime `set_bitrate`, lookahead-derived
  pre-skip (fix their zero pre-skip regardless), FEC/DTX ctl plumbing, and the
  decoder-side remix. Adopt from theirs: rate snapping and validation, variable
  frame durations, and the layout/knob config split. Add what neither has: a
  PLC `decode_lost` path, FEC decode, and complexity control. Decide the
  application mode explicitly.
- **PCM codec: keep, offer upstream.** 559 lines, no dependencies, valuable
  for test paths and pipeline debugging. It slots into their `Codec` enum
  trivially if wanted; nothing forces the issue.
- **Resampler: converge on theirs, keep our remix.** Their wrapper is leaner
  (sinc 128, pending-buffer handling, preallocated scratch); ours is
  higher-quality but allocates per call. Channel remix stays ours since they
  reject it. The fixed_resample device channel is orthogonal and stays.
- **audio_backend + AEC: keep; no moq equivalent exists.** Playback, mixing,
  declicker fades, volume, metering, device switching, restart with backoff,
  and sonora AEC have zero counterpart anywhere in moq-audio. This is the
  strongest asset of our audio stack and a candidate to upstream as a standalone
  playback/duplex crate, but it must not be cut in any alignment.
- **Capture: adopt theirs.** System audio (macOS SCK), `format()` without
  opening the device, TCC permission flow, device enumeration, and
  demand-gated open/close with `reset_epoch` are all things ours lacks. Port
  them onto bounded ring buffers; do not import their unbounded realtime-to-
  async mpsc (`capture.rs:162`).
- **File source: keep ours, use their importers alongside.** Decoded-PCM
  sources and container remuxers solve different problems.
- **Pipelines and sync hooks: keep ours, pending 5-compare-pubsub.** Their
  consumer gives timestamps and `latency_max` but no pacing surface; our
  `AudioSink::occupied_seconds` anchor has no replacement on their side.
