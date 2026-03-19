# Codebase review

Open items are at the top, grouped by crate. Completed items and architecture notes are at the bottom.

---

## moq-media

### Bugs

- [ ] **B5**: `watch_local` hardcoded 30 fps — should use source/encoder framerate (`subscribe.rs:554`)
- [ ] **B6**: Per-frame Vulkan resource allocation in DMA-BUF import — pool Y/UV plane images (`render/dmabuf_import.rs`)
- [ ] **B8**: No VAAPI-to-Vulkan sync for DMA-BUF — works on Intel implicit sync, may fail elsewhere (`render/dmabuf_import.rs`)
- [ ] **B9**: `SharedVideoSource` pacing accumulates paused time — resume bursts frames with no sleep (`publish.rs:529-569`)
- [ ] **B10**: Opus encoder ignores requested input sample rate — hardcodes 48k (`codec/opus/encoder.rs:141-147`)

### Design

- [ ] **D2a**: Dual NAL format support (avcC / Annex B) — decoders and encoders should handle both length-prefixed and Annex B H.264 transport modes
- [ ] **D3**: Encoder config by instantiation — creates/drops encoders just for config; VAAPI construction is expensive (`publish.rs:363,475`)
- [ ] **D4**: `SharedVideoSource` park/unpark fragile — replace with condvar or select (`publish.rs:520-586`)
- [ ] **D6**: `apply_audio` fire-and-forget task — no handle, caller cannot observe failure (`controller.rs:281-293`)
- [ ] **D7**: No backpressure encoder-to-transport — frames produced at source rate regardless (`publish.rs:706-732`)
- [ ] **D9**: VkImage coded vs display dimension mismatch — coded height rounded up to 16 but wgpu texture uses display height (`render/dmabuf_import.rs`)

### Performance

- [ ] **P4**: `parse_annex_b` allocates Vec — could use iterator (`h264/annexb.rs`)
- [ ] **P11**: Annex-B / length-prefixed round-trip — each direction allocates a `Vec<u8>` (`h264/annexb.rs`)
- [ ] **P12**: Payload copy in all decoders — `copy_to_bytes()` copies for multi-chunk BufLists (`h264/decoder.rs`, `av1/decoder.rs`)
- [ ] **P16**: Double YUV-RGB-YUV conversion on encode path — NV12 sources go NV12->RGBA->YUV420 (~5 ms/frame at 1080p)
- [ ] **P17**: VAAPI decoder opens four VA Displays per instance — could consolidate pool/mapping/export displays
- [ ] **P18**: Per-frame DMA-BUF re-export in VAAPI decoder — surface export metadata could be cached (`vaapi/decoder.rs:198`)
- [ ] **P19**: `frame.raw.to_vec()` in video source passthrough — copies entire `Bytes` frame when no scaling needed (`subscribe.rs:396`)
- [ ] **P20**: VAAPI encoder `query_image_formats` per frame upload — result is stable, should cache at construction (`vaapi/encoder.rs:126-133`)

### Safety

- [ ] **S1**: `unsafe impl Send` on five-plus types — no runtime thread-ID assertions (`vaapi/encoder.rs`, `vaapi/decoder.rs`, `opus/encoder.rs`, `opus/decoder.rs`, `av1/rav1d_safe.rs`)
- [ ] **S2**: rav1d_safe FFI bounds — `copy_nonoverlapping` without bounds check, `assume_init` after C call (`av1/rav1d_safe.rs`)
- [ ] **S3**: DMA-BUF fd ownership complexity — correct but no RAII wrapper for dup'd fds (`render/dmabuf_import.rs`)
- [ ] **S4**: VTB encoder Arc callback dance — `Arc::into_raw`/`from_raw` in C callback (`vtb/encoder.rs:83`)

### Testing

- [ ] **T3**: No `render.rs` tests — `WgpuVideoRenderer` untested (needs GPU)
- [ ] **T5**: `PublishCaptureController` not tested — `set_opts` has no tests
- [ ] **T6**: No fuzz tests — codec decoders not fuzzed with malformed input

### API

- [ ] **A3**: Quality enum is coarse — four fixed presets, no custom resolution/bitrate
- [ ] **A4**: `DecodeConfig` minimal — only `pixel_format` and `backend`, no resolution/framerate constraints
- [ ] **A6**: No `set_keyframe_interval` on `VideoEncoder` trait — all encoders hardcode interval at construction
- [ ] **A7**: `AudioEncoderFactory::with_preset` contract ambiguous on sample-rate handling — Opus hardcodes 48k

