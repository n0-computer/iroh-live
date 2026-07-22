# Trait / Interface / Rust-API Comparison: rusty-codecs and moq-media versus moq-video / moq-audio

> Campaign: upstream | Kind: comparison | Read ../0-overview.md first; index at 0-index.md.

Scope: the API-level comparison the alignment discussion has to be grounded in.
Every decision that adopting or converging with moq's codec surface would force is
listed in section 8 with options and a recommendation. This is the primary discussion
artifact; the concrete upstream change proposals live in the companion
`moq-changes.md`, which this document references rather than drafts.

Sources, read directly from the working trees on 2026-07-22:

- iroh-live: `rusty-codecs/src/{traits,format,config}.rs`, `moq-media/src/{transport,publish}.rs`,
  and the platform backends under `rusty-codecs/src/codec/`.
- moq: `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8` (`feat(stats): count datagrams in the
  model layer (#2430)`, 2026-07-21). moq merged its `dev` line into `main` on 2026-07-21,
  so there is one moq codebase and this document draws no main-versus-dev distinction:
  every moq citation is the current tree. Paths are abbreviated to `rs/moq-video/...`,
  `rs/moq-audio/...`, `rs/moq-net/...`, and `rs/hang/...`.

Companion evidence maps live under `maps/`.

---

## 1. Design-philosophy contrast

### 1.1 Ours: open traits, factory per role, push/pop streaming, transport-free crate

rusty-codecs defines one public trait per codec role, a factory trait per encoder role,
and boxed forwarding impls for object safety. The full roster in
`rusty-codecs/src/traits.rs`:

- `VideoEncoder` (traits.rs:338-355) and `VideoEncoderFactory` (traits.rs:311-331).
- `VideoDecoder` (traits.rs:380-410).
- `AudioEncoder` (traits.rs:175-192) and `AudioEncoderFactory` (traits.rs:142-168).
- `AudioDecoder` (traits.rs:217-227).
- `Decoders` (traits.rs:14-19), the associated-type pairing of an audio and a video
  decoder for the dynamic pipeline.
- The device-layer traits: `VideoSource` (traits.rs:230-241), `PreEncodedVideoSource`
  (traits.rs:268-287), `AudioSource` (traits.rs:22-28), `AudioSink` (traits.rs:31-45),
  `AudioSinkHandle` (traits.rs:54-76), and `AudioStreamFactory` (traits.rs:88-94).

Every codec trait uses a push/pop streaming shape. Input goes in through `push_frame`,
`push_packet`, or `push_samples`; output is drained through `pop_packet`, `pop_frame`,
or `pop_samples`; and the two calls are decoupled in time. The crate has no moq, hang,
or iroh dependency. Its transport seam is `moq-media`'s `PacketSource` (transport.rs:16)
and `PacketSink` (transport.rs:24), implemented by `MoqPacketSource` (transport.rs:34)
and `MoqPacketSink` (transport.rs:89). Runtime backend selection is layered on top with
enums (`codec::VideoCodec`, `DynamicVideoDecoder`) that probe hardware in order.

iroh-live's publish path depends on the factory traits. The `const ID: &str` names the
rendition (`format!("video/{}-{}", E::ID, preset)`, moq-media/src/publish.rs:1016;
audio at publish.rs:828), and the static `config_for(&VideoEncoderConfig) -> VideoConfig`
(traits.rs:322, called at publish.rs:1018) produces the catalog entry without
constructing an encoder. That static config is what lets `LocalBroadcast` register every
simulcast rendition up front and start encoders lazily on subscriber demand.

### 1.2 Theirs: concrete public structs, private Backend trait, one-shot calls, Candidate table

moq-video's public codec surface is two concrete structs, `encode::Encoder`
(rs/moq-video/src/encode/encoder.rs:106-111) and `decode::Decoder`
(rs/moq-video/src/decode/decoder.rs:85-89), with no public trait anywhere. The crate
doc states the rule verbatim (rs/moq-video/src/lib.rs:38-44):

> The public API is codec-agnostic: no public type, signature, or error variant names a
> backend (openh264 / VideoToolbox / NVENC / NVDEC) or a capture implementation.

The pluggability seam is a `pub(crate)` trait per direction. Encode
(rs/moq-video/src/encode/backend/mod.rs:37-57):

```rust
pub(crate) trait Backend: Send {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;
	fn finish(&mut self) -> Result<Vec<Bytes>, Error>;
	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;
	fn name(&self) -> &str;
}
```

Decode (rs/moq-video/src/decode/backend/mod.rs:67-75):

```rust
pub(crate) trait Backend: Send {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Decoded>, Error>;
	fn name(&self) -> &str;
}
```

Backend selection is a data-driven candidate table, not a match. Encode
(rs/moq-video/src/encode/backend/mod.rs:60-64):

```rust
struct Candidate {
	name: &'static str,
	codecs: &'static [Codec],
	open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
}
```

`open(config)` filters candidates by codec support, orders them per `Kind` (Auto is
hardware then software), tries each in order, and reports the tried list on exhaustion
(encode/backend/mod.rs:106-134; the decode mirror at decode/backend/mod.rs:119-146 adds
a per-candidate `supports: fn(Codec) -> bool` at decode/backend/mod.rs:81-85). The
`HARDWARE` and `SOFTWARE` candidate slices are `const` and internal
(encode/backend/mod.rs:68-102).

Above the codec layer sit the track-facing types, `encode::Producer`
(rs/moq-video/src/encode/producer.rs:43-45) and `decode::Consumer`
(rs/moq-video/src/decode/consumer.rs:15-22), both coupled to `moq_mux`, `moq_net`, and
`hang`. moq-audio follows the same layout but goes further: it has no trait at all, only
concrete `Encoder` and `Decoder` structs with `match config.codec` dispatch, with an
explicit note that a trait now would be premature (rs/moq-audio/src/encode/encoder.rs:1-7):

> When AAC or other codecs land we'll factor out a backend dispatch behind [`Codec`];
> introducing a trait now would be premature.

### 1.3 Consequences

Extensibility by downstream crates. Anyone can implement `VideoEncoder` outside
rusty-codecs today; nothing outside moq-video can implement their `Backend`, because it
is `pub(crate)`. Every new moq backend is a change inside moq-video's tree. That is a
deliberate posture: moq vendors everything (openh264 statically, an in-tree NVENC
binding, a trimmed VAAPI crate) and keeps its runtime dependencies minimal. For
iroh-live, which carries out-of-tree platform backends (V4L2 M2M, Android MediaCodec,
VAAPI decode) plus a wgpu render integration, this is the single biggest structural
conflict, treated in sections 3 and 5.

API stability. moq's shape is maximally semver-stable: `#[non_exhaustive]` on every
config and public enum (`Codec` at encode/encoder.rs:21-30, `Kind` at
encode/encoder.rs:34-48, `Config` at encode/encoder.rs:55-70, the `Error` enum in
rs/moq-video/src/error.rs), no trait whose method set can break downstream implementors,
and no backend type in the public API. Our trait methods are a public contract, so
adding a required method breaks every implementor. We own all implementors today, which
is why that cost is theoretical for us and load-bearing for them.

