# rusty-codecs - Code Map

> Campaign: upstream | Kind: map | Read ../../0-overview.md first; index at ../0-index.md.

Owned native encode/decode implementations for iroh-live. **No iroh, no moq, no transport.**
Codec configuration types "mirror the catalog types from `hang` but are transport-agnostic:
no serde, no container format, no jitter fields" (`config.rs:2-5`). A feature-gated `hang`
interop module converts to/from `hang::catalog` types (`config.rs:159-318`).

Crate root re-exports only these modules (`lib.rs:1-8`):

```rust
pub mod codec;
pub mod config;
pub mod format;
pub mod processing;
#[cfg(any(feature = "wgpu", feature = "gles"))]
pub mod render;
pub mod test_sources;
pub mod traits;
```

There is **no `pub use` flattening at the crate root** - consumers import via
`rusty_codecs::traits::*`, `rusty_codecs::format::*`, `rusty_codecs::codec::*`, etc.

---

## 1. Core Trait System

All traits live in `traits.rs`. They form a **push/pop streaming model**: feed raw input,
drain encoded output (and vice versa). Boxed forwarding impls (`impl Trait for Box<dyn Trait>`)
exist for the object-safe traits so they can be used as `Box<dyn _>`.

### VideoEncoder / VideoEncoderFactory (`traits.rs:311-377`)

```rust
pub trait VideoEncoderFactory: VideoEncoder {
    const ID: &str;                                    // e.g. "h264-openh264"
    fn with_config(config: VideoEncoderConfig) -> Result<Self> where Self: Sized;
    fn config_for(config: &VideoEncoderConfig) -> VideoConfig;
    fn with_preset(preset: VideoPreset) -> Result<Self> where Self: Sized { .. }
}

pub trait VideoEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> VideoConfig;                   // catalog config of encoded stream
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> { Ok(()) }   // default no-op
}
```

### VideoDecoder (`traits.rs:379-410`)

```rust
pub trait VideoDecoder: Send + 'static {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self> where Self: Sized;
    fn name(&self) -> &str;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn reset(&mut self) -> Result<()> { Ok(()) }        // HW decoders reinit after loss
    fn set_viewport(&mut self, w: u32, h: u32);         // optional downscale target
    fn burst_size(&self) -> usize { 0 }                 // HW DPB flush burst size
}
```

Note the **asymmetry**: `VideoDecoder::new` takes a `&VideoConfig` (catalog) directly (no
factory trait), whereas encoders go through `VideoEncoderFactory::with_config`.

### AudioEncoder / AudioEncoderFactory (`traits.rs:138-214`)

```rust
pub trait AudioEncoderFactory: AudioEncoder {
    const ID: &str;                                    // e.g. "opus"
    fn with_config(config: AudioEncoderConfig) -> Result<Self> where Self: Sized;
    fn config_for(config: &AudioEncoderConfig) -> AudioConfig;
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self> where Self: Sized { .. }
}

pub trait AudioEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;   // interleaved f32 PCM
    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>>;
    fn set_bitrate(&mut self, _bitrate: u64) -> Result<()> { Ok(()) }
}
```

### AudioDecoder (`traits.rs:216-227`)

```rust
pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self> where Self: Sized;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;         // borrows internal buffer
}
```

### Source / Sink / Factory traits

- `Decoders` (`traits.rs:12-19`) - associates an `Audio: AudioDecoder` + `Video: VideoDecoder`
  pair for the dynamic pipeline. Implemented by `DefaultDecoders`.
- `AudioSource` (`traits.rs:21-28`) - `format()`, `pop_samples(&mut [f32]) -> Option<usize>`.
- `AudioSink` + `AudioSinkHandle` (`traits.rs:30-121`) - playback with thread-safe pause/
  resume/volume/level-metering handle. `AudioSinkHandle: Send + Sync`, and
  `Box<dyn AudioSinkHandle>: Clone` via `cloned_boxed` (`traits.rs:78-82`).
- `AudioStreamFactory` (`traits.rs:84-94`) - async `create_input`/`create_output` returning
  boxed source/sink (`BoxFuture`).
- `VideoSource` (`traits.rs:229-259`) - `pop_frame() -> Option<VideoFrame>`, plus start/stop.
- `PreEncodedVideoSource` (`traits.rs:261-305`) - yields `EncodedFrame`s directly (HW capture
  that encodes internally, e.g. `rpicam-vid`, RTSP cameras, file demuxers).

