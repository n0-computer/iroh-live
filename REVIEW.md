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
- [x] **D1i: `StreamClock` decoupled** from `hang::Timestamp`, uses `Duration`
- [ ] **D1j: `PacketSink` trait** — abstract over `OrderedProducer` and `PipeSink` (currently `PipeSink` is concrete only)
- [ ] **D1k: Refactor `publish.rs` `EncoderThread`** to use pipeline structs / `PacketSink` trait
- [ ] **D1l: `AudioDecoderPipeline` / `AudioEncoderPipeline`** — audio equivalents not yet implemented
- [ ] **D1m: `subscribe.rs` `AudioTrack` still converts `OrderedFrame`→`MediaPacket` inline** (line 399)
- [ ] **D1n: `forward_frames` in `subscribe.rs`** still uses `OrderedFrame` for audio path

### Bugs

- [ ] **B1: `Vec::remove(0)` in encoder packet buffers** — O(n) per pop, should be `VecDeque::pop_front()` (`h264/encoder.rs:182`, `av1/encoder.rs`, `vaapi/encoder.rs:489`)
- [ ] **B2: VAAPI encoder Drop silently loses frames** on drain failure (`vaapi/encoder.rs:494-504`)
- [ ] **B3: VAAPI Display::open() per frame download** — expensive, should cache (`vaapi/decoder.rs:95`)
- [ ] **B4: Audio encoder timing re-measurement** — `start.elapsed()` called twice (`publish.rs:819-825`)
- [ ] **B5: `watch_local` hardcoded 30fps** (`subscribe.rs:554`)
- [ ] **B6: Per-frame Vulkan resource allocation** in DMA-BUF import — pool Y/UV plane images (`render/dmabuf_import.rs`)
- [ ] **B7: NV12→RGBA shader missing limited-range expansion** — washed-out blacks/whites (`nv12_to_rgba.wgsl`)
- [ ] **B8: No VAAPI→Vulkan sync for DMA-BUF** — works on Intel implicit sync, may fail elsewhere (`render/dmabuf_import.rs`)

### Design

- [ ] **D2a: Dual NAL format support (avcC / Annex B)** — decoders and encoders should handle both length-prefixed (avcC, `description` present) and Annex B inline (`description` absent) H.264 transport modes. See details in Design Issues section below.
- [ ] **D3: Encoder config by instantiation** — creates/drops encoders just for config (`publish.rs:363,475`)
- [ ] **D4: SharedVideoSource park/unpark fragile** — replace with condvar or select (`publish.rs:520-586`)
- [ ] **D5: `.expect("poisoned")` throughout** — use `parking_lot::Mutex` (`publish.rs` 8x, `controller.rs` 5x)
- [ ] **D6: `apply_audio` fire-and-forget task** — no handle, caller can't observe failure (`controller.rs:281-293`)
- [ ] **D7: No backpressure encoder→transport** — frames produced at source rate regardless (`publish.rs:706-732`)
- [ ] **D8: BGRA pixel swap code still in `from_video_source`** (`subscribe.rs:582-586`); extracted in pipeline decode_loop but not in video source path
- [ ] **D9: VkImage coded vs display dimension mismatch** (`render/dmabuf_import.rs`)
- [ ] **D10: Per-frame bind group creation in renderer** — cache when dimensions unchanged (`render.rs:187-204,306-323`)

### Performance

- [ ] **P1: Per-frame YUV allocation** in encoders — ~90MB/s at 1080p30 (`processing/convert.rs`)
- [ ] **P2: Per-frame RGBA allocation** in SW decoders — double alloc (YUV→RGBA + scale) (`h264/decoder.rs`, `av1/decoder.rs`)
- [ ] **P3: Opus channel conversion allocates on identity** — `to_vec()` when from==to (`opus/decoder.rs`)
- [ ] **P4: `parse_annex_b` allocates Vec** — could use iterator (`h264/annexb.rs`)
- [ ] **P5: StreamClock unused** — computes delay but value is ignored by decoders (`processing/clock.rs`)

### Safety

- [ ] **S1: `unsafe impl Send` on 5+ types** — no runtime thread-ID assertions (`vaapi/encoder.rs:511`, `vaapi/decoder.rs:85`, `opus/encoder.rs`, `opus/decoder.rs`, `av1/rav1d_safe.rs`)
- [ ] **S2: rav1d_safe FFI bounds** — `copy_nonoverlapping` without bounds check, `assume_init` after C call (`av1/rav1d_safe.rs`)
- [ ] **S3: DMA-BUF fd ownership complexity** — correct but no RAII wrapper for dup'd fds (`render/dmabuf_import.rs`)
- [ ] **S4: VTB encoder Arc callback dance** — complex `Arc::into_raw`/`from_raw` in C callback (`vtb/encoder.rs:83`)

