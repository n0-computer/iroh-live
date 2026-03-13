# moq-media Codebase Review

## Recommendations

### API Decoupling (D1, D2)

- [x] **D1a: `MediaPacket` type** — own packet type replacing `OrderedFrame` in traits (`format.rs`)
- [x] **D1b: `VideoDecoder`/`AudioDecoder` traits use `MediaPacket`** instead of `OrderedFrame` (`traits.rs`)
- [x] **D1c: All codec impls updated** — h264, av1, vaapi, opus decoders take `MediaPacket`
- [x] **D2: `EncodedFrame` transport-independent** — own `timestamp: Duration` + `payload: Bytes`, `to_hang_frame()` at boundary
- [x] **D1d: `PacketSource` trait + `MoqPacketSource` wrapper** (`transport.rs`)
- [x] **D1e: `media_pipe()` / `PipeSink` / `PipeSource`** for local encode→decode (`transport.rs`)
- [x] **D1f: `VideoDecoderPipeline` struct** — standalone decoder with thread, any `PacketSource` (`pipeline.rs`)
- [x] **D1g: `VideoEncoderPipeline` struct** — standalone encoder with thread, `PipeSink` output (`pipeline.rs`)
- [x] **D1h: `subscribe.rs` uses `VideoDecoderPipeline`** — `from_consumer` delegates to pipeline
- [x] **D1i: `StreamClock` removed** — was unused dead code, deleted `processing/clock.rs`
- [x] **D1j: `PacketSink` trait** — abstract over `OrderedProducer` and `PipeSink` (`transport.rs`)
- [x] **D1k: Refactor `publish.rs` `EncoderThread`** to use `PacketSink` trait
- [x] **D1l: `AudioDecoderPipeline` / `AudioEncoderPipeline`** — audio equivalents (`pipeline.rs`)
- [x] **D1m: `subscribe.rs` `AudioTrack` uses `MediaPacket` throughout** (no inline `OrderedFrame` conversion)
- [x] **D1n: `subscribe.rs` audio path uses `PacketSource`/`forward_packets`** (removed `forward_frames`)
- [x] **D1o: Pipeline consolidation** — all encode/decode loops moved to `pipeline.rs`; `EncoderThread` removed from `publish.rs`; `AudioTrack::run_loop` removed from `subscribe.rs`; `publish.rs`/`subscribe.rs` use pipeline types

### Bugs

- [x] **B1: `Vec::remove(0)` in encoder packet buffers** — O(n) per pop, should be `VecDeque::pop_front()` (`h264/encoder.rs:182`, `av1/encoder.rs`, `vaapi/encoder.rs:489`)
- [x] **B2: VAAPI encoder Drop silently loses frames** on drain failure (`vaapi/encoder.rs:494-504`)
- [x] **B3: VAAPI Display::open() per frame download** — cached via `OnceCell` in `VaapiGpuFrame` (`vaapi/decoder.rs:93-100`)
- [x] **B4: Audio encoder timing re-measurement** — `start.elapsed()` called once, reused (`publish.rs:818-822`)
- [ ] **B5: `watch_local` hardcoded 30fps** (`subscribe.rs:554`)
- [ ] **B6: Per-frame Vulkan resource allocation** in DMA-BUF import — pool Y/UV plane images (`render/dmabuf_import.rs`)
- [x] **B7: NV12→RGBA shader missing limited-range expansion** — BT.601 limited range scaling added (`nv12_to_rgba.wgsl`)
- [ ] **B8: No VAAPI→Vulkan sync for DMA-BUF** — works on Intel implicit sync, may fail elsewhere (`render/dmabuf_import.rs`)
- [ ] **B9: `SharedVideoSource` pacing accumulates paused time** — resume can burst frames with no sleep (`publish.rs:529-569`)
- [ ] **B10: Opus encoder ignores requested input sample rate** — hardcodes 48k, can drift/non-48k mismatch (`codec/opus/encoder.rs:141-147`, `publish.rs:764-768`)
- [x] **B11: `watch_local` video scaling errors are silently swallowed** — now logs warning on `Err`, only `Ok(None)` falls back silently (`subscribe.rs`)

### Design