### Data model - raw frame

`VideoFrame` (`format.rs:568-811`) unifies CPU and GPU frames:

```rust
pub struct VideoFrame {
    pub dimensions: [u32; 2],
    pub data: FrameData,
    pub timestamp: Duration,           // Duration::ZERO before encoder assigns PTS
    cached_rgba: OnceLock<RgbaImage>,  // lazy RGBA cache for render/legacy accessors
}

pub enum FrameData {                   // format.rs:544-566
    Packed { pixel_format: PixelFormat, data: bytes::Bytes },   // RGBA or BGRA
    I420 { y: Bytes, u: Bytes, v: Bytes },                      // planar YUV 4:2:0
    Nv12(Nv12Planes),                                          // semi-planar
    Gpu(GpuFrame),                                             // HW-resident
}
```

- `PixelFormat` (`format.rs:49-57`): `Rgba` | `Bgra` (CPU byte order only).
- `Nv12Planes` (`format.rs:469-478`): `y_data`/`y_stride`/`uv_data`/`uv_stride`/`width`/`height`.
- `GpuFrame` (`format.rs:420-467`): `Arc<dyn GpuFrameInner>` wrapper.
- `GpuFrameInner` (`format.rs:480-503`): `download_rgba`, `gpu_pixel_format`, `dimensions`,
  `download_nv12`, `native_handle`.
- `GpuPixelFormat` (`format.rs:527-536`): `Nv12` | `Bgrx` | `Bgra` (GPU-native formats).
- `NativeFrameHandle` (`format.rs:68-87`): platform zero-copy handle, `#[non_exhaustive]`,
  variants gated per-OS: `DmaBuf(DmaBufInfo)` (Linux, `format.rs:505-525`),
  `HardwareBuffer(HardwareBufferInfo)` (Android, `format.rs:89-109`),
  `CvPixelBuffer(CvPixelBufferInfo)` (macOS, `format.rs:111-193`).
- `AppleGpuFrame` (`format.rs:195-387`) is the macOS `GpuFrameInner` impl, backed by a
  retained `CVPixelBuffer`, with CPU-readback fallbacks (`download_bgra`, `download_nv12_planes`).

`VideoFrame::rgba_image()` (`format.rs:748-810`) lazily materializes RGBA from any variant
(Bgra swap, GPU download, NV12/I420 conversion - the last two require the `h264`/`av1` feature
because they call `processing::convert`).

### Data model - encoded frame & packet

```rust
pub struct EncodedFrame {              // format.rs:409-418  (encoder OUTPUT)
    pub is_keyframe: bool,
    pub timestamp: Duration,
    pub payload: bytes::Bytes,
}

pub struct MediaPacket {               // format.rs:389-407  (decoder INPUT)
    pub timestamp: Duration,
    pub payload: buf_list::BufList,    // scatter-gather, zero-copy from transport
    pub is_keyframe: bool,
}
```

The two are deliberately distinct: `EncodedFrame` uses a contiguous `Bytes` (encoder produced
it), `MediaPacket` uses `buf_list::BufList` for zero-copy from the (external) MoQ transport,
with `into_payload_bytes()` collapsing it (`format.rs:400-407`).

### Config types

Two families:

1. **Catalog configs** (`config.rs`) - describe an *encoded stream*, WebCodecs-modeled:
   - `VideoConfig` (`config.rs:11-33`): `codec: VideoCodec`, `description` (avcC), `coded_width/
     height`, `display_ratio_*`, `bitrate`, `framerate`, `optimize_for_latency`.
   - `AudioConfig` (`config.rs:38-50`): `codec`, `sample_rate`, `channel_count`, `bitrate`, `description`.
   - `VideoCodec` (`config.rs:53-61`): `H264(H264)` | `AV1(AV1)` | `Other(String)`.
   - `AudioCodec` (`config.rs:64-72`): `Opus` | `Pcm` | `Other(String)`.
   - `H264` (`config.rs:75-85`): `inline`, `profile`, `constraints`, `level` (avc1/avc3 params).
   - `AV1` (`config.rs:90-116`): full ISOBMFF codec-string param set (profile/level/tier/bitdepth/
     chroma/color primaries…).