### Audio Backend

- [ ] **AB1**: ~30 `.unwrap()` calls in `audio_backend.rs` — should propagate `Result` when no audio device present
- [ ] **AB2**: `.expect("poisoned")` on every mutex lock in `OutputStream` — Firewheel callbacks could panic and poison them

### Transport

- [x] **TR1**: Doc comments on `media_pipe()`, `PipeSink`, `PipeSource` — already present

### Stubs

- [x] **ST1**: `VideoPublisher::set_enabled()` — documented as unimplemented stub
- [x] **ST2**: `AudioPublisher::set_muted()` — documented as unimplemented stub

### Concurrency

- [ ] **CC1**: `SharedVideoSource` park/unpark race — thread can hang if broadcast drops while parked (`publish.rs:737-859`)
- [ ] **CC2**: `PlayoutClock` mutex acquired every frame — lock contention if clock shared across tracks (`playout.rs:206-259`)
- [ ] **CC3**: AEC ring buffer `.unwrap()` in tight loop (`audio_backend/aec.rs:306-331`)

### Performance (additional)

- [ ] **PP1**: `PlayoutClock::mode()` clones entire `PlayoutMode` on every call (`playout.rs:114`)
- [ ] **PP2**: `AudioBackendOpts` fully cloned for device switching (`audio_backend.rs:697-699`)

### Error Handling

- [ ] **EH1**: Adaptive rendition switch failures logged but state not reset — could lead to stuck selections (`adaptive.rs`)
- [ ] **EH2**: `AudioDriver::new()` unwraps on CPAL stream start — panics if audio device unavailable (`audio_backend.rs:489`)
- [x] **EH3**: Inconsistent lock panic messages — normalized `playout.rs` from `.expect("lock")` to `.expect("poisoned")`

### New findings (2026-03-18)

- [ ] **B12**: `VideoEncoderPipeline` hardcodes 30 fps frame duration — ignores encoder config framerate (`pipeline.rs:260`)
- [ ] **B13**: `AdaptiveVideoTrack::format()` returns zeroed `[0, 0]` dimensions — breaks callers using format for layout (`adaptive.rs:374-378`)
- [ ] **D11**: `set_enabled()` / `set_muted()` are public no-ops — should be removed, return `Err`, or implemented (`publish.rs:324,361`)
- [ ] **D12**: Pipeline thread panics are invisible — no panic hook or error channel, callers see frozen stream (`pipeline.rs`)
- [ ] **D13**: `VideoPublisher::set()` and `replace()` are identical — confusing duplicate API surface (`publish.rs`)
- [ ] **D14**: PlayoutClock leaked into public API — users must call `.clock().set_buffer()` instead of `RemoteBroadcast` methods (`subscribe.rs`)
- [ ] **D15**: `LocalBroadcast::producer()` exposes internal `BroadcastProducer` — bypasses catalog/rendition safety (`publish.rs`)
- [ ] **D16**: No observability in AdaptiveVideoTrack — no way to query probe state or decision reasons (`adaptive.rs`)
- [ ] **D17**: Adaptive rendition switch failure loops forever with no backoff (`adaptive.rs:561-570`)

---

## iroh-moq

### Bugs

- [ ] **MQ1**: Two `.expect()` panics in actor run loop — task panic crashes actor (`lib.rs:414, 422`)

### Design

- [ ] **MQ4**: `MoqSession::subscribe(&mut self)` takes `&mut self` unnecessarily — prevents concurrent subscriptions
- [x] **MQ5**: `publish()` takes `String` by value — changed to `impl ToString` for consistency
- [ ] **MQ6**: Channel capacities hardcoded — no sizing rationale, high-churn scenarios silently drop sessions (`lib.rs:79-80`)
- [ ] **MQ7**: Error context lost in conversions — `Error` -> `AnyError` -> `AcceptError` discards specific variants (`lib.rs:165`)
- [ ] **MQ8**: `handle_publish_broadcast` clones name per session — should clone once outside loop (`lib.rs:493-496`)

### Documentation

- [x] **MQ9**: Module-level doc comment added to `lib.rs`
- [x] **MQ10**: `session_connect()` and `session_accept()` — already documented

### Error Handling

- [ ] **MQ11**: `published_broadcasts()` swallows actor death — returns empty vec if actor dies (`lib.rs:123`)
- [ ] **MQ12**: Silent `.ok()` on critical oneshot sends (`lib.rs:433, 453, 466, 468, 511`)
- [ ] **MQ13**: `subscribe()` loops forever if name never announced — no timeout or cancellation (`lib.rs:322-334`)