- [ ] **D2a: Dual NAL format support (avcC / Annex B)** — decoders and encoders should handle both length-prefixed (avcC, `description` present) and Annex B inline (`description` absent) H.264 transport modes. See details in Design Issues section below.
- [ ] **D3: Encoder config by instantiation** — creates/drops encoders just for config (`publish.rs:363,475`)
- [ ] **D4: SharedVideoSource park/unpark fragile** — replace with condvar or select (`publish.rs:520-586`)
- [ ] **D6: `apply_audio` fire-and-forget task** — no handle, caller can't observe failure (`controller.rs:281-293`)
- [ ] **D7: No backpressure encoder→transport** — frames produced at source rate regardless (`publish.rs:706-732`)
- [x] **D8: BGRA pixel swap code still in `from_video_source`** — decoders now output native BGRA when `DecodeConfig.pixel_format == Bgra`; swap loops removed from both `pipeline.rs` and `subscribe.rs`
- [ ] **D9: VkImage coded vs display dimension mismatch** (`render/dmabuf_import.rs`)
- [x] **D10: Per-frame bind group creation in renderer** — NV12 bind group cached on `Nv12PlaneTextures`, recreated only on resolution change (`render.rs`)

### Performance

- [x] **P1: Per-frame YUV allocation** in encoders — `take_owned()` extracts `Vec` from `BufferStoreMut::Owned` without copying; eliminates 3 `.to_vec()` per YUV conversion (`processing/convert.rs`)
- [x] **P2: Per-frame RGBA allocation** in SW decoders — H.264 reuses `pixel_buf: Vec<u8>` across frames; AV1 uses `yuv420_to_rgba_from_slices` to avoid intermediate `YuvData` allocation (`h264/decoder.rs`, `av1/decoder.rs`)
- [x] **P3: Opus channel conversion allocates on identity** — caller now skips `convert_channels` when from==to (`opus/decoder.rs:94-98`)
- [ ] **P4: `parse_annex_b` allocates Vec** — could use iterator (`h264/annexb.rs`)
- [x] **P5: StreamClock unused** — removed dead code (`processing/clock.rs`, h264/av1 decoders)
- [x] **P6: Opus encoder queue/buffer use front-removal on `Vec`** — `packet_buf` changed to `VecDeque` with `pop_front()` (`codec/opus/encoder.rs`)
- [x] **P7: VideoToolbox encoder packet queue uses `Vec::remove(0)`** — changed to `VecDeque` with `pop_front()` (`codec/vtb/encoder.rs`)
- [x] **P8: Scaler clones source buffer** — `ImageStore::from_slice()` accepts borrowed input; `PicScaler` cached on struct (`processing/scale.rs`)
- [x] **P9: VAAPI encoder I420→NV12 intermediate** — `pixel_format_to_nv12()` converts RGBA/BGRA→NV12 directly, skipping I420 (`vaapi/encoder.rs`)
- [x] **P10: Opus decoder per-packet PCM allocation** — reusable `pcm_buf: Vec<f32>` on decoder struct; `convert_channels_into()` writes into existing `samples` Vec (`opus/decoder.rs`)
- [ ] **P11: Annex-B ↔ length-prefixed round-trip** — encoders produce Annex-B, convert to length-prefixed for transport; decoders convert back. Each direction allocates a `Vec<u8>` (`h264/annexb.rs`)
- [ ] **P12: Payload copy in all decoders** — `copy_to_bytes()` is zero-copy for single-chunk, but multi-chunk BufLists copy. Could check `chunk().len() == remaining` to avoid (`h264/decoder.rs`, `av1/decoder.rs`)

### Safety

- [ ] **S1: `unsafe impl Send` on 5+ types** — no runtime thread-ID assertions (`vaapi/encoder.rs:511`, `vaapi/decoder.rs:85`, `opus/encoder.rs`, `opus/decoder.rs`, `av1/rav1d_safe.rs`)
- [ ] **S2: rav1d_safe FFI bounds** — `copy_nonoverlapping` without bounds check, `assume_init` after C call (`av1/rav1d_safe.rs`)
- [ ] **S3: DMA-BUF fd ownership complexity** — correct but no RAII wrapper for dup'd fds (`render/dmabuf_import.rs`)
- [ ] **S4: VTB encoder Arc callback dance** — complex `Arc::into_raw`/`from_raw` in C callback (`vtb/encoder.rs:83`)

### Testing