2. **Encoder/decoder-construction configs** (`format.rs`):
   - `VideoEncoderConfig` (`format.rs:983-1107`): builder over width/height/framerate/bitrate/
     `scale_mode: ScaleMode`/`keyframe_interval`/`nal_format: NalFormat`. Has
     `resolve_for_source`, `default_bitrate(bits_per_pixel)`, `bitrate_or_default`.
   - `AudioEncoderConfig` (`format.rs:1119-1162`): sample_rate/channel_count/bitrate builder.
   - `DecodeConfig` (`format.rs:917-936`): `pixel_format`, `backend: DecoderBackend`.
   - `PlaybackConfig` (`format.rs:938-957`): `backend` + `pixel_format` + `quality: Quality`;
     converts to `DecodeConfig`.
   - `DecoderBackend` (`format.rs:905-916`): `Auto` (try HW then SW) | `Software`.
   - `Quality` (`format.rs:890-903`): `Highest`/`High`/`Mid`/`Low`.
   - `NalFormat` (`format.rs:959-970`): `AnnexB` (start codes) | `Avcc` (4-byte length prefix).
   - `VideoPreset` (`format.rs:817-870`): `P180/P360/P720/P1080`, all 30fps.
   - `AudioPreset` (`format.rs:872-888`): `Hq` (128kbps) | `Lq` (32kbps).
   - `AudioFormat` (`format.rs:14-47`): sample_rate + channel_count.
   - `VideoFormat` (`format.rs:59-66`): `pixel_format` + `dimensions`.

> **Comparison anchor for moq:** the moq/hang world uses `hang::catalog::VideoConfig` /
> `hang::frame`. rusty-codecs mirrors those config shapes 1:1 (`config.rs` `From` impls) but
> owns a distinct in-flight data model: `VideoFrame`/`FrameData` (raw, multi-backing) and the
> `EncodedFrame`(out)/`MediaPacket`(in) split. There is no hang `Frame`/`GroupConsumer` concept
> here - grouping/latency/jitter lives in moq-media, not rusty-codecs.

### Dynamic dispatch layer (`codec.rs`, `codec/dynamic.rs`)

- `codec.rs` defines two *runtime enums* distinct from the `config.rs` codec enums:
  `AudioCodec` (`codec.rs:53-94`, `Opus`/`Pcm`) and `VideoCodec` (`codec.rs:97-279`) which
  enumerate concrete *backends* (`H264`, `Av1`, `VtbH264`, `VaapiH264`, `V4l2H264`,
  `AndroidH264`), each strum-serialized (`"h264-vaapi"` etc). Provides `available()`,
  `best_available()` (HW-preferred, `codec.rs:154-176`), `is_hardware()` (`codec.rs:179-194`),
  `create_encoder()` dispatch (`codec.rs:197-216`), `display_name()`, `parse_or_*`.
- `codec/dynamic.rs`: `DynamicVideoDecoder` (`dynamic.rs:59-171`) and `DynamicAudioDecoder`
  (`dynamic.rs:178-235`) - `#[non_exhaustive]` enums whose `new()` inspects `config.codec` and,
  for H.264 with `Auto` backend, **probes HW decoders in order** (VAAPI → V4L2 → VideoToolbox →
  Android HW → Android ByteBuffer) falling back to software openh264 (`dynamic.rs:83-134`). A
  `dispatch_video!` macro (`dynamic.rs:26-52`) forwards each trait method. `DefaultDecoders`
  (`dynamic.rs:13-19`) binds these as the `Decoders` impl.

Both dispatch layers use cfg-flags `any_video_codec` / `any_audio_codec` (build-script-derived
cfgs) to stay compilable with zero codec features.

---

## 2. Codec Backend Implementations

All under `codec/`. Trait impl line references from grep.