### Code Quality

- [x] **MQ14**: Empty `#[derive()]` on Actor struct — removed

---

## iroh-live

### Bugs

- [ ] **IL1**: `Call::closed()` always returns `RemoteClose` — ignores actual disconnect reason (`call.rs:122-125`)

### Design

- [x] **IL3**: `postcard::to_stdvec().unwrap()` — replaced with `.expect()` with infallibility messages
- [ ] **IL5**: Room actor silently drops events on send failure — should detect and shut down (`rooms.rs:233, 298`)
- [ ] **IL6**: `broadcasts.clone()` in room actor — marginal savings possible (`rooms.rs:284`)
- [x] **IL8**: `RoomEvent::RemoteConnected` — documented as reserved/not emitted
- [ ] **IL9**: `Call::dial()` and `Call::accept()` duplicate code — should extract shared helper (`call.rs:51-94`)
- [ ] **IL10**: `spawn_thread()` always panics on failure — should return `Result<JoinHandle>` (`util.rs:23`)

### Documentation

- [x] **IL7**: Missing docs — added doc comments to `Room`, `RoomHandle`, `RoomTicket`, `RoomEvent` variants, `RoomPublisherSync`, `LiveTicket::from_bytes`

### New findings (2026-03-18)

- [ ] **IL11**: `LiveTicket::to_bytes()` and `serialize()` call `.unwrap()` on postcard — panics in `Display` impl are unrecoverable (`ticket.rs:46,56`)
- [ ] **IL12**: Two serialization APIs on tickets (`to_bytes`/`from_bytes` vs `serialize`/`deserialize`) — confusing, consolidate (`ticket.rs`)
- [ ] **IL13**: Room gossip dependency is implicit — `Room::new()` fails at runtime if gossip not enabled, no type-level guard (`rooms.rs`)
- [ ] **IL14**: `Live::subscribe()` returns tuple `(MoqSession, RemoteBroadcast)` — unclear ownership, should wrap (`live.rs`)
- [ ] **IL15**: `spawn()` vs `spawn_with_router()` fork is confusing — user can forget to mount protocols (`live.rs`)

---

## rusty-codecs

### Safety

- [x] **RC8**: `NonNull::new().unwrap()` in rav1d_safe — replaced with `.expect()` and SAFETY comment

### Concurrency

- [ ] **RC9**: Lock poison not handled in V4L2 encoder — `ts_cb.lock().unwrap()` panics on poisoned lock (`v4l2/encoder.rs:382, 397`)
- [ ] **RC10**: Lock poison not handled in VAAPI decoder — same issue in decode hot paths (`vaapi/decoder.rs:391, 485, 522`)

### Performance

- [ ] **RC11**: Ash Device/Instance cloned in DMA-BUF importer — could use `Arc` or borrow (`render/dmabuf_import.rs:83, 85`)
- [ ] **RC12**: Per-frame TextureView + BindGroup in imported NV12 path — should cache when fd/modifier match (`render.rs:225-244`)
- [ ] **RC13**: `Bit::from_f32().unwrap()` timestamp overflow — could overflow for extreme PTS values (`config.rs:338`)
- [ ] **RC15**: Android MediaCodec encoder uses ByteBuffer mode with CPU scaling — should use Surface input mode for zero-copy

### Documentation

- [ ] **RC14**: Minimal SAFETY comments on VAAPI encoder unsafe blocks (`vaapi/encoder.rs:405, 490, 507, 695`)

### New findings (2026-03-18)

- [ ] **RC16**: VAAPI device path hardcoded to `/dev/dri/renderD128` — multi-GPU or renamed nodes fail silently (`vaapi/encoder.rs`, `vaapi/decoder.rs`, `ffmpeg/decoder.rs:66`)
- [ ] **RC17**: Opus pre-skip always zero — standard encoder delay is 312 samples, can cause A/V sync drift (`opus/encoder.rs:210`)
- [x] **RC18**: Vulkan command pool not destroyed on DmaBufImporter drop — verified: already cleaned up in Drop impl (`render/dmabuf_import.rs:949`)
- [ ] **RC19**: AV1 decoder stride assumption — no assert that `picture.stride() >= width` (`av1/decoder.rs:87-89`)
- [ ] **RC20**: FFmpeg `get_extradata()` lacks safety comments on raw pointer dereference (`ffmpeg/encoder.rs:612-625`)
- [x] **RC21**: V4L2 decoder `copy_plane()` allocates Vec even when stride == width — fixed: returns `Cow<[u8]>` for zero-copy fast path
- [ ] **RC22**: VAAPI VPP retiler re-initializes on every incompatible frame — should set initialized flag on error to stop retrying (`render/dmabuf_import.rs:157-168`)