- [ ] **T1: No `subscribe.rs` tests** — `SubscribeBroadcast`, `VideoTrack`, `AudioTrack` untested
- [ ] **T2: No audio decode loop tests** — `audio_decode_loop` in `pipeline.rs` untested
- [ ] **T3: No `render.rs` tests** — `WgpuVideoRenderer` untested (needs GPU)
- [ ] **T4: No integration tests** — no end-to-end encode→transport→decode
- [ ] **T5: `PublishCaptureController` not tested** — `set_opts` has no tests
- [ ] **T6: No fuzz tests** — codec decoders not fuzzed with malformed input
- [ ] **T7: `VideoDecoderPipeline` / `VideoEncoderPipeline` not tested** — new pipeline module has no tests yet

### API

- [x] **A1: Encoder rate control API** — `set_bitrate()` on `VideoEncoder`/`AudioEncoder` traits (default no-op); implemented for Opus and VTB
- [x] **A2: Builder pattern for encoder configuration** — `VideoEncoderConfig`/`AudioEncoderConfig` with `from_preset()` + builder methods; `with_config()` on factory traits
- [ ] **A3: Quality enum is coarse** — `Highest/High/Mid/Low` maps to 4 fixed presets only
- [ ] **A4: `DecodeConfig` minimal** — only `pixel_format` and `backend`, no resolution/framerate constraints
- [ ] **A5: VideoToolbox decoder stub** — `vtb/decoder.rs` is TODO skeleton only
- [ ] **A6: No `set_keyframe_interval` on `VideoEncoder` trait** — all encoders hardcode keyframe interval = framerate (1/sec) at construction. Only VTB supports runtime change via `VTSessionSetProperty`. OpenH264/rav1e/VAAPI require encoder recreation. Add trait method with default no-op; implement for VTB; others store desired interval for next reset.
- [ ] **A7: `AudioEncoderFactory::with_preset(format, ..)` contract is ambiguous on sample-rate handling** — Opus currently ignores `format.sample_rate` and hardcodes 48k (`codec/opus/encoder.rs:141-147`)

---

## Architecture

### Module Map

```
lib.rs
├── format.rs          — Frame types, presets, pixel formats, MediaPacket
├── traits.rs          — Encoder/Decoder/Source/Sink traits (use MediaPacket)
├── transport.rs       — PacketSource/PacketSink traits, MoqPacketSource/Sink, media_pipe
├── pipeline.rs        — Video/Audio Encoder/Decoder Pipelines (all encode/decode loops)
├── codec/
│   ├── mod.rs         — Codec enums, re-exports, create_encoder()
│   ├── dynamic.rs     — DynamicVideoDecoder/DynamicAudioDecoder dispatch
│   ├── h264/          — OpenH264 encoder + decoder + Annex B utils
│   ├── av1/           — rav1e encoder + rav1d decoder + safe wrapper
│   ├── opus/          — Opus encoder + decoder + channel conversion
│   ├── vaapi/         — VAAPI H.264 encoder + decoder (Linux)
│   ├── vtb/           — VideoToolbox H.264 encoder + decoder stub (macOS)
│   └── test_util.rs   — Test pattern generation
├── capture/
│   ├── camera.rs      — nokhwa camera capture
│   └── screen.rs      — xcap screen capture
├── processing/
│   ├── convert.rs     — YUV ↔ RGBA (yuvutils-rs)
│   ├── scale.rs       — Bilinear scaling (pic-scale)
│   ├── resample.rs    — Audio resampling (rubato)
│   └── mjpg.rs        — MJPEG decoder for camera frames
├── audio_backend.rs   — Firewheel audio I/O + AEC
├── publish.rs         — PublishBroadcast, VideoRenditions, AudioRenditions
├── publish/controller.rs — PublishCaptureController
├── subscribe.rs       — SubscribeBroadcast, VideoTrack, AudioTrack
├── render.rs          — wgpu NV12→RGBA renderer
│   └── dmabuf_import.rs — Zero-copy DMA-BUF Vulkan import
└── util.rs            — spawn_thread helper
```

### Threading Model

- **Video encoders**: OS thread per active rendition (source → scale → encode → write)
- **Video decoders**: OS thread per subscribed track (read → decode → frame output)
- **Audio encoders**: OS thread per rendition (20ms tick interval)
- **Audio decoders**: OS thread (10ms tick interval)
- **Catalog watcher**: Tokio task (async polling)
- **Frame forwarding**: Tokio task bridging async PacketSource → mpsc → sync decoder thread
- **SharedVideoSource**: Single OS thread with park/unpark, fan-out via `watch::channel`
- **Audio backend**: OS thread running Firewheel graph