| Backend | Type(s) | Trait(s) | HW/SW | Platform | Key dep | Notes |
|---|---|---|---|---|---|---|
| **h264** | `H264Encoder`, `H264VideoDecoder` | `VideoEncoderFactory`+`VideoEncoder` (`h264/encoder.rs:179,200`), `VideoDecoder` (`h264/decoder.rs:36`) | **SW** | all | `openh264` 0.9 (`features=["source"]`, builds Cisco lib) | ID `"h264-openh264"`. Baseline/constrained/L3.0 config via `h264_video_config()` (`h264.rs:16-41`) |
| **av1** | `Av1Encoder`, `Av1VideoDecoder` | `VideoEncoderFactory`+`VideoEncoder` (`av1/encoder.rs:141,181`), `VideoDecoder` (`av1/decoder.rs:33`) | **SW** | all | encode `rav1e` 0.8 (`av1/encoder.rs:4`); decode `rav1d` (git, memorysafety fork, bitdepth 8/16 + asm) via safe wrapper `rav1d_safe.rs` (`av1/decoder.rs:5`) | ID `"av1-rav1e"` |
| **opus** | `OpusEncoder`, `OpusAudioDecoder` | `AudioEncoderFactory`+`AudioEncoder` (`opus/encoder.rs:157,179`), `AudioDecoder` (`opus/decoder.rs:43`) | **SW** | all | `unsafe-libopus` 0.2 (pure-Rust libopus port) (`opus/encoder.rs:4`, `opus/decoder.rs:2`) | ID `"opus"`. Always 48kHz internal |
| **pcm** | `PcmEncoder`, `PcmAudioDecoder` | `AudioEncoderFactory`+`AudioEncoder` (`pcm/encoder.rs:67,86`), `AudioDecoder` (`pcm/decoder.rs:27`) | **SW** | all | none (raw f32 passthrough) | ID `"pcm"`. 20ms LE-f32 chunks, no compression (`pcm.rs:1-9`) |
| **vaapi** | `VaapiEncoder`, `VaapiDecoder`, `VppScaler` | `VideoEncoderFactory`+`VideoEncoder` (`vaapi/encoder.rs:1230,1251`), `VideoDecoder` (`vaapi/decoder.rs:469`) | **HW** | Linux | `cros-codecs` 0.0.6 (`cros-codecs/vaapi`); raw libva FFI for VPP (`vaapi/vpp_scaler.rs:15`) | H.264 via Intel/AMD VAAPI. Produces DMA-BUF `Gpu` frames. `VppScaler` does GPU scale/re-tile |
| **v4l2** | `V4l2Encoder`, `V4l2Decoder` | `VideoEncoderFactory`+`VideoEncoder` (`v4l2/encoder.rs:184,205`), `VideoDecoder` (`v4l2/decoder.rs:50`) | **HW** | Linux (ARM SoC) | encoder = **raw libc ioctls** on V4L2 M2M (`v4l2/encoder.rs:294+`); decoder = `v4l2r` 0.0.7 crate (`v4l2/decoder.rs:169`) | Tested on Pi bcm2835-codec. Device paths `/dev/video11`/`10`, env-overridable (`v4l2.rs:57-74`). Extensive SoC-portability doc (`v4l2.rs:1-45`) |
| **vtb** (VideoToolbox) | `VtbEncoder`, `VtbDecoder` | `VideoEncoderFactory`+`VideoEncoder` (`vtb/encoder.rs:204,225`), `VideoDecoder` (`vtb/decoder.rs:82`) | **HW** | macOS | `objc2-video-toolbox`/`-core-media`/`-core-video`/`-core-foundation` 0.3 (`vtb/encoder.rs:10-24`) | ID `"h264-vtb"`. CVPixelBuffer `Gpu` frames for Metal zero-copy. Module wrapped in clippy-allow (`codec.rs:16-28`) |
| **android** | `AndroidEncoder`, `AndroidDecoder`, `AndroidHwDecoder` | `VideoEncoderFactory`+`VideoEncoder` (`android/encoder.rs:259,280`), `VideoDecoder` (`android/decoder.rs:61`, `android/hw_decoder.rs:67`) | **HW** | Android | `ndk` 0.9 (`media`, `api-level-26`) MediaCodec (`android/encoder.rs:10`, `hw_decoder.rs:12`) | Two decoders: `AndroidDecoder` (ByteBuffer, CPU NV12→RGBA) and `AndroidHwDecoder` (ImageReader surface, zero-copy `HardwareBuffer`) (`android/mod.rs:1-15`). Extra files `format.rs`, `gpu_frame.rs` |

### H.264 bitstream helpers (`codec/h264/`)

- `annexb.rs` - NAL-unit tooling shared by all H.264 backends. `AnnexBNalIter` iterates NAL
  units splitting on `0x000001`/`0x00000001` start codes (`annexb.rs:1-40`). Exports (used by
  e.g. v4l2 encoder `v4l2/encoder.rs:10`): `annex_b_to_length_prefixed`, `build_avcc`,
  `extract_sps_pps`, `parse_annex_b`. Converts between Annex B and avcC framing, builds the
  avcC `description` record.