### rusty-capture

- [ ] **CAP1**: PipeWire `parse_format_pod()` dereferences raw pointer without bounds validation — malformed pod causes UB (`pipewire.rs:391-405`)
- [ ] **CAP2**: X11 `stop_capture()` ignores `shm::detach()` and `shmdt()` errors — SHM segment leaked without logging (`x11.rs:269-277`)
- [ ] **CAP3**: `CameraConfig::select_format()` falls back silently when preferred format unavailable — not documented (`types.rs:277-286`)
- [ ] **CAP4**: PipeWire frame drop logging at power-of-two intervals — can miss hundreds of drops between messages (`pipewire.rs:910-918`)
- [ ] **CAP5**: No test for V4L2 DMA-BUF export path (`VIDIOC_EXPBUF`) — regression risk on driver changes
- [ ] **CAP6**: PipeWire portal thread panics are unobserved — `result_rx.recv_timeout()` catches timeout but not panic (`pipewire.rs:1027-1036`)

---

## moq-media-android

- [ ] **MA1**: `CameraFrameSource` allocates per-frame — could pool frame buffers when resolution is stable
- [ ] **MA2**: `SharedCameraSource` uses `std::sync::Mutex` — `parking_lot::Mutex` would avoid poisoning
- [ ] **MA3**: EGL function pointers resolved lazily via `OnceLock` — no error recovery if `dlopen` fails
- [ ] **MA4**: No unit tests — camera source, handle helpers, and EGL wrappers all untested

---

## android-demo

- [ ] **AD1**: `connect_impl` still ~90 lines — could extract publish/subscribe setup into separate functions
- [ ] **AD2**: `renderFrame` does GL calls without EGL context check — debug assertion would catch misuse
- [ ] **AD3**: `JNI_OnLoad` does not warm up cpal/Oboe — lazy init causes latency spike on first call
- [ ] **AD4**: No JNI exception checking — same as ON14
- [ ] **AD5**: Hardcoded 640x480 camera resolution — should accept from Kotlin or negotiate with CameraX

---

## Open (overnight review, general)

- [ ] **ON11**: Hardcoded H.264 profile/level in Android encoder `config()` — returns fixed values regardless of MediaCodec negotiation
- [ ] **ON13**: `set_bitrate` never takes effect — only applies on codec reset after three consecutive errors
- [ ] **ON14**: JNI exception checking — no `exception_check()`/`exception_clear()` after JNI calls; pending exception + continued JNI calls = undefined behavior
- [ ] **ON21**: Commit 83d0b16 bundles unrelated changes — noted for future practice
- [ ] **ON23**: UI thread blocking in `split.rs` `resubscribe()` — `block_on()` in egui UI method; acceptable for example code

---

# Completed

## moq-media

### API Decoupling (D1, D2)

- [x] **D1a**: `MediaPacket` type replacing `OrderedFrame` in traits
- [x] **D1b**: `VideoDecoder`/`AudioDecoder` traits use `MediaPacket`
- [x] **D1c**: All codec impls updated (h264, av1, vaapi, opus)
- [x] **D1d**: `PacketSource` trait + `MoqPacketSource` wrapper
- [x] **D1e**: `media_pipe()` / `PipeSink` / `PipeSource` for local encode-decode
- [x] **D1f**: `VideoDecoderPipeline` struct
- [x] **D1g**: `VideoEncoderPipeline` struct
- [x] **D1h**: `subscribe.rs` uses `VideoDecoderPipeline`
- [x] **D1i**: `StreamClock` removed (dead code)
- [x] **D1j**: `PacketSink` trait
- [x] **D1k**: Refactor `publish.rs` `EncoderThread` to use `PacketSink`
- [x] **D1l**: `AudioDecoderPipeline` / `AudioEncoderPipeline`
- [x] **D1m**: `subscribe.rs` `AudioTrack` uses `MediaPacket` throughout
- [x] **D1n**: `subscribe.rs` audio path uses `PacketSource`/`forward_packets`
- [x] **D1o**: Pipeline consolidation — all encode/decode loops in `pipeline.rs`
- [x] **D2**: `EncodedFrame` transport-independent with `to_hang_frame()` at boundary