### API Decoupling Status

The worktree has partially decoupled moq-media from hang/moq-lite transport types:

**Done:**
- `MediaPacket` type with `From<OrderedFrame>` conversion (`format.rs`)
- `EncodedFrame` now owns `timestamp: Duration` + `payload: Bytes` with `to_hang_frame()` at boundary
- `VideoDecoder`/`AudioDecoder` traits use `MediaPacket` instead of `OrderedFrame`
- All codec impls updated (h264, av1, vaapi, opus)
- `PacketSource` trait + `MoqPacketSource` wrapper (`transport.rs`)
- `media_pipe()` / `PipeSink` / `PipeSource` for local pipelines (`transport.rs`)
- `VideoDecoderPipeline` / `VideoEncoderPipeline` structs (`pipeline.rs`)
- `subscribe.rs` `from_consumer` delegates to `VideoDecoderPipeline`
- `StreamClock` removed (was unused dead code)

**All items complete** (D1a–D1o).

---

## Bugs

### B1: `Vec::remove(0)` in all encoder packet buffers (FIXED)

Changed `packet_buf` from `Vec` to `VecDeque` in all three encoders (h264, av1, vaapi). `pop_packet()` now uses `pop_front()` — O(1) instead of O(n).

### B2: VAAPI encoder Drop silently loses frames on drain failure (FIXED)

Drop now logs a warning on drain failure before returning early.

### B3: VAAPI decoder `Display::open()` called per frame download (FIXED)

Cached the VAAPI display in `VaapiGpuFrame` via `OnceCell` — opened lazily on first download, reused for subsequent calls on the same frame.

### B4: Audio encoder timing re-measurement (FIXED)

`start.elapsed()` is now captured once into `elapsed` and reused for both the warning check and sleep calculation.

### B5: `watch_local` viewport hardcoded to 30 fps

**File**: `subscribe.rs:554`

```rust
// TODO: Make configurable.
let fps = 30;
```

Local video preview always runs at 30 fps regardless of source or encoder framerate.

### B6: Per-frame Vulkan resource allocation in DMA-BUF import

**File**: `render/dmabuf_import.rs`

`import_nv12()` creates 3 VkImages + 3 VkDeviceMemory allocations per frame (NV12 source, Y plane, UV plane). wgpu defers cleanup until device poll, so at 30fps resources pile up faster than freed. This causes "device memory allocation failed" errors under sustained use.

**Fix**: Pool Y and UV plane images across frames (same resolution = same image). Only the NV12 source image needs to be per-frame (different DMA-BUF fd).

### B7: NV12→RGBA shader missing limited-range expansion (FIXED)

Shader now applies BT.601 limited-range scaling: Y [16..235] and UV [16..240] are expanded before color conversion.

### B8: No VAAPI→Vulkan synchronization for DMA-BUF

**File**: `render/dmabuf_import.rs`

`drain_events()` calls `handle.sync()` for VAAPI decode completion, but no explicit synchronization primitive (fence/semaphore) between VAAPI write and Vulkan read of the DMA-BUF. The `VK_QUEUE_FAMILY_EXTERNAL` barrier provides an implicit acquire, but the Vulkan spec requires either a fence signaled by the exporter or `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`. Works on Intel (implicit sync) but may fail on explicit-sync-only drivers.

### B9: `SharedVideoSource` pacing accumulates parked time, causing bursty catch-up on resume

**File**: `publish.rs:529-569`

`SharedVideoSource::new` uses a fixed `start = Instant::now()` and frame index `i` to compute:

```rust
let expected = frame_time * i;
let actual = start.elapsed();
if actual < expected {
    thread::sleep(expected - actual);
}
```

When the thread is parked (no subscribers), wall-clock time keeps advancing while `i` does not. After unpark, `actual >> expected`, so sleep is skipped for many iterations and frames are pulled/sent as fast as possible until the schedule catches up. This creates bursty CPU usage and uneven local preview cadence.

**Fix**: Reset pacing baseline after each park/unpark transition (or use a rolling `next_deadline` that is reinitialized on resume).