- `sps.rs` - **SPS VUI patcher for low-latency decode** (`sps.rs:1-13`): rewrites SPS NALs to
  set `max_num_reorder_frames=0` / `max_dec_frame_buffering=1`, eliminating DPB reordering
  delay on Baseline streams. Contains exp-golomb bit reader/writer (`read_ue` `sps.rs:14-40`).
  Currently `#[allow(dead_code)]` "kept for potential future use" (`h264.rs:4`).

---

## 3. Render + Processing (likely UNIQUE to iroh-live - not in moq/hang)

### `processing/` (`processing.rs:1-6`)

- `scale.rs` - `ScaleMode` (`Fit`/`Stretch`/`Cover`, `scale.rs:4-18`) with even-dimension
  `resolve()` (`scale.rs:20-32`); `Scaler` wraps **`pic-scale`** with bilinear resampling and
  double-buffered destinations to avoid per-frame alloc (`scale.rs:34-66`). Also `fit_within`.
  Gated always-on (no feature).
- `convert.rs` - colorspace conversion via **`yuv`** crate (`convert.rs:1-8`): RGBA/BGRA ↔
  NV12/I420 in both directions (`rgba_to_yuv_nv12`, `yuv_nv12_to_rgba`, `yuv420_to_rgba`, …),
  BT.601. `YuvData` planar container (`convert.rs:10-39`). Gated `any(h264, av1)`.
- `resample.rs` - `Resampler` wraps **`rubato`** sinc resampler + `audioadapter-buffers`
  (`resample.rs:1-16`); passthrough when rates match. Gated `any(opus, pcm)`.

### `render/` - GPU renderers (gated `wgpu` OR `gles`) (`render.rs`)

The `render.rs` root defines **`WgpuVideoRenderer`** (`render.rs:45-799`): renders any
`VideoFrame`/`FrameData` variant to an RGBA `wgpu` texture, choosing a path per frame
(`render.rs:267-359`) tracked by `RenderPath` enum (`render.rs:70-100`): `CpuRgba`, `CpuNv12`,
`DmaBuf` (Linux zero-copy), `MetalZeroCopy` (macOS zero-copy), `GpuDownload` (fallback). NV12→
RGBA is a fragment-shader pass (`nv12_to_rgba.wgsl`, `render.rs:152-155`). Has failure counters
that disable zero-copy paths after 3 failures. `render_cached()` copies into a persistent
texture for external compositors (Bevy, egui, dioxus-native).

Submodules (all platform + feature gated):

- `render/dmabuf_import.rs` (Linux, `dmabuf-import` feature) - **zero-copy DMA-BUF → wgpu via
  raw Vulkan** (`ash`): imports VAAPI NV12 DMA-BUFs using `VK_EXT_image_drm_format_modifier` +
  external-memory-fd, GPU-copies planes to R8/RG8; runs a VAAPI VPP re-tile blit when the
  modifier is Vulkan-incompatible (Intel Y_TILED) (`dmabuf_import.rs:1-13`). Exports
  `create_device_with_dmabuf_extensions` (`render.rs:28`).
- `render/gles.rs` (`gles` feature, `glow`) - GLES2 fullscreen-triangle renderer with RGBA and
  NV12 (LUMINANCE/LUMINANCE_ALPHA) upload paths + fragment-shader convert (`gles.rs:1-14`).
- `render/gles_dmabuf.rs` (Linux, `gles-dmabuf`) - zero-copy DMA-BUF → EGL/GLES via
  `EGL_EXT_image_dma_buf_import` + `glEGLImageTargetTexture2DOES`, binds Y/UV EGLImages into the
  `gles::GlesRenderer` NV12 program (`gles_dmabuf.rs:1-12`).
- `render/metal_import.rs` (macOS, `metal-import`) - zero-copy `CVPixelBuffer` → wgpu via
  `CVMetalTextureCache` (IOSurface aliasing); mirrors the DMA-BUF path for VideoToolbox/
  ScreenCaptureKit frames (`metal_import.rs:1-8`).

**These render/processing modules are the parts most clearly unique to iroh-live.** moq/hang
concern themselves with transport, catalogs, and grouping; they carry no GPU renderer, no
DMA-BUF/Metal/EGL zero-copy import, no wgpu/GLES pipeline, and no pic-scale/yuv/rubato
processing. The colorspace-convert and resample helpers may overlap conceptually with any
moq-side conversion but are implemented independently here.

### `test_sources.rs`

Ready-to-use `VideoSource` (animated SMPTE color bars, bouncing scan line, beep indicator) and
`AudioSource` (880 Hz tone) for demos/tests (`test_sources.rs:1-40`).