### Bugs

- [x] **B1**: `Vec::remove(0)` in encoder packet buffers — changed to `VecDeque::pop_front()`
- [x] **B2**: VAAPI encoder Drop silently loses frames — now logs warning
- [x] **B3**: VAAPI `Display::open()` per frame download — cached via `OnceCell`
- [x] **B4**: Audio encoder timing re-measurement — captured once and reused
- [x] **B7**: NV12-RGBA shader missing limited-range expansion — BT.601 scaling added
- [x] **B11**: `watch_local` swallows scaler errors — now logs warning on `Err`

### Design

- [x] **D8**: BGRA pixel swap code removed — decoders output native BGRA when requested
- [x] **D10**: Per-frame bind group creation — cached on `Nv12PlaneTextures`, recreated only on resolution change

### Performance

- [x] **P0a**: `MediaPacket.payload` eager copy — now uses `BufList` moved directly
- [x] **P0b**: `DecodedVideoFrame::img()` cloned pixel buffer — `CpuFrame` stores `RgbaImage` directly
- [x] **P1**: Per-frame YUV allocation — `take_owned()` eliminates `.to_vec()`
- [x] **P2**: Per-frame RGBA allocation — reuses `pixel_buf` across frames
- [x] **P3**: Opus channel conversion allocates on identity — skips when from==to
- [x] **P5**: `StreamClock` unused — removed dead code
- [x] **P6**: Opus encoder front-removal on `Vec` — changed to `VecDeque`
- [x] **P7**: VideoToolbox packet queue `Vec::remove(0)` — changed to `VecDeque`
- [x] **P8**: Scaler clones source buffer — `ImageStore::from_slice()` accepts borrowed input
- [x] **P9**: VAAPI encoder I420-NV12 intermediate — converts RGBA/BGRA directly to NV12
- [x] **P10**: Opus decoder per-packet PCM allocation — reusable `pcm_buf` on struct
- [x] **P13**: Opus encoder per-frame output allocation — reusable `encode_buf` field
- [x] **P14**: VAAPI decoder NV12 plane copy via iterator — pre-allocated Vecs with `copy_from_slice`
- [x] **P15**: VAAPI encoder I420-NV12 bounds-checked per pixel — direct indexing with pre-allocated output

### Testing

- [x] **T1**: `subscribe.rs` tests — eight tests for RemoteBroadcast, VideoTrack, AudioTrack
- [x] **T2**: Audio decode loop tests — two AudioDecoderPipeline tests
- [x] **T4**: Integration tests — 15 tests in `pipeline_integration.rs`
- [x] **T7**: Pipeline tests — subscribe tests exercise pipelines indirectly

### API

- [x] **A1**: Encoder rate control — `set_bitrate()` on `VideoEncoder`/`AudioEncoder` traits
- [x] **A2**: Builder pattern for encoder configuration — `VideoEncoderConfig`/`AudioEncoderConfig`
- [x] **A5**: VideoToolbox decoder implemented — NV12 GpuFrame output, dynamic decoder routing, mid-stream resolution change

### Good API design

- [x] Trait-based codec abstraction: `VideoEncoder`, `VideoDecoder`, `AudioEncoder`, `AudioDecoder`
- [x] Dynamic dispatch: `DynamicVideoDecoder` auto-selects HW vs SW codec
- [x] `DecodedVideoFrame`: CPU/GPU duality with lazy download caching via `OnceCell`
- [x] Rendition system: `VideoRenditions`/`AudioRenditions` with factory callbacks
- [x] Watchable state: reactive observation via `n0_watcher`
- [x] `GpuFrameInner` trait: extensible platform abstraction for GPU frames
- [x] `PacketSource` trait + `media_pipe`: clean decoupling of decode from transport
- [x] `VideoCodec::create_encoder()`: dynamic encoder creation by codec enum

### Good test coverage

- [x] Codec roundtrip tests (encode-decode) for every preset: H.264, AV1, Opus
- [x] Cross-channel audio tests (mono encode to stereo decode)
- [x] Dynamic decoder routing tests
- [x] Processing: YUV conversion edge cases, scaling, resampling, MJPEG corruption
- [x] Publish: `watch_local`, `set_video`, encoder pipeline end-to-end, rendition ordering
- [x] Hardware codec tests (marked `#[ignore]`)

## iroh-moq

- [x] **MQ2**: Malformed doc comment — false positive, comment is correct
- [x] **MQ3**: Commented-out code — removed dead `MoqSession::connect` comment