### Testing

- [ ] **T1: No `subscribe.rs` tests** — `run_loop`, `forward_frames`, `SubscribeBroadcast` untested
- [ ] **T2: No audio decode loop tests** — `AudioTrack::run_loop` untested
- [ ] **T3: No `render.rs` tests** — `WgpuVideoRenderer` untested (needs GPU)
- [ ] **T4: No integration tests** — no end-to-end encode→transport→decode
- [ ] **T5: `PublishCaptureController` not tested** — `set_opts` has no tests
- [ ] **T6: No fuzz tests** — codec decoders not fuzzed with malformed input
- [ ] **T7: `VideoDecoderPipeline` / `VideoEncoderPipeline` not tested** — new pipeline module has no tests yet

### API

- [ ] **A1: No encoder rate control API** — bitrate hardcoded `pixels * 0.07 * framerate_factor`
- [ ] **A2: No builder pattern** for encoder/decoder configuration
- [ ] **A3: Quality enum is coarse** — `Highest/High/Mid/Low` maps to 4 fixed presets only
- [ ] **A4: `DecodeConfig` minimal** — only `pixel_format` and `backend`, no resolution/framerate constraints
- [ ] **A5: VideoToolbox decoder stub** — `vtb/decoder.rs` is TODO skeleton only

---

## Architecture

### Module Map