Testability. Our traits admit mock encoders and decoders in moq-media tests without a
real codec. Their private trait keeps dispatch tests in-crate and drives integration
tests through the concrete front ends (for example `software_encoder_emits_annexb`,
encode/encoder.rs:284-311, and `openh264_round_trip`, decode/decoder.rs:280-284).
Adopting their shape wholesale would cost us in-tree mocks unless the sans-IO front ends
stay cheap to construct, which they are: openh264 is always compiled in.

Coupling to transport. Both sides keep the codec core transport-free: their
`Encoder`/`Decoder` and our trait objects speak `Bytes` and frames, and both isolate the
container coupling in a separate layer (their `Producer`/`Consumer`, our
`MoqPacketSink`/`MoqPacketSource` plus pipelines). The difference is that their transport
layer lives in the same crate, and their sans-IO layer is documented as the escape hatch
(rs/moq-video/src/decode/decoder.rs:80-86):

> The bring-your-own-payload layer under [`Consumer`]: use it when the frames don't come
> from a plain track subscription, e.g. a transcoder serving individually fetched groups.

That sans-IO seam is exactly what iroh-live would keep its own pipeline behind
(section 7).

Catalog timing. Our `config_for` registers the catalog entry before any encoding
happens. Their video path registers the rendition only after the importer parses the SPS
out of the first keyframe (`capture_loop` sets `catalog_ready |= !packets.is_empty()`,
encode/producer.rs:391, with the comment "The catalog video rendition only appears once
a frame has been encoded (the importer reads the SPS)", producer.rs:315-318). Their
audio path, by contrast, registers at construction (`catalog_mut.lock().audio.insert`,
rs/moq-audio/src/encode/producer.rs:137, documented at producer.rs:78-79 as "registered
at construction (not on first write)"). Converging on their video Producer changes when
subscribers first see a rendition, which matters for our quality-ranked rendition
selection at subscribe time.

---

## 2. Exhaustive side-by-side signature tables

### 2.0 Cross-cutting: timestamps and error types

Timestamps cut across every row. Ours is `std::time::Duration` relative to an arbitrary
pipeline epoch (`VideoFrame.timestamp`, rusty-codecs/src/format.rs:583; `MediaPacket` and
`EncodedFrame` at format.rs:393 and format.rs:415). Theirs is `moq_net::Timestamp`, a
`(value: VarInt, scale: Timescale)` pair (rs/moq-net/src/model/time.rs:151-154)
documented as "An instant, not a number ... so it has no arithmetic operators"
(time.rs:130-136). It has no naked operators; combination goes through `checked_add`,
`checked_sub`, and `convert`, all scale-aware, and construction is fallible
(`from_micros(u64) -> Result<Timestamp, TimeOverflow>`, time.rs:207). It is the wire type
on the hang container. Our `Duration` forces a conversion at every codec boundary an
integration would cross.

Error types. Ours is `anyhow::Result` on every trait method. Theirs is a structured
`#[non_exhaustive]` enum with actionable variants (rs/moq-video/src/error.rs):
`NoEncoder(String)`, `NoDecoder(String)`, `UnsupportedCodec(String)`,
`Unsupported(String)`, `InvalidFramerate(u32)`, `BitrateUnsupported(&'static str)`,
`Codec(#[from] anyhow::Error)` as the catch-all, plus `Mux`, `Net`, and `TimeOverflow`
transport variants. moq-audio has its own parallel `Error` with `Misaligned { got,
expected }` (used by `encode` when the buffer is not exactly one codec frame).

### 2.1 Video encode

Ours, verbatim (rusty-codecs/src/traits.rs:311-355):

```rust
pub trait VideoEncoderFactory: VideoEncoder {
    const ID: &str;
    fn with_config(config: VideoEncoderConfig) -> Result<Self> where Self: Sized;
    fn config_for(config: &VideoEncoderConfig) -> VideoConfig;
    fn with_preset(preset: VideoPreset) -> Result<Self> where Self: Sized { .. }
}

pub trait VideoEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> VideoConfig;
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> { Ok(()) }   // default no-op
}
```

Theirs, verbatim (rs/moq-video/src/encode/encoder.rs; the private `Backend` is quoted in
section 1.2):

```rust
pub struct Encoder { backend: Box<dyn Backend>, codec: Codec, size: Size, bitrate: u64 }   // :106-111

impl Encoder {
    pub fn new(config: &Config) -> Result<Self, Error>;                                                   // :115-133
    pub fn name(&self) -> &str;                                                                            // :136
    pub fn size(&self) -> Size;                                                                            // :141
    pub fn bitrate(&self) -> u64;                                                                          // :148
    pub fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;                                     // :166-175
    pub fn codec(&self) -> Codec;                                                                          // :179
    pub fn encode_rgba(&mut self, rgba: &[u8], size: Size, keyframe: bool) -> Result<Vec<Bytes>, Error>;  // :189-197
    pub fn encode_i420(&mut self, i420: &[u8], size: Size, keyframe: bool) -> Result<Vec<Bytes>, Error>;  // :210-219
    pub fn encode(&mut self, frame: &crate::decode::Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;  // :249-251
    pub fn finish(self) -> Result<Vec<Bytes>, Error>;                                                      // :270-272
}
```

| Concern | Ours | Theirs | Difference |
| --- | --- | --- | --- |
| Call shape | `push_frame` then `pop_packet`, decoupled | `encode*(...) -> Vec<Bytes>`, one call | We stream; they are one-shot, zero or more packets per input. Section 6. |
| Input type | owned `VideoFrame` (Packed, I420, Nv12, Gpu) | `&[u8]` RGBA, `&[u8]` I420, or `&decode::Frame` | Their public `encode` inputs are CPU slices or a decoded frame; RGBA converts to I420 at the door (encoder.rs:195), `encode_i420` copies to own (encoder.rs:213-217). Our GPU capture frames have no public entry on their side. |
| GPU input | any `FrameData::Gpu` variant | only via `pub(crate) encode_raw(&Frame)` (encoder.rs:255-264), reachable publicly only through `encode(&decode::Frame)` | Their zero-copy capture-to-encode path is crate-private; the only public GPU encode input is a frame their own decoder produced. |
| Keyframe | none; cadence from `keyframe_interval` config (format.rs:996-998) | per-call `keyframe: bool` forcing an IDR (Backend contract, encode/backend/mod.rs:38-40) | Real gap on our side: no caller can force an IDR. Adaptive resubscribe and demand reopen both want one. |
| Bitrate change | `set_bitrate` default no-op `Ok(())` (traits.rs:352-354) | `set_bitrate` mandatory, `Error::BitrateUnsupported` when unsupported (encode/backend/mod.rs:50-53) | Theirs refuses to update the cached rate until the backend accepts (encoder.rs:170-174), and the rate loop retires on `BitrateUnsupported` (encode/producer.rs:275-278). Strictly better for congestion control. |
| Flush | none; buffered tail lost on drop | `finish(self) -> Vec<Bytes>` (encoder.rs:270-272), plus `Backend::finish` | Minor for live, wrong for file export. |
| Config | `VideoEncoderConfig` (format.rs:983-1011) | `Config` (encode/encoder.rs:55-70) | See section 4. Their `Config` has `codec` and `kind` fields; ours has no codec field because the codec is the factory type, and no `Kind` because backend order lives in `DynamicVideoDecoder`. Ours adds `scale_mode` and `nal_format`; theirs is Annex-B in-band only, so it has neither. Both default bitrate to ~0.07 bits per pixel (encoder.rs:94-100; format.rs:1096-1099). |
| Codec matrix | H.264 and AV1, no H.265 | H.264 and H.265, no AV1 encoder (`Codec`, encode/encoder.rs:21-30) | Disjoint. Neither side encodes the other's second codec. |
| Send bounds | `VideoEncoder: Send + 'static` | `Backend: Send`; `Encoder` is `Send` | Both confine `!Send` internals behind documented `unsafe impl Send`. |
| Error | `anyhow::Result` | structured `Error` | Section 2.0. |

### 2.2 Video decode

Ours, verbatim (rusty-codecs/src/traits.rs:380-410):

```rust
pub trait VideoDecoder: Send + 'static {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self> where Self: Sized;
    fn name(&self) -> &str;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn reset(&mut self) -> Result<()> { Ok(()) }
    fn set_viewport(&mut self, w: u32, h: u32);
    fn burst_size(&self) -> usize { 0 }
}
```

Theirs, verbatim (rs/moq-video/src/decode/decoder.rs; `Decoded` at
decode/backend/mod.rs:53-62):

```rust
pub struct Decoder { backend: Box<dyn Backend>, conversion: Conversion, got_keyframe: bool }   // :85-89

impl Decoder {
    pub fn new(catalog: &hang::catalog::VideoConfig, config: &Config) -> Result<Self, Error>;               // :94-140
    pub fn name(&self) -> &str;                                                                              // :143-145
    pub fn decode(&mut self, payload: &Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Frame>, Error>;  // :152-184
}

#[non_exhaustive]
pub struct Config { pub kind: Kind, pub latency_max: Option<Duration>, pub resize: Option<Size> }   // :43-58

pub(crate) struct Decoded { pub timestamp: Timestamp, pub frame: crate::frame::Frame }              // decode/backend/mod.rs:53-62
```

| Concern | Ours | Theirs | Difference |
| --- | --- | --- | --- |
| Input framing | `MediaPacket { timestamp: Duration, payload: buf_list::BufList, is_keyframe: bool }` (format.rs:389-398), scatter-gather zero-copy, collapsed on demand (format.rs:400-407) | owned `Bytes` plus separate `timestamp: Timestamp` and `keyframe: bool` params | Their comment: owned `Bytes` "so a backend can split codec units without copying" (decode/backend/mod.rs:71). A `BufList` collapses to cross this boundary; in practice moq-mux frames arrive as a single `Bytes`, so the loss is theoretical. |
| Output shape | `pop_frame() -> Option<VideoFrame>`, one at a time, pending held internally | `decode(...) -> Vec<Frame>`, all pictures for the AU | Their Vec directly exposes DPB flush bursts; our `burst_size()` hint becomes unnecessary. |
| Per-picture PTS | timestamp rides on each `VideoFrame` | each `Decoded` carries its own `Timestamp` (decode/backend/mod.rs:54-58, "NVDEC threads timestamps through its parser, so they survive decoder delay and frame reordering") | Equivalent for reordering; theirs is the cleaner surface. |
| Payload prep | pushed into each decoder plus `codec/h264/annexb.rs` helpers | front end owns avc1/hvc1 to Annex-B conversion and parameter-set injection ahead of keyframes (decoder.rs:94-140, `Conversion` enum at decoder.rs:68-76) | Their factoring is cleaner and we would inherit it. |
| First-keyframe gating | in each decoder | in the front end (`got_keyframe`, decoder.rs:155-160) | Theirs centralizes it. |
| Recovery | `reset()` for post-loss hardware reinit (traits.rs:396-398), used at moq-media/src/pipeline/video_decode.rs:333 | none; recovery is rebuild the Consumer | Real gap. Section 7 item 4. |
| Runtime viewport | `set_viewport(w, h)` at runtime | `Config::resize` at construction only, best effort (decoder.rs:52-57) | We keep runtime scaling local. |
| Backend selection | `DecodeConfig { pixel_format, backend: DecoderBackend { Auto, Software } }` (format.rs:917-927) plus probe order in `DynamicVideoDecoder` | `Kind { Auto, Hardware, Software, Named(String) }` (decoder.rs:24-37) plus the candidate table | Their `Hardware` and `Named` are strictly more expressive. |
| Decoded frame type | `VideoFrame` with `Gpu(GpuFrame)` and `NativeFrameHandle` export for the renderer | `decode::Frame { pub timestamp, pub size, pub(crate) inner }` (decode/mod.rs:36-46), escapes only via `resize(Size)` (decode/mod.rs:57-85) and `into_i420() -> Result<Bytes, Error>` (decode/mod.rs:94-101) | No decode-to-render GPU handoff exists in their public API. Section 3. |
| Codec support | H.264, AV1 | H.264, H.265, AV1 (AV1 and H.265 hardware-only; software fallback H.264 only, decode/backend/mod.rs:110-114) | Their `Decoder::new` accepts an AV1 catalog only when 8-bit 4:2:0 (`is_supported_av1`, decoder.rs:187-189). |

### 2.3 Audio encode

Ours, verbatim (rusty-codecs/src/traits.rs:142-192):

```rust
pub trait AudioEncoderFactory: AudioEncoder {
    const ID: &str;
    fn with_config(config: AudioEncoderConfig) -> Result<Self> where Self: Sized;
    fn config_for(config: &AudioEncoderConfig) -> AudioConfig;
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self> where Self: Sized { .. }
}

pub trait AudioEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> { Ok(()) }
}
```

Theirs, verbatim (rs/moq-audio/src/encode/encoder.rs):

```rust
pub struct Encoder { inner: *mut OpusEncoder, config: Config, codec_rate: u32, codec_channels: u32, frame_size: usize, scratch: Vec<u8> }  // :133-143
unsafe impl Send for Encoder {}                                                       // :148

impl Encoder {
    pub fn new(config: &Config) -> Result<Self, Error>;                               // :152-156
    pub fn config(&self) -> &Config;                                                  // :204
    pub fn codec(&self) -> Codec;                                                     // :210
    pub fn codec_rate(&self) -> u32;                                                  // :216
    pub fn codec_channels(&self) -> u32;                                              // :222
    pub fn frame_size(&self) -> usize;                                                // :228
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Bytes, Error>;                    // :237-260
    pub fn catalog(&self) -> hang::catalog::AudioConfig;                              // :263-279
}
```

| Concern | Ours | Theirs | Difference |
| --- | --- | --- | --- |
| Buffering | accumulates any-length input, drains via `pop_packet` | demands exactly one codec frame per call, `Error::Misaligned` otherwise (encoder.rs:238-244) | Theirs moves accumulation, format conversion, and resampling up into `Producer::write` (`pending: Vec<f32>`, rs/moq-audio/src/encode/producer.rs:198-208). Same total functionality, different layer. |
| Output | `pop_packet() -> Option<EncodedFrame>` | `encode(&[f32]) -> Bytes` | Theirs returns owned `Bytes` per frame; PTS is applied later by `Producer`. |
| Input format | interleaved f32 only; device format handled by `AudioStreamFactory` | full WebCodecs `Format` enum via `Config::input` (`Input { format, sample_rate, channels }`, encoder.rs:68-75) | Their `Format` (U8, S16, S32, F32 and planar variants, rs/moq-audio/src/format.rs:16-35) with zero-copy `as_interleaved_f32` (format.rs:65-88) is strictly richer and FFI-oriented. |
| Bitrate | runtime `set_bitrate` (no-op default) | applied only at construction (encoder.rs:182-191); no runtime setter | No runtime audio retune exists upstream. |
| Catalog | static `config_for` | instance `catalog()` returning `hang::catalog::AudioConfig` with OpusHead description (encoder.rs:263-279) | Both produce a catalog config without transport. |
| Codec | Opus and PCM (raw f32) | Opus only, `#[non_exhaustive]` (encode/encoder.rs:28-32) | Our `pcm` codec has no counterpart; their enum is designed to grow one. |

### 2.4 Audio decode

Ours, verbatim (rusty-codecs/src/traits.rs:217-227):

```rust
pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self> where Self: Sized;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}
```

Theirs, verbatim (rs/moq-audio/src/decode/decoder.rs):

```rust
pub struct Decoder { inner: *mut OpusDecoder, sample_rate: u32, channel_count: u32, max_frame_size: usize }  // :60-65
unsafe impl Send for Decoder {}                                                        // :68

impl Decoder {
    pub fn new(catalog: &hang::catalog::AudioConfig) -> Result<Self, Error>;           // :75-104
    pub fn sample_rate(&self) -> u32;                                                  // :107
    pub fn channel_count(&self) -> u32;                                               // :112
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, Error>;               // :117-136
}
```

| Concern | Ours | Theirs | Difference |
| --- | --- | --- | --- |
| Output lifetime | `pop_samples() -> Option<&[f32]>`, borrows the decoder buffer | `decode(&[u8]) -> Vec<f32>`, owned, sized to the 120 ms Opus max then truncated (decoder.rs:118-134) | Our borrow avoids one allocation but pins the decoder between pop and consume and cannot loop while holding results. At 50 packets per second their owned return is the simpler, correct contract. |
| Resampling | inside the decoder to `target_format` | codec-native rate out; `Resampler` plus format conversion in `Consumer` (`decode::Config { format, sample_rate, channels, latency_max }`, decoder.rs:26-46) | Their split keeps the codec pure; ours saves a layer. |
| Construction input | our `AudioConfig` mirror plus `target_format` | `&hang::catalog::AudioConfig` directly | Section 4. |
| PLC / FEC | none | none; `opus_decode_float(..., 0)` always with real data (decoder.rs:129), no `decode_lost` entry point | Our phase-3c FEC/PLC plan needs an API on whichever decoder survives. Flag it so the aligned API reserves a `decode_lost(duration)` or `decode(None)` shape. |

### 2.5 Raw video frame

Ours, verbatim (rusty-codecs/src/format.rs:544-587):

```rust
pub enum FrameData {
    Packed { pixel_format: PixelFormat, data: bytes::Bytes },
    I420 { y: Bytes, u: Bytes, v: Bytes },
    Nv12(Nv12Planes),
    Gpu(GpuFrame),
}

pub struct VideoFrame {
    pub dimensions: [u32; 2],
    pub data: FrameData,
    pub timestamp: Duration,
    cached_rgba: OnceLock<RgbaImage>,   // private
}
```

Theirs, verbatim (the encode-input frame at rs/moq-video/src/frame.rs:23-36; the decoded
frame at rs/moq-video/src/decode/mod.rs:36-46):

```rust
pub(crate) enum Frame {
    #[cfg(target_os = "macos")]                                  Surface(macos::Surface),  // CVPixelBuffer, clone = retain
    #[cfg(target_os = "windows")]                                Texture(d3d11::Texture),  // ID3D11Texture2D + device, clone = AddRef
    #[cfg(all(target_os = "linux", feature = "nvdec"))]          Cuda(cuda::Frame),        // Arc<Buffer> NV12 device memory
    I420(I420),                                                  // Vec<u8>, tightly packed
}

pub struct Frame {                       // decode::Frame
    pub timestamp: Timestamp,
    pub size: Size,
    pub(crate) inner: crate::frame::Frame,
}
```

Full field-by-field comparison is section 3.

### 2.6 Encoded frame and packet

Ours splits producer output from consumer input (rusty-codecs/src/format.rs):

```rust
pub struct EncodedFrame { pub is_keyframe: bool, pub timestamp: Duration, pub payload: bytes::Bytes }  // :409-418
pub struct MediaPacket  { pub timestamp: Duration, pub payload: buf_list::BufList, pub is_keyframe: bool }  // :389-398
```

Theirs has no encoded-frame type on the video encode side. Output is `Vec<Bytes>`, one
Annex-B access unit per element, keyframe implicit in the bitstream, timestamp supplied
later at `Producer::publish(packets: Vec<Bytes>, timestamp: Timestamp)`
(rs/moq-video/src/encode/producer.rs:87-104). On the decode side the tuple `(Bytes,
Timestamp, keyframe)` is three parameters. moq-audio does have a raw-PCM frame type at
the Producer/Consumer boundary (rs/moq-audio/src/frame.rs:11-17):

```rust
pub struct Frame { pub timestamp: Timestamp, pub data: Bytes }
```

The missing timestamp on the video encode side is the crux of section 6.

### 2.7 Catalog / codec config

Ours: a transport-agnostic mirror of the hang catalog types (rusty-codecs/src/config.rs).
`VideoConfig` (config.rs:11-33), `AudioConfig` (config.rs:38-50), `VideoCodec { H264(H264),
AV1(AV1), Other(String) }` (config.rs:53-61), `AudioCodec { Opus, Pcm, Other(String) }`
(config.rs:64-72), with feature-gated `From` conversions to `hang::catalog` types
(config.rs:159-318).

Theirs: no mirror. `Decoder::new` takes `&hang::catalog::VideoConfig` directly
(decode/decoder.rs:94), `Encoder::catalog()` returns `hang::catalog::AudioConfig`
(rs/moq-audio/src/encode/encoder.rs:263), and codec identity for encoding is the small
public `Codec` enum. Detailed drift analysis, including a field rename that our mirror has
not tracked, is section 4.

### 2.8 Device layer (capture, playback)

This layer has no codec-role counterpart on the moq side that iroh-live would adopt, but
it is listed for completeness because it defines what stays local (section 8, D12).

Ours: `VideoSource` (traits.rs:230-241), `PreEncodedVideoSource` (traits.rs:268-287),
`AudioSource` (traits.rs:22-28), `AudioSink` and `AudioSinkHandle` (traits.rs:31-76,
with pause/resume/volume/level metering and a clonable `Box<dyn AudioSinkHandle>` via
`cloned_boxed`), and `AudioStreamFactory` (traits.rs:88-94, async `create_input` and
`create_output` returning `BoxFuture`).

Theirs: capture is a concrete `pub(crate)` `FrameStream` fed by per-platform backends
behind `capture::Config` and `capture::Source` (rs/moq-video/src/capture/mod.rs:59-291),
with public listing helpers `cameras`, `displays`, `windows`, and `apps`
(capture/mod.rs:366-412). The zero-copy `CVPixelBuffer` extraction is
`surface_frame(&CMSampleBuffer) -> Option<Frame>` (rs/moq-video/src/capture/surface.rs:12-20).
Audio capture is a `pub(crate)` cpal-backed `Stream` (rs/moq-audio/src/capture.rs:77-93)
with public `devices()` enumeration. There is no playback or `AudioSink` surface upstream
at all: moq-audio and moq-video are contribution-and-decode libraries, so speaker output,
volume, AEC, and level metering stay entirely on our side.

---

## 3. Frame-model deep dive

Two models sit at the center of the alignment question. Ours is public and open; theirs
is private and closed. The concrete proposal for a shared type lives in the companion
`moq-changes.md`; this section is the field-by-field comparison that motivates it.

### 3.1 CPU representations

| Layout | Ours | Theirs | Note |
| --- | --- | --- | --- |
| Packed RGBA/BGRA | `FrameData::Packed { pixel_format: PixelFormat, data: Bytes }` (format.rs:546-551) | none; RGBA converts to I420 at `encode_rgba` (encoder.rs:195) | Their `frame::Frame` has no packed CPU variant. |
| I420 | `FrameData::I420 { y, u, v }` as three separate `Bytes` (format.rs:553-560) | `I420 { width, height, data: Vec<u8> }`, one tightly packed buffer (rs/moq-video/src/frame.rs:79-85) | Ours keeps planes as independent `Bytes`; theirs is one `Vec<u8>`, Y then U then V. |
| NV12 | `FrameData::Nv12(Nv12Planes)` with per-plane strides (format.rs:562, `Nv12Planes` at format.rs:470-478) | none as a public frame variant; NV12 handled only inside GPU download paths | Their CPU story is I420-only. |

### 3.2 GPU representations

Ours: `FrameData::Gpu(GpuFrame)` where `GpuFrame` wraps `Arc<dyn GpuFrameInner>`
(format.rs:420-424). The trait `GpuFrameInner: Send + Sync + fmt::Debug + 'static`
(format.rs:481-503) exposes `download_rgba`, `gpu_pixel_format`, `dimensions`,
`download_nv12`, and `native_handle`. The native handle is
`NativeFrameHandle` (format.rs:73-87), `#[non_exhaustive]`, with these platform-gated
variants:

- `DmaBuf(DmaBufInfo)` on Linux: `fd: OwnedFd`, `modifier: u64`, `drm_format: u32`,
  coded and display width/height, and `planes: Vec<DmaBufPlaneInfo { offset, pitch }>`
  (format.rs:505-525).
- `HardwareBuffer(HardwareBufferInfo)` on Android: an
  `ndk::hardware_buffer::HardwareBufferRef` plus NV12 plane layout (format.rs:89-109).
- `CvPixelBuffer(CvPixelBufferInfo)` on macOS behind the `apple-gpu` feature: a
  `CFRetained<CVPixelBuffer>` plus width/height/`GpuPixelFormat` (format.rs:116-125).
- A commented-out future `D3D11Texture(D3D11TextureInfo)` on Windows (format.rs:86).

Theirs: the private `frame::Frame` enum (rs/moq-video/src/frame.rs:23-36) with concrete
platform types:

- `Surface(macos::Surface)`: a `CFRetained<CVPixelBuffer>`, "Cloning is a cheap retain
  (no pixel copy)" (frame.rs:351-357), `unsafe impl Send`/`Sync` (frame.rs:368-369).
- `Texture(d3d11::Texture)`: an `ID3D11Texture2D` plus its device and array subresource,
  "Cloning the COM handles is a cheap `AddRef`" (frame.rs:721-729).
- `Cuda(cuda::Frame)`: an `Arc<Buffer>` of NV12 device memory with a row `pitch`,
  `#[derive(Clone)]`, "Cloning bumps refcounts (no pixel copy)" (frame.rs:508-515).
- `I420(I420)`: the CPU fallback.

### 3.3 Visibility, cloning, and the download escape hatch

| Property | Ours | Theirs |
| --- | --- | --- |
| Visibility | `FrameData`, `VideoFrame.data`, `GpuFrame`, `NativeFrameHandle` all `pub` | `frame::Frame` and every variant `pub(crate)`; `decode::Frame.inner` `pub(crate)` |
| Producer extensibility | open: any crate can implement `GpuFrameInner` and construct a `GpuFrame` (format.rs:437) | closed: variants are a fixed set of concrete types, constructible only in-crate |
| Clone cost | refcount bump; `Arc<dyn GpuFrameInner>` clone, plus a fresh `OnceLock` for `cached_rgba` (format.rs:589-598) | refcount bump per variant (retain / AddRef / Arc) |
| CPU download | `GpuFrame::download_rgba() -> Result<RgbaImage>` (format.rs:442) and `download_nv12() -> Option<Result<Nv12Planes>>` (format.rs:457) | `Frame::to_i420() -> Result<Cow<'_, I420>, Error>` (frame.rs:64-74); public `decode::Frame::into_i420() -> Result<Bytes, Error>` (decode/mod.rs:94-101) |
| Native handle export | `native_handle() -> Option<NativeFrameHandle>`, computed on demand so no fd is held per buffered frame (format.rs:461-466, 495-502) | none; no handle leaves the crate |
| Thread-safety guarantee | `GpuFrameInner: Send + Sync` bound | compile-time `assert_send`/`assert_sync` on `decode::Frame` (decode/mod.rs:104-118) |

### 3.4 Handle-variant coverage, stated exactly

Variants ours has that theirs has no counterpart for, as a public or private frame member:

- `DmaBuf` (Linux VAAPI and PipeWire). Theirs has no DMA-BUF frame at all; its Linux GPU
  frame is CUDA only.
- `HardwareBuffer` (Android). Theirs has no Android target anywhere.
- `Packed` RGBA/BGRA and `Nv12` as CPU frame variants. Theirs converts these to I420 at
  the boundary and never carries them as a frame.

Variants theirs has that ours has no counterpart for today:

- `Cuda` (NVDEC output feeding NVENC in device memory). We have no CUDA frame type; our
  Linux GPU frame is DMA-BUF.
- `Texture` (Windows D3D11). We list `D3D11Texture` only as a commented-out future
  variant (format.rs:86).

Variants both sides carry: `CVPixelBuffer` on macOS (ours as `CvPixelBufferInfo`, theirs
as `macos::Surface`), and CPU I420.

The consequence is that a unified frame type has to carry, at minimum: CPU I420, CPU NV12,
CPU packed RGBA/BGRA, DmaBuf, CVPixelBuffer, D3D11 texture, CUDA buffer, and
HardwareBuffer. No existing type on either side spans that set. Their `decode::Frame`
deliberately hides the enum so nothing downstream matches on it; our `VideoFrame` exposes
it because the wgpu renderer matches on `FrameData` and `NativeFrameHandle` to pick its
zero-copy import path. That single divergence, whether the GPU frame variant set is public,
is the crux of D1.

---

## 4. Config and catalog types

### 4.1 Our mirror versus hang 0.19.5

rusty-codecs mirrors the hang catalog types so the crate needs no hang dependency
(config.rs:1-5). The mirror has drifted from the current hang, which is version 0.19.5
(rs/hang/Cargo.toml:8). Concrete divergences, verified against the current hang source:

- Field rename. hang's `VideoConfig` now names the display-aspect fields
  `display_aspect_width` and `display_aspect_height`, with a `#[serde(alias =
  "displayRatioWidth")]` / `"displayRatioHeight"` for catalogs from publishers predating
  the rename (rs/hang/src/catalog/video/mod.rs:133-138). Our mirror still uses
  `display_ratio_width` and `display_ratio_height` (config.rs:24-26), and our interop
  `From` impls assign `config.display_ratio_width = c.display_ratio_width`
  (config.rs:171, 186). Those impls no longer compile against hang 0.19.5, because the
  destination fields were renamed. The mirror is not merely redundant; it is already
  broken against the current hang.
- Missing codecs. hang's `VideoCodec` is `H264(H264)`, `H265(H265)`, `VP9(VP9)`,
  `AV1(AV1)`, `Unknown(String)` (rs/hang/src/catalog/video/codec.rs:13-29). Our mirror is
  `H264(H264)`, `AV1(AV1)`, `Other(String)` (config.rs:53-61), so H.265 and VP9 collapse
  into `Other`.
- Missing fields. hang's `VideoConfig` also carries `broadcast:
  Option<PathRelativeOwned>`, `container: Container`, `jitter: Option<Duration>`, and
  `timeline: Option<Timeline>` (rs/hang/src/catalog/video/mod.rs:98-178). Our mirror
  carries none of these, and its `From` impl hard-codes `config.container =
  Default::default()` and `config.jitter = None` (config.rs:190-191).

### 4.2 Adopting hang directly

Both moq codec crates depend on hang and pass its catalog types across the codec boundary
unchanged (`Decoder::new(&hang::catalog::VideoConfig)`, decode/decoder.rs:94;
`Encoder::catalog() -> hang::catalog::AudioConfig`, rs/moq-audio/src/encode/encoder.rs:263).
The mirror existed only to keep rusty-codecs hang-free. The moment the crates align on
hang as a shared dependency, the mirror is pure duplication plus the drift shown above,
and its entire `hang_interop` module (config.rs:159-318) is dead weight. This is D5.

### 4.3 Timestamp adoption

Adopting `moq_net::Timestamp` (section 2.0) end to end in iroh-live removes a `Duration`
to `Timestamp` conversion at every point where our pipeline meets the hang container or
their Producer/Consumer. Their Producer already stamps with it
(`Timestamp::from_micros(clock.micros())`, encode/producer.rs:386), and hang frames carry
it. Keeping `Duration` in our frame and packet types means paying that conversion forever.
This is D4.

---

## 5. The extension-point problem

Their `Backend` traits are `pub(crate)`. Everything iroh-live ships that moq-video does
not have must therefore move in-tree upstream or wait for a public extension point. The
concrete inventory, with what each backend needs from the moq API:

1. V4L2 M2M encoder and decoder (Linux ARM SoCs, tested on Pi bcm2835-codec; encoder on
   raw ioctls, decoder on `v4l2r`). Upstream has no Linux hardware encode besides NVENC
   and an unvalidated VAAPI (encode/backend/mod.rs:81-92), and no Linux decode besides
   NVDEC (decode/backend/mod.rs:102-108). Admitting it needs two `Candidate` entries
   behind a `v4l2` feature plus the timestamp-through-encode change (section 6): the M2M
   queue model cannot honestly implement `encode -> Vec<Bytes>` without either a full
   device round trip per frame or a mis-stamped PTS.
2. Android MediaCodec encoder and both decoders (ByteBuffer and ImageReader
   HardwareBuffer). Upstream has zero Android support in any crate. Needs an android
   target in both candidate tables, the `ndk` dependency, the same timestamp change
   (MediaCodec is dequeue-based and asynchronous like V4L2), and a `HardwareBuffer` frame
   variant for the zero-copy decoder (section 3.4).
3. VAAPI decoder (cros-codecs, Intel/AMD, produces DMA-BUF GPU frames). Upstream VAAPI is
   encode-only and gated behind a `vaapi` feature with only H.264
   (encode/backend/mod.rs:87-92); Linux hardware decode upstream is NVDEC-only. Needs a
   decode `Candidate` and a DmaBuf frame variant if the output is to stay on the GPU,
   otherwise it degrades to an I420 download and loses our render zero-copy path.
4. AV1 software codec (rav1e encode, rav1d decode). Upstream has no AV1 encoder and AV1
   decode only on NVDEC. Needs `Codec::Av1` in the public encode enum (non-breaking, it is
   `#[non_exhaustive]`), a software candidate on both tables, and feature flags for the
   heavy dependencies.
5. PCM audio codec. moq-audio has no trait and an Opus-only `Codec`
   (rs/moq-audio/src/encode/encoder.rs:28-32). Needs `Codec::Pcm` plus match arms in
   `Encoder` and `Decoder`, roughly a hundred lines; their enum is `#[non_exhaustive]`
   for exactly this.
6. GPU-frame output for rendering (DmaBuf into wgpu/Vulkan, CVPixelBuffer into Metal,
   HardwareBuffer into GLES). Not a backend but the consumer side of the frame model;
   upstream has no decode-to-render GPU handoff. Needs the frame-model decision (D1), not
   a Backend change.
7. Pre-encoded sources (`LibcameraH264Source`, rpicam-vid). Needs nothing structural:
   `Producer::publish(Vec<Bytes>, Timestamp)` (encode/producer.rs:87) already accepts
   externally produced Annex-B access units without an `Encoder`, a clean fit for our
   `PreEncodedVideoSource` output (traits.rs:268-287).

The three realistic postures (upstream each backend in-tree with `Backend` private; make
`Backend` public or sealed; or add a public registration API over the existing candidate
table) plus their trade-offs and the recommended split are worked out in the companion
`moq-changes.md`. The one load-bearing observation to record here is that the candidate
table is already data-driven and internal (encode/backend/mod.rs:60-102), so a
registration mechanism is small in principle, but it is only useful if the frame type
external backends receive and produce is public, which makes any registration proposal
contingent on D1.

---

## 6. Streaming versus one-shot

The question is whether adopting their one-shot calls (`encode(&Frame, keyframe) ->
Vec<Bytes>`, `decode(Bytes, ts, keyframe) -> Vec<Decoded>`) loses capability relative to
our push/pop model. The answer, from the sources, splits by direction.

Decode: no loss. Their `Vec<Decoded>` with a per-picture `Timestamp` handles everything
our push/pop handles. A DPB flush burst is a longer Vec; reordering is covered because
each `Decoded` carries its own timestamp (decode/backend/mod.rs:54-58); and `Consumer`
buffers the excess in a `pending: VecDeque<Frame>` to keep `read` one-frame-per-call
(rs/moq-video/src/decode/consumer.rs:20-73). Our `burst_size()` hint (traits.rs:401-409)
becomes redundant since the burst is directly observable as the Vec length. Our decoders'
internal pending queues map mechanically onto returning the Vec instead.

Encode: loss, precisely identified, and fixable with one signature change. Their
`Backend::encode` takes no timestamp (encode/backend/mod.rs:40). PTS is applied afterward
by the capture loop, which reads a single timestamp before the encode
(`let ts = Timestamp::from_micros(clock.micros())?;`, encode/producer.rs:386) and stamps
every packet from that frame with it (`producer.publish(packets, ts)?;`,
encode/producer.rs:392). That is correct only if the encoder has zero frame delay, and
their backends are all driven to guarantee exactly that: NVENC without B-frames drained
synchronously, VideoToolbox completed per encode, Media Foundation driven to completion on
its dedicated thread (the threaded `Sink`, rs/moq-video/src/encode/sink.rs:24-164), and
openh264 inherently one-in one-out.

Our stack contains an encoder for which that assumption is false by construction. The
V4L2 M2M encoder is a hardware queue. `push_frame` converts to NV12 and sends
`EncoderCmd::Encode { nv12, timestamp_us }` over a bounded `SyncSender`
(rusty-codecs/src/codec/v4l2/encoder.rs:267, channel created at encoder.rs:69 with
capacity 4), then returns immediately. `pop_packet` drains completed CAPTURE buffers from
a `packet_buf: VecDeque<EncodedFrame>` (encoder.rs:274-276, buffer at encoder.rs:42,
filled at encoder.rs:119). Bitstream for frame N routinely surfaces during frame N+k, and
the PTS survives because it rides the V4L2 buffer timestamp and returns on the dequeued
buffer (`buf.timestamp_sec * 1_000_000 + buf.timestamp_usec`, encoder.rs:718; set at
encoder.rs:974-975). Android MediaCodec has the same dequeue-based shape. Under their API,
such a backend must either block a full device round trip per `encode` call (adding a
frame or more of latency and stalling the capture loop) or return frame N-k's bitstream to
be mis-stamped with frame N's PTS.

B-frames are not the argument. Nothing on either side emits them today: their NVENC sets
no B-frames, their GOP is IPPP, and our encoders configure low-latency H.264. The
queue-based device model is the argument, and it stands on its own.

The fix is small and precedented by their own decode side: thread the timestamp through
encode and return it with each packet, for example `encode(&Frame, timestamp: Timestamp,
keyframe: bool) -> Result<Vec<Packet>, Error>` with `Packet { payload: Bytes, timestamp:
Timestamp }` (or reuse the audio-style `frame::Frame`). `Producer::publish` then uses the
returned per-packet timestamps instead of the call-site one. Because `Vec` already permits
returning zero packets now and more later, this one change makes the one-shot shape
capability-equivalent to push/pop for pipelined encoders, and it changes no existing
backend's observable behavior: they all return the current frame's packets and echo its
timestamp. Verified against the current source, the change is still exactly one signature
plus the `Producer` call site. This is D3, and it is a precondition for upstreaming the
V4L2 and Android encoders.

---

## 7. Track-facing layer

Their layer: `encode::Producer` wraps a per-codec moq-mux splitter and importer with a
catalog reservation, exposes `demand()` for on-demand capture, and takes already-encoded
packets (`publish(Vec<Bytes>, Timestamp)`, rs/moq-video/src/encode/producer.rs:43-127).
`decode::Consumer` bundles a `Decoder`, a `moq_mux::container::Consumer<legacy::Wire>`
subscribed at video priority with `latency_max` applied through `with_latency`, and a
pending queue; `read()` is async pull (rs/moq-video/src/decode/consumer.rs:15-73). Ours:
OS-thread pipelines bridged to async via bounded channels, delivering into a latest-wins
frame channel, with the transport seam behind `MoqPacketSink`/`MoqPacketSource`
(moq-media/src/transport.rs:16, 24, 34, 89).

Publish side. Their `Producer` can replace our `MoqPacketSink` plus the moq-mux plumbing
nearly one for one, and its packets-in design serves both our encoder pipelines and our
`PreEncodedVideoSource` passthrough. Two gaps. First, the video `Producer` derives its
track name internally (`unique_track(&mut broadcast, ".avc3")`, producer.rs:58) with no
override, while the audio `Options` already exposes `track: Option<String>`
(rs/moq-audio/src/encode/producer.rs:29); our simulcast rendition registry needs
deterministic per-rendition names, so it wants the audio treatment on the video side.
Second, their rendition appears in the catalog only after the first SPS (section 1.3),
against our register-up-front model. Both are small upstream asks (D9).

Subscribe side, against our adaptive machinery. The exact integration points, checked
against the current source:

1. Open a second Consumer on another rendition and swap. Supported: `Consumer::new(&broadcast,
   &VideoConfig, name, Config)` is per track name (consumer.rs:27-50), so the staging probe
   (subscribe to a higher rendition in parallel, commit on first decoded frame) maps to
   constructing a second Consumer and swapping which one our task reads into the shared
   frame channel. Our `FrameSender` swap sits above the consumer and survives unchanged.
2. Access to decode timestamps. Supported: `decode::Frame.timestamp` is public
   (decode/mod.rs:37-40); our sync gating and stats keep working.
3. Runtime `latency_max`. Not supported at runtime. `Config.latency_max` is forwarded once
   to `with_latency` at construction, verified verbatim (consumer.rs:41-43):

   ```rust
   if let Some(latency) = config.latency_max {
       track = track.with_latency(latency);
   }
   ```

   There is no runtime setter on `Consumer` or the underlying container consumer, and our
   `PlayoutMode::Auto { min, max }` adjusts the skip threshold live. Upstream ask: a
   `set_latency(Duration)` on the moq-mux container consumer, surfaced through
   `decode::Consumer`. Without it, Auto playout degrades to rebuild-on-change, which drops
   group position. This is D6.
4. Decoder reset after loss. Not supported: there is no `reset` on `Decoder` or `Consumer`;
   recovery is rebuild-the-Consumer, which resubscribes the track. Our decode loop resets
   in place and waits for a keyframe (`decoder.reset()?`, moq-media/src/pipeline/video_decode.rs:333).
   This is D10.
5. Runtime viewport. Not supported: `Config::resize` is construction-time (decoder.rs:52-57).
   Acceptable to keep local: our renderer already scales.
6. Forced IDR on switch. Supported on their encode side (`keyframe: bool`), missing on
   ours (D8); irrelevant to the subscribe side, since group boundaries carry the keyframe.

Threading model. Their `Consumer::read` is async and their decode backends run inline in
it (synchronous by design). Our convention is dedicated OS decoder threads with a blocking
condvar sync. These do not compose directly: adopting `Consumer` wholesale would move
decode into tokio tasks and put multi-millisecond software decodes and a blocking condvar
on runtime workers. The escape hatch is theirs by design, the sans-IO `Decoder` under the
Consumer (decode/decoder.rs:80-86, quoted in section 1.3). The realistic integration is
therefore to keep moq-media's OS-thread pipelines, frame channel, sync, and adaptation
task, and replace their internals (our `VideoDecoder` trait objects and H.264 prep code)
with `moq_video::decode::Decoder`, while `MoqPacketSource` keeps feeding it. Adopting the
full `Consumer` becomes attractive only if we also move playout gating to an async model,
which is out of scope for this alignment.

---

## 8. Decision list

Priority-ordered. Each item gives the question, the options, our recommendation, and an
estimated diff size (S under ~150 lines, M ~150-600, L larger). Items 1 through 3 gate
the rest. Items marked "upstream ask" require moq buy-in; items marked "local" we can
decide alone. The concrete upstream proposals for the upstream-ask items are drafted in
`moq-changes.md`.

D1. Public frame model (upstream ask; gates D2, the render integration, and the VAAPI and
Android backends). Question: does a public raw-frame type with GPU handle variants (DmaBuf,
CVPixelBuffer, D3D11, CUDA, HardwareBuffer) plus CPU planes exist, and where? Options: (i)
stay private, which blocks VAAPI decode output, Android zero-copy, and any decode-to-render
handoff; (ii) a public module inside moq-video; (iii) a small `moq-frame` crate. Section 3
shows no existing type spans the required variant set. Recommendation: (iii) falling back
to (ii); a closed `#[non_exhaustive]` enum of concrete handles, not an open trait object.
Diff: L.

D2. Backend extension mechanism (upstream ask). Question: how do non-upstream codec
backends plug in, given `Backend` is `pub(crate)`? Options: (a) in-tree behind features
with `Backend` private; (b) public `Backend` trait; (c) a registration API over the
existing candidate table with a public minimal contract. Recommendation: (a) for the
broadly useful pieces (V4L2 encode and decode, VAAPI decode, AV1 software, PCM) and (c)
for Android and future out-of-tree backends, contingent on D1; not (b). Diff: per backend
M-L upstream; (c) itself S upstream.

D3. Timestamp through encode (upstream ask; precondition for the V4L2 and Android
encoders). Question: does encode carry PTS per input and return it per packet? Options: (i)
keep `encode(&Frame, keyframe) -> Vec<Bytes>` and require zero-delay backends, which blocks
queue-based devices or mis-stamps PTS (evidence in section 6); (ii) `encode(&Frame,
timestamp, keyframe) -> Vec<Packet>` with per-packet timestamps, `Producer` using them.
Recommendation: (ii); it mirrors what their decode side already does and changes no
existing backend's observable behavior. Diff: S-M, mechanical across the backends plus the
`Producer` call site.

D4. Timestamp type in iroh-live (local). Question: keep `Duration` in our frame and packet
types, or adopt `moq_net::Timestamp` end to end? Recommendation: adopt `Timestamp`; it is
the wire type on the hang container and their Producer/Consumer boundary, and every
integration point otherwise pays a conversion (section 4.3). Diff: M, mechanical but wide
(format.rs, all pipelines, sync).

D5. Drop the catalog-config mirror (local; small upstream implication). Question: keep
rusty-codecs `config::{VideoConfig, AudioConfig, VideoCodec, AudioCodec, H264, AV1}`, or use
`hang::catalog` types directly as both moq crates do? The mirror is already broken against
hang 0.19.5 (the `displayRatio` to `displayAspect` rename, section 4.1) and misses H.265,
VP9, and four hang fields. Recommendation: adopt hang types directly and delete the mirror
plus its `From` layer; the mirror's only purpose was avoiding the hang dependency, which
alignment makes moot. Diff: M.

D6. Runtime latency control on the consumer path (upstream ask). Question: can `latency_max`
change after construction? It is forwarded once via `with_latency` (consumer.rs:41-43) with
no runtime setter; our PlayoutMode Auto requires a live one. Options: (i) rebuild the
Consumer on change, losing group position; (ii) add `set_latency` to the moq-mux container
consumer and surface it through `decode::Consumer`. Recommendation: (ii). Diff: S upstream.

D7. set_bitrate contract and error type (local; align to theirs). Question: adopt their
strict contract (no silent no-op, `BitrateUnsupported`, structured `#[non_exhaustive]`
`Error`) in place of our `anyhow` plus default-`Ok`? Their default no-op (traits.rs:352-354)
is a footgun for the phase-3d rate-control work, and their `rate::Control` policy
(rs/moq-video/src/encode/rate.rs:86-160) is worth adopting alongside it. Recommendation:
yes. Diff: M.

D8. Forced keyframe on our encode path (local; partly moot if D2 and D3 land). Question:
add a per-frame force-IDR to `VideoEncoder`, or adopt their `keyframe: bool`? Demand-driven
reopen and rendition switches both need it, and every hardware API supports it (V4L2
`V4L2_CID_MPEG_VIDEO_FORCE_KEY_FRAME`, the VideoToolbox and Media Foundation properties,
NVENC FORCEIDR). Recommendation: yes. Diff: S.

D9. Video Producer track naming and simulcast (upstream ask). Question: can a caller name
the video track, as the audio `Options.track` already allows (rs/moq-audio/src/encode/producer.rs:29),
so simulcast renditions get deterministic names, and can the rendition register before the
first SPS? Recommendation: ask for a `track: Option<String>` on a video `Options` struct;
register-before-SPS is negotiable, since their late registration is deliberate and tied to
SPS-derived catalog fields. Diff: S upstream.

D10. Decoder loss recovery (upstream ask; low urgency). Question: reset-in-place versus
rebuild-the-Consumer after a fatal decode error? Their only path today is rebuild, which
resubscribes; ours resets in place and waits for a keyframe (video_decode.rs:333).
Recommendation: raise it, accept rebuild if they prefer, but measure the resubscribe cost
on a live stream first. Diff: S-M upstream.

D11. Audio: accept the no-trait design plus PCM upstream (local plus small upstream ask).
Question: keep our `AudioEncoder`/`AudioDecoder` traits, or accept their concrete
`Encoder`/`Decoder` with enum dispatch, contributing `Codec::Pcm`? Recommendation: accept
theirs; two codecs do not justify a trait, their Producer-owned accumulation and resampling
is a cleaner split than our in-decoder resample, and their WebCodecs `Format` enum is richer
than our f32-only surface. Our `pop_samples() -> Option<&[f32]>` borrow is not worth
defending against their owned `Vec<f32>`. Flag the missing PLC/FEC entry point (phase-3c) so
the aligned API reserves a `decode_lost` shape. Diff: S upstream (PCM), M local (audio
pipeline rewiring).

D12. What explicitly stays local (a decision not to upstream). The moq-media layer above the
codec seam: the latest-wins frame channel, the playout clock and sync, `adaptive.rs` and the
adaptation task, the cpal-plus-AEC audio backend, the `AudioSink`/`AudioSinkHandle`/
`AudioStreamFactory`/`AudioSource` device traits (traits.rs:22-94; moq has no playback
surface at all, section 2.8), `SharedVideoSource` fan-out, the wgpu and GLES renderers, and
runtime viewport scaling. None of these have upstream counterparts or need upstream API
changes beyond D1 and D6. Diff: none; this is the boundary statement for the meeting.

Reading order for the meeting: D1 and D2 are the strategic questions (the frame model and
the extension mechanism), D3 is the technical precondition that must be settled before any
backend contribution, and D4, D5, D7, and D11 are the local convergence work that can begin
as soon as the direction is agreed.
