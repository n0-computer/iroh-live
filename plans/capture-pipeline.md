# Capture and Encoding Pipeline: API Review and Future Design

## 1. Current State Assessment

### What works

The pipeline has a clean separation of concerns across three crates. `rusty-codecs` defines the trait boundary (`VideoSource`, `VideoEncoder`, `PreEncodedVideoSource`), `moq-media` owns the pipeline orchestration (`VideoEncoderPipeline`, `PreEncodedVideoPipeline`, `VideoPublisher`), and `rusty-capture` provides platform-specific capture implementations behind `VideoSource`.

The `VideoSource` trait is deliberately synchronous and polling-based (`pop_frame() -> Result<Option<VideoFrame>>`), which suits the OS-thread model well. Encoder pipelines run on dedicated OS threads rather than tokio tasks, avoiding async overhead in tight encode loops. The `SharedVideoSource` wrapper handles fan-out to multiple simulcast encoders from a single capture source, with reference-counted start/stop gating.

The recently added `PreEncodedVideoSource` and `PreEncodedVideoPipeline` solve the Raspberry Pi problem cleanly: rpicam-vid's internal hardware encoder produces H.264 directly, and piping raw YUV through a second encode step wastes 30x the bandwidth for zero quality gain.

The `PacketSink` trait at the pipeline output boundary is transport-agnostic. `MoqPacketSink` bridges to the MoQ transport, and `PipeSink` enables local encode-to-decode loops for testing and preview.

### What is awkward

**Two separate publish paths with no shared abstraction.** `VideoPublisher` exposes `set()` for raw sources and `set_pre_encoded()` for pre-encoded sources. These share no common type. The `State` struct tracks them in two separate fields (`available_video` and `pre_encoded_video`), and `start_track()` has branching logic to check both. Adding a third variant (say, pre-encoded audio, or a source that switches modes dynamically) would add another branch and another field.

**`PreEncodedVideoSource` requires `Clone`.** The `set_pre_encoded()` signature requires `S: PreEncodedVideoSource + Clone`, because the factory closure needs to create new instances per subscriber. But the Pi implementation separates `LibcameraH264Source` (cloneable config) from the runtime state (non-cloneable). This split is necessary but clunky. The `VideoSource` path avoids this because `SharedVideoSource` handles fan-out centrally.

**No simulcast for pre-encoded sources.** `VideoRenditions` supports multiple encoder presets sharing one `VideoSource`. `PreEncodedVideoEntry` supports exactly one track. If a hardware encoder could produce multiple quality layers (some RTSP cameras can), there is no way to express that.

**Config must be known before start.** `PreEncodedVideoSource::config()` is documented as "called once after start() succeeds," but the catalog needs the config to advertise the track. The Pi implementation works around this by extracting SPS/PPS from the first keyframe during `pop_packet()`, then returning it from `config()` — but `set_pre_encoded()` takes `config` as a parameter at registration time, before the source is started. This means the caller must guess or hardcode the config.

**No backpressure signaling from encoders to sources.** If the encoder is slower than the source (common with software AV1), frames are silently dropped by the `watch::Receiver` in `SharedVideoSource`. This works but is invisible — there is no metric for how many frames were skipped due to encoder backpressure.

### What is missing

- **No `PreEncodedAudioSource` equivalent.** If a hardware device outputs encoded Opus or AAC directly, there is no way to publish it without decoding first.
- **No runtime codec parameter changes on pre-encoded sources.** `VideoEncoder` has `set_bitrate()`, but `PreEncodedVideoSource` has no equivalent.
- **No way to request a keyframe from a pre-encoded source.** When a new subscriber joins, the transport waits for the next natural keyframe. An encoder can be forced to emit a keyframe; a pre-encoded source cannot (without out-of-band signaling to the capture process).
- **No processing stage between capture and encode.** Overlays, watermarks, background blur, composition — all require inserting a transform between `VideoSource` and `VideoEncoder`. The current pipeline has no hook for this.
- **No metadata channel alongside the media pipeline.** Timestamps come from the source, but there is no way to attach per-frame metadata (rotation, exposure info, face detection boxes).

## 2. Industry Comparison

### GStreamer

GStreamer's core abstraction is the **element graph**: a DAG of elements connected by **pads** with **capabilities** (caps). Pipeline negotiation happens by intersecting caps between connected pads.

A `v4l2src` element can produce either raw YUV or H.264 (if the device supports it), and the downstream element's caps determine which format is actually used. There is no separate "pre-encoded source" concept — it is just a source element whose output caps happen to be `video/x-h264`.

**Strengths:** Maximally flexible. Inserting a processing stage is just linking in another element. **Weaknesses:** Enormously complex. Building a simple capture-to-network pipeline requires 50+ lines of boilerplate. Overkill for a focused real-time streaming library.

### libwebrtc

Uses a **track-based model** with internal adaptation. `VideoTrackSource` produces frames, `VideoStreamEncoder` handles encoding, `VideoSendStream` manages RTP output. Simulcast is built into the send stream.

Strongly assumes raw frames. No first-class path for injecting pre-encoded frames into a send stream.

**Strengths:** Deep adaptation integration (bitrate allocation, resolution scaling, frame dropping). **Weaknesses:** Monolithic, hard to use outside Chrome. Pre-encoded sources are second-class citizens.