### B10: Opus encoder ignores requested input sample rate

**Files**: `codec/opus/encoder.rs:141-147`, `publish.rs:764-768`

`OpusEncoder::with_preset(format, preset)` accepts an `AudioFormat` but always constructs the encoder with `SAMPLE_RATE` (48_000):

```rust
Self::new(SAMPLE_RATE, format.channel_count, bitrate)
```

Meanwhile, `EncoderThread::spawn_audio` sizes its per-tick input buffer from `source.format().sample_rate`. For non-48k sources, samples are fed at the source cadence into a 48k-configured Opus encoder, causing packetization/timestamp drift and incorrect real-time behavior.

**Fix options**:
- Enforce 48k at the API boundary (reject non-48k in `with_preset` with a clear error).
- Or add explicit resampling before/inside Opus encoding.

### B11: `watch_local` swallows scaler errors (FIXED)

`Err` and `Ok(None)` are now handled separately. Scaler errors are logged via `warn!` before falling back to the unscaled frame. `Ok(None)` (no scaling needed) still falls back silently as intended.

---

## Design Issues

### D3: Encoder config discovery by instantiation

**Files**: `publish.rs:363-365,475-476`

```rust
// We need to create the encoder to get the config, even though we drop it
// again (it will be created on demand). Not ideal, but works for now.
let config = make_encoder()?.config();
```

Creates and immediately drops encoders just to extract `VideoConfig`/`AudioConfig`. VAAPI encoder construction is expensive (opens display, creates surfaces, encodes priming frame). Should separate config from construction.

### D4: SharedVideoSource park/unpark is fragile

**File**: `publish.rs:520-586`

The source thread uses `thread::park()/unpark()` for lifecycle management. This is correct but hard to reason about:
- Thread parks when `running == false`
- Unparked by `start()` (subscriber_count 0→1) or `VideoRenditions::drop()` (line 505)
- Race between cancellation check and park: if `shutdown.cancel()` fires between `shutdown.is_cancelled()` check and `thread::park()`, the thread hangs until explicit `unpark()` in `VideoRenditions::drop()`

Currently safe due to the `unpark()` in `drop()`, but brittle. A condvar or select-style mechanism would be clearer.

### D6: `PublishCaptureController::apply_audio` spawns fire-and-forget tokio task

**File**: `controller.rs:281-293`

```rust
tokio::spawn(async move {
    let mic = match audio_ctx.default_input().await { ... };
    camera.lock().unwrap().set_audio(Some(renditions))?;
});
```

Audio setup runs in a detached task. If it fails, the error is logged but `set_opts()` has already returned `Ok`. The caller has no way to know audio setup failed. The task handle is dropped (not joined or tracked).

### D7: No backpressure between encoder and transport

**File**: `publish.rs:706-732`

Encoder threads call `producer.write(pkt.to_hang_frame())` synchronously. If the transport can't keep up (slow network), writes may fail, but there's no backpressure to the encoder or source. Frames are produced at source rate regardless.

### D8: BGRA pixel swap code in decoders and pipeline (FIXED)

Decoders (H.264, AV1) now respect `DecodeConfig.pixel_format` and output BGRA natively via `yuv420_to_bgra_from_slices()` when requested. Per-pixel swap loops removed from both `pipeline.rs` decode_loop and `subscribe.rs` video source path. `DecodedVideoFrame::new_cpu_with_format()` preserves the pixel format through to the renderer.

### D9: VkImage coded vs display dimension mismatch

**File**: `render/dmabuf_import.rs`

The Y plane VkImage is created with `coded_width x coded_height` (e.g., 1920x1088) but wrapped as a wgpu texture with `display_width x display_height` (e.g., 1920x1080). H.264 coded height is commonly rounded up to 16. The wgpu TextureDescriptor size doesn't match the actual VkImage extent — could cause Vulkan validation errors or incorrect sampling.

### D10: Per-frame bind group creation in renderer (FIXED)

NV12 bind group is now cached on `Nv12PlaneTextures` and recreated only when textures are recreated (resolution change). The DMA-BUF path (`render_imported_nv12`) still creates per-frame bind groups because imported textures change every frame (different DMA-BUF FDs).

---

## Performance

### P0a: `MediaPacket.payload` eager copy from hang (FIXED)

**File**: `format.rs` — `From<OrderedFrame> for MediaPacket`