---

## 4. Public API Surface (what a consumer imports)

No root re-exports; consumers reach into modules:

- `rusty_codecs::traits` - `VideoEncoder`, `VideoEncoderFactory`, `VideoDecoder`,
  `AudioEncoder`, `AudioEncoderFactory`, `AudioDecoder`, `Decoders`, `AudioSource`, `AudioSink`,
  `AudioSinkHandle`, `AudioStreamFactory`, `VideoSource`, `PreEncodedVideoSource`.
- `rusty_codecs::format` - `VideoFrame`, `FrameData`, `EncodedFrame`, `MediaPacket`,
  `PixelFormat`, `GpuPixelFormat`, `GpuFrame`, `GpuFrameInner`, `Nv12Planes`,
  `NativeFrameHandle` (+per-OS info structs), `VideoFormat`, `AudioFormat`, `VideoPreset`,
  `AudioPreset`, `Quality`, `DecoderBackend`, `DecodeConfig`, `PlaybackConfig`, `NalFormat`,
  `VideoEncoderConfig`, `AudioEncoderConfig`, `ScaleMode` (re-exported here from processing).
- `rusty_codecs::config` - `VideoConfig`, `AudioConfig`, `VideoCodec`, `AudioCodec`, `H264`,
  `AV1` (catalog/WebCodecs config + hang interop `From` impls).
- `rusty_codecs::codec` - dispatch enums `codec::VideoCodec` / `codec::AudioCodec`,
  `DynamicVideoDecoder`, `DynamicAudioDecoder`, `DefaultDecoders`, and (feature-gated,
  flattened via `pub use backend::*`) the concrete backend types (`H264Encoder`,
  `Av1Encoder`, `OpusEncoder`, `VaapiEncoder`, `VtbEncoder`, …). H.264 module is fully `pub`
  (`codec.rs:6`), so `codec::h264::annexb` helpers are public.
- `rusty_codecs::render` - `WgpuVideoRenderer`, `RenderPath`, `CachedOutput`, importer submods.
- `rusty_codecs::processing` - `scale`, `convert`, `resample`.
- `rusty_codecs::test_sources` - test pattern sources.

---

## 5. Dependencies & Feature Flags (`Cargo.toml`)

**Always-on deps:** `anyhow`, `buf-list`, `bytes`, `derive_more`, `image` (no-default),
`n0-future`, `pic-scale` (scaling), `strum`, `throttled-tracing`, `tracing`, `yuv` (colorspace),
`libc` (raw ioctls).

**Optional codec/GPU deps:** `openh264` 0.9 (`source`), `unsafe-libopus` 0.2, `rubato` 1.0 +
`audioadapter-buffers` 2.0, `rav1e` 0.8, `rav1d` (git memorysafety fork; `bitdepth_8/16`, `asm`),
`glow` 0.16, `wgpu` 27 (wgsl), `wgpu-hal` 27, `hang` (workspace).
Linux-only: `cros-codecs` 0.0.6, `ash` 0.38, `v4l2r` 0.0.7. Android-only: `ndk` 0.9.
macOS-only: `objc2-*` 0.3 family, `objc2` 0.6, `metal` 0.32.

**Features:**

- `default = ["h264", "opus"]`
- Codecs: `h264` → openh264; `opus` → unsafe-libopus + rubato + audioadapter; `pcm` → rubato +
  audioadapter; `av1` → rav1e + rav1d.
- HW backends: `vaapi` → cros-codecs (+`cros-codecs/vaapi`); `v4l2` → v4l2r; `videotoolbox` →
  `apple-gpu` + objc2 video-toolbox/core-media; `android` → ndk; `media-foundation` (empty
  placeholder, Windows); `raspberry-pi` → `h264`.
- GPU/render: `wgpu`; `gles` → glow; `gles-dmabuf` → `gles`; `dmabuf-import` → `wgpu` + ash +
  `wgpu/vulkan`; `apple-gpu` → objc2 core-video/core-foundation; `metal-import` → `wgpu` +
  `apple-gpu` + metal + wgpu-hal + objc2-metal + CVMetalTexture bindings.
- Interop/util: `hang` (catalog conversions); `test-util`.

**Build-script cfgs** (referenced but derived externally): `any_video_codec`, `any_audio_codec`
gate the dispatch enums so the crate compiles with no codec features.