### OBS Studio

**Source/filter/output** model. Sources produce raw frames (or occasionally encoded frames for file playback). Filters transform frames. Outputs encode and send.

**Strengths:** Powerful composition model (scenes, sources, filters). **Weaknesses:** Audio and video pipelines are separate. C-based plugin API.

### LiveKit

Added `EncodedVideoSource` for injecting already-encoded frames from hardware encoders or file demuxers. This is a separate publish path, similar to iroh-live's current approach.

**Strengths:** Simple API for common cases. **Weaknesses:** Pre-encoded sources are a bolt-on, not integrated into the core model.

### FFmpeg

**Demuxer/decoder/filter/encoder/muxer** pipeline. Core abstraction is `AVPacket` (encoded) and `AVFrame` (raw). "Stream copy" (`-c copy`) passes packets directly from demuxer to muxer without decoding — the pre-encoded passthrough pattern as a first-class operation.

**Strengths:** The `AVPacket`/`AVFrame` split makes encoded-vs-raw explicit at the type level. Stream copy is natural. **Weaknesses:** C API is error-prone. Not designed for real-time interactive use.

### Key takeaway

FFmpeg and GStreamer have a clean type-level distinction between raw and encoded media. WebRTC and LiveKit treat pre-encoded as a bolt-on. iroh-live currently falls into the bolt-on camp. The question is whether to move toward the type-level distinction camp, and how far.

## 3. Design Alternatives

### Alternative A: Minimal — Keep current + polish

Keep `VideoSource` and `PreEncodedVideoSource` as separate traits. Unify the internal storage in `State` with an enum, add `PreEncodedAudioSource`, clean up the `Clone` requirement.

```rust
enum VideoSlot {
    Renditions(VideoRenditions),
    PreEncoded(PreEncodedVideoEntry),
}

impl VideoPublisher<'_> {
    pub fn set(&self, source: impl VideoSource, codec: VideoCodec,
               presets: impl IntoIterator<Item = VideoPreset>) -> Result<()>;
    pub fn set_pre_encoded(&self, source: ..., track_name: ...,
                           config: VideoConfig) -> Result<()>;
    pub fn clear(&self);
}
```

**Pros:** Smallest change. Each trait is simple and single-purpose. Easy to understand.
**Cons:** Two publish paths remain permanently. Adding future variants adds more branches. No processing stage hook.

### Alternative B: Unified `VideoInput` enum

Introduce a `VideoInput` enum that wraps either a raw source + encoder config or a pre-encoded source. Single `set()` method on `VideoPublisher`.

```rust
pub enum VideoInput {
    Raw {
        source: Box<dyn VideoSource>,
        codec: VideoCodec,
        presets: Vec<VideoPreset>,
    },
    PreEncoded(Vec<PreEncodedTrack>),
}

pub struct PreEncodedTrack {
    pub name: String,
    pub config: VideoConfig,
    pub factory: Box<dyn Fn() -> Result<Box<dyn PreEncodedVideoSource>> + Send>,
}

impl VideoPublisher<'_> {
    pub fn set(&self, input: VideoInput) -> Result<()>;
}

// Usage:
broadcast.video().set(VideoInput::raw(camera, H264, P720))?;
broadcast.video().set(VideoInput::pre_encoded("video/h264-pi", config, factory))?;
```

**Pros:** Single entry point. Multi-rendition pre-encoded sources natural. No `Clone` requirement. Extensible with future variants (SVC).
**Cons:** Slightly more verbose for the simple case. Internal implementation still branches.

### Alternative C: Pipeline builder with typed stages

Model the pipeline as a chain of typed stages, inspired by FFmpeg's packet/frame distinction. Adds a `VideoProcessor` trait for processing stages.

```rust
pub trait VideoProcessor: Send + 'static {
    fn name(&self) -> &str;
    fn process(&mut self, frame: VideoFrame) -> Result<VideoFrame>;
}

let spec = VideoPipelineBuilder::from_source(camera)
    .process(WatermarkOverlay::new("iroh.computer"))
    .encode_simulcast(H264, [P360, P720]);
broadcast.video().set(spec)?;

let spec = VideoPipelineBuilder::from_pre_encoded(rpicam_source)
    .passthrough("video/h264-pi", config);
broadcast.video().set(spec)?;
```

**Pros:** Processing stages are first-class. Explicit and readable pipeline structure. Extensible.
**Cons:** More API surface. Risk of over-engineering for features with no current users.

## 4. Recommendation

**Alternative B (Unified `VideoInput` enum)**, with elements from Alternative C deferred.

Alternative A is too conservative — two separate paths with no shared type will not scale. Alternative C is premature — the processing pipeline has zero current users and designing it in the dark risks getting the abstraction wrong.

Alternative B unifies the two paths behind a single entry point, supports multi-rendition pre-encoded sources, eliminates the `Clone` requirement, and is a small change that does not break existing code.

### Deferred work (phase 2, when needed)

- `VideoProcessor` trait and builder pattern from Alternative C, when the first processing use case arrives.
- `PreEncodedAudioSource` and `AudioInput` enum, mirroring the video pattern.
- Keyframe request signaling for pre-encoded sources.
- Config-after-start for pre-encoded sources (allow `config()` to return `None` initially and update via a watcher after the first keyframe).