The `OrderedFrame.payload` is a `BufList` (scatter-gather `VecDeque<Bytes>`). The original conversion did `f.payload.copy_to_bytes(f.payload.remaining())` — an eager contiguous copy on every packet from MoQ transport. For single-chunk payloads (the common case), `BufList::copy_to_bytes` is just a refcount bump, but for multi-chunk payloads this copies all data.

**Fixed**: `MediaPacket.payload` is now `BufList`, moved directly from `OrderedFrame` with zero copy. Decoders call `copy_to_bytes()` at decode time, which is zero-copy for single-chunk payloads.

### P0b: `DecodedVideoFrame::img()` cloned entire pixel buffer (FIXED)

**File**: `format.rs` — `DecodedVideoFrame::img()`

The refactored `DecodedVideoFrame` stored raw `Vec<u8>` in `CpuFrame` and created an `RgbaImage` lazily in `img()` via `RgbaImage::from_raw(w, h, cpu.data.clone())`. This cloned the entire pixel buffer (e.g. 8MB at 1080p) on every first call. The pre-refactor code stored `image::Frame` directly and `img()` was a zero-copy reference return.

**Fixed**: `CpuFrame` now stores `RgbaImage` directly. `img()` returns `&cpu.image` for CPU frames — zero-copy, matching pre-refactor behavior. Added `DecodedVideoFrame::from_image()` constructor to avoid round-tripping through `into_raw()`/`from_raw()`.

### P1: YUV allocation per frame (FIXED)

`take_owned()` helper extracts the owned `Vec<u8>` from `BufferStoreMut::Owned` without copying — eliminates 3 `.to_vec()` per YUV conversion (~3MB/frame at 1080p).

### P2: RGBA allocation in decoders (FIXED)

H.264 decoder reuses `pixel_buf: Vec<u8>` across frames (`.resize()` only reallocates on resolution change). AV1 decoder passes plane slices directly to `yuv420_to_rgba_from_slices()`, avoiding intermediate `YuvData` allocation.

### P3: Channel conversion allocates unnecessarily (FIXED)

`push_packet` now moves the resampled buffer directly when `channel_count == target_channel_count`, skipping `convert_channels` entirely.

### P4: `parse_annex_b` allocates Vec of slices

**File**: `h264/annexb.rs`

Builds a `Vec<&[u8]>` of all NAL units. Could use an iterator pattern to avoid the allocation.

### P5: StreamClock unused (FIXED)

Removed `StreamClock` from both H.264 and AV1 decoders and deleted `processing/clock.rs`. The computed delay was never used for frame pacing.

### P6: Opus encoder front-removal on `Vec` (FIXED)

`packet_buf` changed from `Vec<EncodedFrame>` to `VecDeque<EncodedFrame>` with `pop_front()` — O(1) instead of O(n). `sample_buf` remains `Vec<f32>` because `opus_encode_float` requires a contiguous pointer via `as_ptr()`; `drain(..n)` is the best available approach without a custom ring buffer with contiguous guarantees.

### P7: VideoToolbox packet queue uses `Vec::remove(0)` (FIXED)

`CallbackState.packets` changed from `Vec<EncodedFrame>` to `VecDeque<EncodedFrame>`. `pop_packet()` now uses `pop_front()` — O(1) instead of O(n).

### P8: Scaler clones source buffer (FIXED)

`ImageStore::from_slice()` accepts borrowed `&[u8]` input, eliminating the full-frame `.to_vec()` clone. `PicScaler` is now a cached field on `Scaler` instead of being recreated per call.

### P9: VAAPI encoder I420→NV12 intermediate (FIXED)

`pixel_format_to_nv12()` converts RGBA/BGRA directly to NV12 via `yuvutils_rs::rgba_to_yuv_nv12`, skipping the I420 intermediate. `Nv12Data::into_contiguous()` concatenates Y and UV planes into the single buffer needed by `Nv12Frame`.

### P10: Opus decoder per-packet PCM allocation (FIXED)

Reusable `pcm_buf: Vec<f32>` on the decoder struct eliminates per-packet allocation. New `convert_channels_into()` writes directly into `self.samples` instead of returning a new `Vec`.

### P11: Annex-B ↔ length-prefixed round-trip

**Files**: `h264/annexb.rs`, `vaapi/encoder.rs`, `h264/decoder.rs`