```
lib.rs
├── format.rs          — Frame types, presets, pixel formats, MediaPacket
├── traits.rs          — Encoder/Decoder/Source/Sink traits (use MediaPacket)
├── transport.rs       — PacketSource trait, MoqPacketSource, media_pipe
├── pipeline.rs        — VideoDecoderPipeline, VideoEncoderPipeline
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
│   ├── clock.rs       — StreamClock (uses Duration, decoupled from hang)
│   └── mjpg.rs        — MJPEG decoder for camera frames
├── audio_backend.rs   — Firewheel audio I/O + AEC
├── publish.rs         — PublishBroadcast, VideoRenditions, EncoderThread
├── publish/controller.rs — PublishCaptureController
├── subscribe.rs       — SubscribeBroadcast, WatchTrack, AudioTrack
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
- `StreamClock` uses `Duration` instead of `hang::Timestamp`

**Not done:**
- `PacketSink` trait (only `PipeSink` concrete type; `EncoderThread` still uses `OrderedProducer` directly)
- `publish.rs` `EncoderThread` not refactored to use pipeline structs
- Audio pipeline equivalents (`AudioDecoderPipeline`, `AudioEncoderPipeline`)
- Audio path in `subscribe.rs` still converts `OrderedFrame` → `MediaPacket` inline

---

## Bugs

### B1: `Vec::remove(0)` in all encoder packet buffers

**Files**: `h264/encoder.rs:182`, `av1/encoder.rs`, `vaapi/encoder.rs:489`

```rust
fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
    Ok(if self.packet_buf.is_empty() {
        None
    } else {
        Some(self.packet_buf.remove(0))
    })
}
```

`Vec::remove(0)` is O(n) — shifts all remaining elements. Should be `VecDeque` with `pop_front()`. With multiple frames buffered (common for AV1 which buffers ~30 frames before output), this becomes quadratic.

### B2: VAAPI encoder Drop silently loses frames on drain failure

**File**: `vaapi/encoder.rs:494-504`

```rust
impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        if self.encoder.drain().is_ok() {  // silent loss if drain() fails
            while let Ok(Some(coded)) = self.encoder.poll() {
                ...
            }
        }
    }
}
```

If `drain()` fails, all buffered frames are silently lost. Should at least log a warning.

### B3: VAAPI decoder `Display::open()` called per frame download

**File**: `vaapi/decoder.rs:95`

```rust
fn derive_nv12_planes(&self) -> Result<Nv12Planes> {
    let display = Display::open().context("failed to open VAAPI display for mapping")?;
```

Every call to `download()` or `download_nv12()` opens a new `Display` connection to the DRM device. This is expensive (involves DRM authentication). The display should be cached or shared.

### B4: Audio encoder timing re-measurement

**File**: `publish.rs:819-825`

```rust
if actual_time > expected_time {
    warn!("audio encoder too slow by {:?}", actual_time - expected_time);
}
let sleep = expected_time.saturating_sub(start.elapsed());  // re-measured after warn!
```

`start.elapsed()` is called again after the warn log. If the warning takes measurable time (formatting + I/O), the sleep calculation uses a stale `expected_time` against a newer `elapsed`. Should capture elapsed once.

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

### B7: NV12→RGBA shader missing limited-range expansion

**File**: `nv12_to_rgba.wgsl`

The shader treats Y values as full-range (0.0-1.0) but VAAPI output is BT.601 limited range (Y: 16-235, UV: 16-240). Y should be scaled: `y = (y_raw - 16.0/255.0) * (255.0/219.0)`. Produces slightly washed-out blacks and dim whites. The CPU-side `nv12_to_rgba_data` (via yuvutils-rs) handles this correctly with `YuvRange::Limited`.

### B8: No VAAPI→Vulkan synchronization for DMA-BUF

**File**: `render/dmabuf_import.rs`

`drain_events()` calls `handle.sync()` for VAAPI decode completion, but no explicit synchronization primitive (fence/semaphore) between VAAPI write and Vulkan read of the DMA-BUF. The `VK_QUEUE_FAMILY_EXTERNAL` barrier provides an implicit acquire, but the Vulkan spec requires either a fence signaled by the exporter or `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`. Works on Intel (implicit sync) but may fail on explicit-sync-only drivers.

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

### D5: Mutex poisoning — `.expect("poisoned")` throughout

**Files**: `publish.rs` (8 occurrences), `controller.rs:136,202,275,290,295`

If any code panics while holding the mutex, all subsequent `.expect("poisoned")` calls cascade into panics. Consider using `parking_lot::Mutex` (no poisoning) or handling the PoisonError.

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

### D8: BGRA pixel swap still in `from_video_source`

**File**: `subscribe.rs:582-586`

The decode_loop in `pipeline.rs` handles BGRA conversion once cleanly, but the `from_video_source` path in `subscribe.rs` still has its own inline BGRA swap. Should extract to a shared helper.

### D9: VkImage coded vs display dimension mismatch

**File**: `render/dmabuf_import.rs`

The Y plane VkImage is created with `coded_width x coded_height` (e.g., 1920x1088) but wrapped as a wgpu texture with `display_width x display_height` (e.g., 1920x1080). H.264 coded height is commonly rounded up to 16. The wgpu TextureDescriptor size doesn't match the actual VkImage extent — could cause Vulkan validation errors or incorrect sampling.

### D10: Per-frame bind group creation in renderer

**File**: `render.rs:187-204, 306-323`

Both `render_imported_nv12` and `render_nv12` create a new wgpu bind group every frame. These could be cached when texture dimensions haven't changed, avoiding per-frame descriptor set allocation.

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

### P1: YUV allocation per frame

Every encode path does `pixel_format_to_yuv420()` which allocates 3 new Vecs (Y, U, V planes) per frame. At 1080p30 that's ~3MB/frame × 30fps = 90MB/s of allocations. Consider reusing buffers.

### P2: RGBA allocation in decoders

H.264 and AV1 software decoders convert YUV→RGBA into a new `Vec<u8>` per frame, then optionally scale into another `Vec`. Two full-frame allocations per decode.

### P3: Channel conversion allocates unnecessarily

**File**: `opus/decoder.rs`

Even the identity case `(from == to)` does `samples.to_vec()`. Should return a borrowed slice or avoid the copy.

### P4: `parse_annex_b` allocates Vec of slices

**File**: `h264/annexb.rs`

Builds a `Vec<&[u8]>` of all NAL units. Could use an iterator pattern to avoid the allocation.

### P5: StreamClock unused

**File**: `processing/clock.rs`

`StreamClock` computes inter-frame delay but the value is ignored downstream in H.264 and AV1 decoders (they pop frames immediately). The clock adds no value currently. Either use the delay for frame pacing or remove StreamClock.

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

- **A1**: No runtime bitrate adjustment — hardcoded `pixels * 0.07 * framerate_factor` in every encoder
- **A2**: No builder pattern for encoder/decoder configuration
- **A3**: Quality enum coarse — 4 fixed presets, no custom resolution/bitrate
- **A4**: `DecodeConfig` minimal — only `pixel_format` and `backend`
- **A5**: VideoToolbox decoder is stub only — macOS HW decode not available