## iroh-live

- [x] **IL2**: `Bit::from_f32(rate).unwrap()` can panic on NaN — changed to `.unwrap_or_default()`
- [x] **IL4**: Stale TODO comments removed (`live.rs:173, 187`)

## rusty-codecs

- [x] **RC1**: `unimplemented!()` in `VideoFrame::rgba_image()` — NV12 and I420 implemented
- [x] **RC2**: `panic!()` in `VideoCodec::best_available()` — returns `Option`
- [x] **RC3**: `.expect()` calls in `alloc_va_dma_frame()` — returns `Result`
- [x] **RC4**: `.expect()` in `WgpuVideoRenderer::render()` — returns `Result`
- [x] **RC5**: Multiple `.unwrap()` on `output_texture` — replaced with error propagation
- [x] **RC6**: VTB encoder CFDictionary `.expect()` — returns `Result`
- [x] **RC7**: `take_owned()` assumes `BufferStoreMut::Owned` — invariant comment added

## Overnight review

- [x] **ON1**: Audio restart backoff never grows — added `restart_backoff` with exponential growth
- [x] **ON2**: xcap `pop_frame` blocks with `thread::sleep` — returns `None` if interval hasn't elapsed
- [x] **ON3**: Dead fallback in `CameraCapturer::with_backend`
- [x] **ON4**: `StreamDropGuard::drop` silent failure — added `debug!` logging
- [x] **ON5**: Android decoder drops intermediate frames — changed to `VecDeque<VideoFrame>`
- [x] **ON6**: Android encoder uses synthetic timestamps — now uses actual `frame.timestamp`
- [x] **ON7**: JNI `nextFrame` returns true on truncated data — now returns `JNI_FALSE`
- [x] **ON8**: JNI `pushCameraFrame` no size validation — added `width * height * 4` check
- [x] **ON9**: JNI `handle_from_jlong` panic safety — `ManuallyDrop` instead of `forget`
- [x] **ON10**: README says CameraX, code uses Camera2
- [x] **ON12**: `keyframe_interval_secs as i32` truncation — `round().max(1)` prevents truncation to 0
- [x] **ON15**: Kotlin `MainActivity` races with render loop — `onDisconnect` cancels render job first
- [x] **ON16**: Kotlin `yuvToRgba` pixel-by-pixel CPU loop — documented limitation; main path uses direct NV12
- [x] **ON17**: TOCTOU in JNI `startPublish` — removed dangling declaration
- [x] **ON18**: PipeWire thread `join()` in Drop blocks indefinitely — `join_with_timeout` with 2 s limit
- [x] **ON19**: `unsafe impl Send` SAFETY comments added
- [x] **ON20**: `CaptureBackend` `#[non_exhaustive]` added
- [x] **ON22**: `bitrate as i32` clamped to `i32::MAX`

## android-demo cleanup

- [x] `SessionHandle` field naming — renamed to descriptive names with `#[allow(dead_code)]`
- [x] JNI helper consolidation — `to_jlong`, `borrow_handle`, `take_handle`, `read_jstring`
- [x] `getStatusLine` simplified — removed `RefCell` hack
- [x] EGL JNI wrappers — replaced `match` with `.map_or()`
- [x] `get_native_handle` — `.and_then` chain instead of nested `match`
- [x] `connect_impl` — `.inspect_err().ok()` for optional audio subscription
- [x] Section headers — clear comment separators for JNI entry points

## moq-media-android extraction

- [x] `camera` module (~110 lines) — `CameraFrameSource` / `SharedCameraSource`
- [x] `egl` module (~172 lines) — safe EGL/GLES wrappers for HardwareBuffer import
- [x] `handle` module (~37 lines) — `Arc<Mutex<T>>` to `i64` JNI handle helpers

---

## Architecture notes

### Module map

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

### Threading model

- **Video encoders**: OS thread per active rendition (source -> scale -> encode -> write)
- **Video decoders**: OS thread per subscribed track (read -> decode -> frame output)
- **Audio encoders**: OS thread per rendition (20 ms tick interval)
- **Audio decoders**: OS thread (10 ms tick interval)
- **Catalog watcher**: Tokio task (async polling)
- **Frame forwarding**: Tokio task bridging async `PacketSource` -> mpsc -> sync decoder thread
- **`SharedVideoSource`**: Single OS thread with park/unpark, fan-out via `watch::channel`
- **Audio backend**: OS thread running Firewheel graph