Encoders produce Annex-B, convert to length-prefixed for transport. Decoders convert back to Annex-B. Each direction allocates a `Vec<u8>`. Could be avoided by using Annex-B end-to-end with a transport flag.

### P12: Payload copy in all decoders

**Files**: `h264/decoder.rs`, `av1/decoder.rs`, `opus/decoder.rs`

`copy_to_bytes()` materializes `BufList` into contiguous `Bytes`. Zero-copy for single-chunk (common case), but copies for multi-chunk. Could check `chunk().len() == remaining` and use the chunk directly.

### P13: Opus encoder per-frame output allocation (FIXED)

**File**: `codec/opus/encoder.rs`

`encode_pending` allocated `vec![0u8; 4000]` on every 20ms frame encode. Replaced with a reusable `encode_buf` field on the struct. Saves ~50 allocations/sec per audio stream.

### P14: VAAPI decoder NV12 plane copy via iterator (FIXED)

**File**: `codec/vaapi/decoder.rs:126`

`derive_nv12_planes` used `flat_map` + `iter().copied().collect()` to strip pitch padding from mapped VA image data. This grew Vecs byte-by-byte through iterator chaining. Replaced with pre-allocated Vecs and `copy_from_slice` per row — same memcpy volume, no reallocation or iterator overhead.

### P15: VAAPI encoder I420→NV12 bounds-checked per pixel (FIXED)

**File**: `codec/vaapi/encoder.rs:347`

`i420_to_nv12` used `u.get(idx).copied().unwrap_or(128)` and `v.get(idx).copied().unwrap_or(128)` per chroma sample — two bounds checks per pixel in the UV interleave loop. Since the planes are always correctly sized from `pixel_format_to_nv12`, replaced with direct indexing and pre-allocated output.

### P16: Double YUV→RGB→YUV conversion on encode path

All software encoders (H.264, AV1) accept RGBA frames and convert to YUV420 in `push_frame`. If the capture source produces NV12 natively (V4L2 cameras on Linux), the pipeline converts NV12→RGBA in the driver, then RGBA→YUV420 in the encoder. At 1080p this wastes ~5ms/frame.

*Fix:* Accept YUV/NV12 directly in encoder `push_frame`, or add a `VideoFrame::Nv12` variant. The VAAPI encoder already takes NV12 internally. Requires `VideoSource` trait changes and camera backend support for raw YUV passthrough.

### P17: VAAPI decoder opens 4 VA Displays per instance

**File**: `codec/vaapi/decoder.rs:394-439`

Each `VaapiDecoder` opens 4 `Display::open()` calls (decode, export, mapping, frame pool). With N concurrent streams, that's 4N DRM FDs + VA contexts. The separate displays exist to avoid driver serialization deadlocks (documented in comments), but the pool and mapping displays could likely share one, and the export display might be consolidatable if export calls are serialized with decode.

### P18: Per-frame DMA-BUF re-export in VAAPI decoder

**File**: `codec/vaapi/decoder.rs:198`

`extract_dma_buf_info` calls `to_native_handle` (clones DMA-BUF FDs) + `export_prime` on every decoded frame. Pool surfaces are long-lived — their export metadata (FD, modifier, planes) could be cached per surface and reused across frames. This would eliminate per-frame FD dup + ioctl overhead and the primary source of FD churn (now mitigated by the EMFILE fix, but still unnecessary work).

### P19: `frame.raw.to_vec()` in video source passthrough

**File**: `subscribe.rs:396`

`VideoTrack::from_video_source` calls `frame.raw.to_vec()` when the scaler returns `None` (no scaling needed). Since `frame.raw` is `bytes::Bytes`, this copies the entire frame. Could accept `Bytes` in `DecodedVideoFrame::new_cpu_with_format` or use `Bytes::into()` to avoid the copy.

### P20: VAAPI encoder `query_image_formats` per frame upload

**File**: `codec/vaapi/encoder.rs:126-133`

`upload_nv12_to_surface` calls `display.query_image_formats()` on every encode to find NV12. The result is stable for a given display. Should cache the `VAImageFormat` at encoder construction time.

---

## Safety

### S1: `unsafe impl Send` on multiple types

- `VaapiEncoder` (`vaapi/encoder.rs:511`) — wraps `Rc<Display>` which is `!Send`
- `VaapiDecoder` (`vaapi/decoder.rs`) — same
- `VaapiGpuFrame` (`vaapi/decoder.rs:85-86`) — Send + Sync
- `OpusEncoder` / `OpusDecoder` — raw C pointers
- `rav1d_safe::Decoder` / `Picture` — C library handles

All justified by single-threaded access patterns, but none have runtime assertions. A debug-mode assert (e.g., tracking thread ID) would catch accidental multi-threaded use.

### S2: rav1d_safe FFI wrapper bounds

**File**: `av1/rav1d_safe.rs`

- `ptr::copy_nonoverlapping()` at line 108: no validation that source pointer is valid for `data.len()` bytes
- Unsafe slice creation from dav1d-owned data (lines 172-174): lifetime tied to Picture struct, but aliasing is not enforced by the type system
- `assume_init()` after C function call: assumes C fully initializes the struct

### S3: DMA-BUF fd ownership complexity

**File**: `render/dmabuf_import.rs`

The fd ownership protocol is correct but complex:
1. `libc::dup()` creates owned copy
2. Vulkan takes ownership on successful `vkAllocateMemory`
3. On failure, fd is manually closed
4. Drop callback destroys VkImage + VkDeviceMemory

Correct but requires careful maintenance. A RAII wrapper for dup'd fds would reduce risk.

### S4: VTB encoder Arc callback dance

**File**: `vtb/encoder.rs:83-84`

The VTB compression callback uses `Arc::into_raw()` to pass state to the C callback, then `Arc::increment_strong_count()` + `Arc::from_raw()` in the callback to reclaim. Correct but the pattern is complex and easy to get wrong on modification.

---

## Testing

### Good Coverage

- Codec roundtrip tests (encode → decode) for every preset: H.264, AV1, Opus
- Cross-channel audio tests (mono encode → stereo decode)
- Dynamic decoder routing tests
- Processing: YUV conversion edge cases, scaling, resampling, MJPEG corruption
- Publish: `watch_local`, `set_video`, encoder pipeline end-to-end, rendition ordering
- Hardware codec tests (marked `#[ignore]`)

### Gaps

- **T1**: `subscribe.rs` — `SubscribeBroadcast`, audio forward path untested
- **T2**: `AudioTrack::run_loop` — no tests
- **T3**: `render.rs` — `WgpuVideoRenderer` no unit tests (needs GPU)
- **T4**: No end-to-end encode→transport→decode integration test
- **T5**: `PublishCaptureController::set_opts` — no tests
- **T6**: No fuzz tests for codec decoders with malformed input
- **T7**: `VideoDecoderPipeline` / `VideoEncoderPipeline` — new modules have no tests

---

## API Design

### Good

- **Trait-based codec abstraction**: `VideoEncoder`, `VideoDecoder`, `AudioEncoder`, `AudioDecoder` are clean
- **Dynamic dispatch**: `DynamicVideoDecoder` auto-selects HW vs SW codec
- **DecodedVideoFrame**: Clean CPU/GPU duality with lazy download caching via `OnceCell`
- **Rendition system**: VideoRenditions/AudioRenditions with factory callbacks
- **Watchable state**: Reactive observation of catalog and publish state via `n0_watcher`
- **GpuFrameInner trait**: Extensible platform abstraction for GPU frames
- **PacketSource trait + media_pipe**: Clean decoupling of decode from transport
- **VideoCodec::create_encoder()**: Dynamic encoder creation by codec enum

### Improvable

- ~~**A1**: No runtime bitrate adjustment~~ — `set_bitrate()` added to `VideoEncoder`/`AudioEncoder` traits; implemented for Opus and VTB
- ~~**A2**: No builder pattern for encoder/decoder configuration~~ — `VideoEncoderConfig`/`AudioEncoderConfig` added with `from_preset()` + builder methods
- **A3**: Quality enum coarse — 4 fixed presets, no custom resolution/bitrate
- **A4**: `DecodeConfig` minimal — only `pixel_format` and `backend`
- **A5**: VideoToolbox decoder is stub only — macOS HW decode not available
- **A7**: `AudioEncoderFactory::with_preset(format, ..)` does not define whether encoders must honor `format.sample_rate` or may coerce internally. Current Opus impl hardcodes 48k, so callers can pass incompatible formats without explicit error.
