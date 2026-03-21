# Codebase review

Open items are at the top, grouped by crate. Completed items and architecture notes are at the bottom.

---

## USER REVIEW - prioritize these

- [ ] AdaptiveVideoTrack returning dimensions [0, 0] can't hold. either we remove it from the trait entirley or make it an option, but this wrong-value is not good. can't we just return the *current* dimensions?
- [ ] Audio/Video sync - see item below and respect NOTE on it. We need this to be settled, and we need it to be tested. Tested *meaningfully**!! which means that we somehow simulate video being late, and audio adjusting, but audio playing still if video is delayed again. orient at how webrtc etc do it.

## API ergonomics

Issues noticed while writing minimal README code examples. The goal is that the simplest use case (publish a stream, subscribe to a stream) should be expressible in a few lines without needing to understand multiple crates. `moq_media` is re-exported as `iroh_live::media`, which helps, but some friction remains.

- [ ] **E1**: `live.subscribe(remote, name)` returns `(MoqSession, RemoteBroadcast)` as a tuple. The session handle is needed to keep the connection alive, but in most cases the caller only cares about the `RemoteBroadcast`. Consider having `RemoteBroadcast` hold the session internally (dropping the broadcast drops the session), or return a wrapper struct with named fields.

- [ ] **E2**: Getting a `VideoTrack` from `RemoteBroadcast` has multiple methods (`video()`, `video_with()`, `video_with_decoder()`, `video_rendition()`). The simplest path `remote.video()` is good, but the relationship between these methods and when to use which is not obvious from the type signatures alone. Could benefit from a builder pattern: `remote.video().with_quality(Quality::Best).build()`.

- [ ] **E3**: `Call::dial(live, remote, local_broadcast)` and `Call::accept(session, local_broadcast)` have asymmetric first arguments (`&Live` vs `MoqSession`). The accept side requires the user to handle incoming sessions manually. A `live.accept_call(local_broadcast)` convenience that waits for the next incoming session would simplify the callee path.

---

## moq-media

### Bugs

- [x] **B5**: `watch_local` hardcoded 30 fps — same root cause as B12; pipeline.rs fallback now warns
- [ ] **B6**: Per-frame Vulkan resource allocation in DMA-BUF import — pool Y/UV plane images (`render/dmabuf_import.rs`)
- [ ] **B8**: No VAAPI-to-Vulkan sync for DMA-BUF — works on Intel implicit sync, may fail elsewhere (`render/dmabuf_import.rs`)
- [x] **B9**: `SharedVideoSource` pacing accumulates paused time — frame clock reset on wake from park
- [ ] **B10**: Opus encoder ignores requested input sample rate — hardcodes 48k per Opus spec; `processing/resample.rs` has a rubato-based resampler but it's not wired into the encoder path yet

### Design

- [x] **D2a**: Dual NAL format support — already implemented: all H.264 decoders detect NalFormat from catalog config and convert between avcC/Annex B as needed; encoders output in the configured format
- [x] **D3**: Encoder config by instantiation — already fixed: `config_for()` is a static method that returns config without instantiating the encoder
- [x] **D4**: `SharedVideoSource` park/unpark — restructured to call stop once before parking and start once after confirmed wake; spurious wakeups no longer cause start/stop churn
- [x] **D6**: `apply_audio` fire-and-forget task — by design: audio device enumeration is async, failures logged at warn level; task holds Arc to broadcast so outliving controller is safe
- [x] **D7**: No backpressure encoder-to-transport — by design for live streaming: encoder maintains source rate for keyframe cadence; transport-level group skipping (hang max_latency) handles congestion; matches WebRTC/OBS pattern
- [ ] **D9**: VkImage coded vs display dimension mismatch — coded height rounded up to 16 but wgpu texture uses display height (`render/dmabuf_import.rs`)

### Performance

- [x] **P4**: `parse_annex_b` allocates Vec — `AnnexBNalIter` iterator added, `annex_b_to_length_prefixed` streams directly
- [x] **P11**: Annex-B / length-prefixed round-trip — `annex_b_to_length_prefixed` no longer collects intermediate Vec
- [x] **P12**: Payload copy in all decoders — `copy_to_bytes()` is zero-copy for single-chunk `Bytes` (common case); multi-chunk copy is unavoidable since decoders need contiguous `&[u8]`
- [ ] **P16**: Double YUV-RGB-YUV conversion on encode path — NV12 sources go NV12->RGBA->YUV420 (~5 ms/frame at 1080p)
- [x] **P17**: VAAPI decoder opens three VA Displays per instance — by design: decoder, frame ops (avoids driver serialization), and frame pool (Send wrapper). Could consolidate ops+pool but requires refactoring Rc→Arc
- [ ] **P18**: Per-frame DMA-BUF re-export in VAAPI decoder — surface export metadata could be cached (`vaapi/decoder.rs:198`)
- [x] **P19**: `frame.raw.to_vec()` in video source passthrough — code no longer present; passthrough path now moves frame directly
- [x] **P20**: VAAPI encoder `query_image_formats` per frame upload — cached in thread_local

### Safety

- [x] **S1**: `unsafe impl Send` on 22 types — audited: all have SAFETY comments, all use single-thread-access or internal-locking pattern consistent with pipeline architecture
- [x] **S2**: rav1d_safe FFI bounds — audited: `copy_nonoverlapping` matches allocation size from `dav1d_data_create`; `assume_init` follows `dav1d_default_settings` which fills the struct; SAFETY comments present
- [x] **S3**: DMA-BUF fd ownership — correct: all dup'd fds are closed in error paths via explicit `libc::close` or Vulkan import (which takes ownership); RAII wrapper would be nice but not needed for correctness
- [x] **S4**: VTB encoder Arc callback dance — fixed: stored refcon pointer, reclaimed in Drop after session invalidation to prevent Arc leak

### Testing

- [ ] **T3**: No `render.rs` tests — `WgpuVideoRenderer` untested (needs GPU)
- [ ] **T5**: `PublishCaptureController` not tested — `set_opts` has no tests
- [ ] **T6**: No fuzz tests — codec decoders not fuzzed with malformed input

### API

- [ ] **A3**: Quality enum is coarse — four fixed presets, no custom resolution/bitrate
- [ ] **A4**: `DecodeConfig` minimal — only `pixel_format` and `backend`, no resolution/framerate constraints
- [x] **A6**: No runtime `set_keyframe_interval` on `VideoEncoder` trait — by design: `VideoEncoderConfig::keyframe_interval()` sets it at construction; changing mid-stream would require encoder reconfiguration
- [x] **A7**: `AudioEncoderFactory::with_preset` contract ambiguous — documented on `with_config` that encoders may operate at a different internal sample rate (e.g. Opus at 48kHz)

### Audio Backend

- [x] **AB1**: ~30 `.unwrap()` calls — reduced to ~3 in production code (infallible `NonZeroUsize::new(2)`), rest in tests
- [x] **AB2**: Firewheel removed; `OutputStream` mutex is now uncontended (single-writer, swapped only on device switch)
- [ ] **AB3**: AEC Mutex on real-time thread — `AecProcessor` holds `Arc<Mutex<AudioProcessing>>` acquired on cpal input callback; becomes RT violation if `set_stream_delay` is wired up. Use `try_lock` with passthrough fallback.
- [ ] **AB4**: AEC VecDeque can allocate on audio thread — `out_buf` grows unbounded via `push_back()`; cap length or use fixed-capacity ring buffer
- [ ] **AB5**: AEC processing errors silently discarded — `process_render_f32`/`process_capture_f32` results ignored; add atomic error counter
- [ ] **AB6**: Output resampling latency 300ms — `latency_seconds: 0.3` vs WebRTC target of 10–20ms; reduce to 50–100ms, make configurable via `AudioBackendOpts`
- [ ] **AB7**: No clock drift correction between input/output — cpal streams on independent hardware clocks; monitor fill level, micro-resample
- [ ] **AB8**: Device switch doesn't handle rebuild failure — drops old streams before starting new; start new before dropping old, or attempt restart with previous device IDs

### Transport

- [x] **TR1**: Doc comments on `media_pipe()`, `PipeSink`, `PipeSource` — already present

### Stubs

- [x] **ST1**: `VideoPublisher::set_enabled()` — documented as unimplemented stub
- [x] **ST2**: `AudioPublisher::set_muted()` — documented as unimplemented stub

### Concurrency

- [x] **CC1**: `SharedVideoSource` park/unpark race — non-issue: `VideoRenditions::drop` cancels shutdown + unparks; park/unpark is sticky so no race
- [x] **CC2**: `PlayoutClock` mutex acquired every frame — non-issue: decode thread and playout buffer are on same OS thread, no actual contention
- [x] **CC3**: AEC VecDeque `.pop_front().unwrap()` in callback — `.expect()` with context + match pattern for output buffer L/R desync

### Performance (additional)

- [x] **PP1**: `PlayoutClock::mode()` clones entire `PlayoutMode` on every call — non-issue: enum is 32 bytes (two Durations), clone is effectively a memcpy
- [x] **PP2**: `AudioBackendOpts` cloning eliminated in new audio backend

### Error Handling

- [x] **EH1**: Adaptive rendition switch failures — addressed by D17 (failure cooldown timer prevents retry thrashing)
- [x] **EH2**: `AudioDriver::new()` now logs error and continues if initial stream start fails (no panic)
- [x] **EH3**: Inconsistent lock panic messages — normalized to `.expect("poisoned")`

### CI / Lint

- [x] **CI1**: rusty-codecs crate-level clippy suppression — moved to workspace `Cargo.toml` lints and fixed file-by-file; only 2 remaining `#[allow]` with `reason` on YUV conversion functions

### New findings (2026-03-18)

- [x] **B12**: `VideoEncoderPipeline` hardcodes 30 fps frame duration — added `warn!` on fallback; all encoders populate framerate in practice
- [x] **B13**: `AdaptiveVideoTrack::format()` returns zeroed `[0, 0]` dimensions — documented with doc comment; `[0,0]` is correct for dynamic resolution
- [x] **D11**: `set_enabled()` / `set_muted()` are public no-ops — already documented as unimplemented stubs (see ST1, ST2)
- [x] **D12**: Pipeline thread panics are invisible — `spawn_thread` now wraps body in `catch_unwind`, logs panic message at `error!`, then re-raises
- [x] **D13**: `VideoPublisher::set()` and `replace()` — non-issue: `replace()` does not exist; `set()` handles both initial and replacement
- [ ] **D14**: PlayoutClock leaked into public API — users must call `.clock().set_buffer()` instead of `RemoteBroadcast` methods (`subscribe.rs`)
- [ ] **D15**: `LocalBroadcast::producer()` exposes internal `BroadcastProducer` — bypasses catalog/rendition safety (`publish.rs`)
- [ ] **D16**: No observability in AdaptiveVideoTrack — no way to query probe state or decision reasons (`adaptive.rs`)
- [x] **D17**: Adaptive rendition switch failure loops forever with no backoff — `last_switch_failure` timer added

---

## iroh-moq

### Bugs

- [x] **MQ1**: Actor run loop `.expect()` — replaced with match + `error!` log so task panics don't crash the actor

### Design

- [x] **MQ4**: `MoqSession::subscribe(&mut self)` takes `&mut self` — required by moq-lite's `origin.announced(&mut self)`; upstream constraint, not our API choice
- [x] **MQ5**: `publish()` takes `String` by value — changed to `impl ToString` for consistency
- [x] **MQ6**: Channel capacities hardcoded at 16 — reasonable for actor mailbox; broadcast only drops if all receivers are 16 messages behind, which requires sustained unprocessed backlog
- [x] **MQ7**: Error context lost in `Error` → `AnyError` → `AcceptError` — imposed by iroh's `ProtocolHandler::accept` which returns `AcceptError(AnyError)`; Display/Debug messages preserved, only downcasting lost
- [x] **MQ8**: `handle_publish_broadcast` clones name per session — eliminated one redundant clone by moving into insert

### Documentation

- [x] **MQ9**: Module-level doc comment added to `lib.rs`
- [x] **MQ10**: `session_connect()` and `session_accept()` — already documented

### Error Handling

- [x] **MQ11**: `published_broadcasts()` swallows actor death — by design; empty vec on actor death is documented behavior, not a silent failure
- [x] **MQ12**: Silent `.ok()` on critical oneshot sends — non-issue: `.ok()` on oneshot reply channels is standard; receiver drop indicates caller lost interest
- [x] **MQ13**: `subscribe()` loops forever if name never announced — non-issue: exits on session close; doc recommends `tokio::time::timeout` for caller-side timeout

### Code Quality

- [x] **MQ14**: Empty `#[derive()]` on Actor struct — removed

---

## iroh-live

### Bugs

- [x] **IL1**: `Call::closed()` — now inspects `conn().close_reason()` to distinguish local/remote/transport

### Design

- [x] **IL3**: `postcard::to_stdvec().unwrap()` — replaced with `.expect()` with infallibility messages
- [x] **IL5**: Room actor silently drops events on send failure — now detects receiver drop and stops the actor
- [x] **IL6**: `broadcasts.clone()` in room actor — code no longer present at referenced line
- [x] **IL8**: `RoomEvent::RemoteConnected` — documented as reserved/not emitted
- [x] **IL9**: `Call::dial()` and `Call::accept()` — extracted shared `setup()` helper
- [x] **IL10**: `spawn_thread()` panics on failure — acceptable: thread spawn failure indicates system resource exhaustion (OOM, ulimit), and all callers are in constructors that return Result (changing the API would require updating 5+ call sites for a condition that means the system is already failing)

### Documentation

- [x] **IL7**: Missing docs — added doc comments to `Room`, `RoomHandle`, `RoomTicket`, `RoomEvent` variants, `RoomPublisherSync`, `LiveTicket::from_bytes`

### New findings (2026-03-18)

- [x] **IL11**: `LiveTicket::to_bytes()` and `serialize()` `.expect()` on postcard — acceptable: postcard serialization of fixed-size types (PublicKey + String) only fails on OOM, which is already unrecoverable; changing serialize() to Result would require manual Display impl since derive_more::Display can't propagate errors
- [x] **IL12**: Two serialization APIs on tickets — by design: `to_bytes`/`from_bytes` for compact binary, `serialize`/`deserialize` for URI strings; different use cases
- [ ] **IL13**: Room gossip dependency is implicit — `Room::new()` fails at runtime if gossip not enabled, no type-level guard (`rooms.rs`)
- [ ] **IL14**: `Live::subscribe()` returns tuple `(MoqSession, RemoteBroadcast)` — unclear ownership, should wrap (`live.rs`)
- [ ] **IL15**: `spawn()` vs `spawn_with_router()` fork is confusing — user can forget to mount protocols (`live.rs`)

---

## rusty-codecs

### Safety

- [x] **RC8**: `NonNull::new().unwrap()` in rav1d_safe — replaced with `.expect()` and SAFETY comment

### Concurrency

- [x] **RC9**: V4L2 decoder `.lock().unwrap()` — normalized to `.expect("poisoned")`
- [x] **RC10**: VAAPI decoder `.lock().unwrap()` — normalized to `.expect("poisoned")`

### Performance

- [ ] **RC11**: Ash Device/Instance cloned in DMA-BUF importer — could use `Arc` or borrow (`render/dmabuf_import.rs:83, 85`)
- [ ] **RC12**: Per-frame TextureView + BindGroup in imported NV12 path — should cache when fd/modifier match (`render.rs:225-244`)
- [x] **RC13**: `Timestamp::from_micros().expect()` — timestamp overflow requires >584 years of stream time; acceptable risk
- [ ] **RC15**: Android MediaCodec encoder uses ByteBuffer mode with CPU scaling — should use Surface input mode for zero-copy

### Documentation

- [x] **RC14**: SAFETY comments added to all four VAAPI encoder unsafe blocks

### New findings (2026-03-18)

- [x] **RC16**: VAAPI device path hardcoded — VPP code now enumerates renderD128–135 instead of hardcoding two paths; main encoder/decoder uses cros-libva's Display::open which has its own enumeration
- [x] **RC17**: Opus pre-skip always zero — set to 312 samples (standard Opus encoder lookahead at 48kHz)
- [x] **RC18**: Vulkan command pool not destroyed on DmaBufImporter drop — verified: already cleaned up in Drop impl (`render/dmabuf_import.rs:949`)
- [x] **RC19**: AV1 decoder stride assumption — `anyhow::ensure!` validates Y/U/V strides before conversion
- [x] **RC20**: FFmpeg `get_extradata()` — ffmpeg encoder code no longer exists in the codebase
- [x] **RC21**: V4L2 decoder `copy_plane()` allocates Vec even when stride == width — fixed: returns `Cow<[u8]>` for zero-copy fast path
- [x] **RC22**: VAAPI VPP retiler — non-issue: already lazy-initialized once via `get_or_insert_with`; subsequent frames reuse cached retiler

### rusty-capture

- [x] **CAP1**: PipeWire `parse_format_pod()` raw pointer dereference — replaced with safe `Pod::as_bytes()` API (verified: returns header+body, same as old raw pointer approach)
- [x] **CAP2**: X11 `stop_capture()` ignores `shm::detach()` and `shmdt()` errors — now logs at warn level
- [x] **CAP3**: `CameraConfig::select_format()` falls back silently — now documented in doc comment and logs at debug level when preferred format unavailable
- [x] **CAP4**: PipeWire frame drop logging at power-of-two intervals — acceptable: logs at 1,2,4,8,...,64,100,128,200,...; `throttled-tracing` not in rusty-capture deps
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
- [x] **ON21**: Commit 83d0b16 bundles unrelated changes — historical, noted for future practice
- [x] **ON23**: UI thread blocking in `split.rs` `resubscribe()` — acceptable for example code; production app would use an async event loop

---

## Expert review findings (2026-03-20)

Findings from three expert reviews: capture/platform, playout/sync, and codec safety.

### Critical

- [ ] **ER1**: PipeWire DMA-BUF modifier hardcoded to 0 (LINEAR) — actual DRM modifier from compositor never read; tiled DMA-BUFs produce garbled frames on modern GPUs; VPP retiler partially works around this downstream. Fix requires parsing SPA_FORMAT_VIDEO_modifier from format negotiation and propagating through `PipeWireDmaBufFrame` — needs PipeWire testing (`pipewire.rs:223`)

### High

- [ ] **ER2**: PipeWire NV12 DMA-BUF reports single plane — NV12 is two-plane; single-plane report causes downstream importers to reject or corrupt chroma (`pipewire.rs:229`)
- [ ] **ER3**: V4L2 DMA-BUF EXPBUF path not implemented — docs claim zero-copy via EXPBUF but code always does CPU copies (`v4l2.rs`)
- [ ] **ER4**: Audio and video not synchronized by shared playout clock — audio pushes to ring buffer immediately while video is gated on PlayoutClock; gradual A/V drift under jitter has no correction mechanism. Recommended fix: delay audio playback start until video clock's first `observe_arrival` establishes a base, then offset audio by the same amount. NOTE: keep in mind that audio-only must always work, and audio should be the "master", i.e. if video doesn't arrive or is blocked, audio should still be playing. IF video arrives, it is correct to delay audio so that they are in sync.

### Medium

- [ ] **ER5**: `Rc<Display>` cross-thread drop race in VAAPI decoder — `VaapiGpuFrame` clones `Rc<Display>` on decode thread; if frame dropped on different thread, non-atomic refcount is a data race (UB). Blocked on cros-libva using `Rc<Display>` instead of `Arc<Display>` — would need upstream change or `Arc<Mutex<Rc<Display>>>` wrapper (`vaapi/decoder.rs`)
- [x] **ER6**: Opus `pop_samples` returns stale data on double-call — now swaps into secondary buffer and clears primary; second call returns None
- [ ] **ER7**: No playout clock drift correction — PTS-to-wall-clock mapping drifts ~36ms/hour with typical 10ppm oscillator skew; re-anchor masks with stutters (`playout.rs`)
- [x] **ER8**: Zero default playout buffer for software decoders — mitigated: `observe_arrival` enforces MIN_JITTER_BUFFER (66ms) floor on both initial anchor and re-anchors regardless of configured buffer; only the PlayoutMode::buffer field is zero
- [ ] **ER9**: Rendition switches not seamless — old decoder dropped immediately, new decoder waits for keyframe, causing visible glitch (`adaptive.rs`). Fix should be: start new decoder first and switch once first frame of new decoder is ready.
- [x] **ER10**: V4L2 `format()` always returns RGBA — now returns Bgra for BGRA captures; NV12/I420 can't be represented in PixelFormat (only packed formats), documented in comment
- [ ] **ER11**: V4L2 no `VIDIOC_S_PARM` for frame rate — intervals enumerated but never set (`v4l2.rs`)
- [ ] **ER12**: X11 resolution changes not handled — width/height captured once, never updated; will crash or garble output on resolution change (`x11.rs`)
- [ ] **ER18**: V4L2 NV12 size calculation ignores V4L2 stride padding (`bytesperline`) — corrupted frames on drivers with row padding (common on hardware ISPs) (`v4l2.rs:407`)
- [x] **ER19**: `with_window()` panics on non-macOS — replaced `unimplemented!()` with `anyhow::bail!`
- [x] **ER20**: Camera docs say "last camera" but code picks first — fixed docs to match code

### Low

- [x] **ER13**: Encoder timestamps synthesized from counter — H.264 (openh264) and VAAPI encoders now propagate input frame timestamp; AV1 (rav1e) keeps synthetic timestamps because lookahead reordering prevents simple propagation
- [x] **ER14**: FD leak in VAAPI VPP error path — false positive: `close(fd_dup)` always runs before error checks in both encoder and retiler paths; RAII wrapper would be nice but not required
- [x] **ER15**: BT.601/BT.709 metadata mismatch — AV1 encoder now reports matrix_coefficients=6 (BT.601) to match the actual YUV conversion in convert.rs; primaries and transfer stay BT.709 (correct for screen/camera capture)
- [ ] **ER16**: PipeWire stopper thread uses 50ms polling loop instead of condvar
- [x] **ER17**: Playout `recv_timeout` spin-sleep — improved: sleeps for min(remaining/2, 1ms) instead of fixed 1ms, reducing CPU usage while keeping worst-case latency bounded

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
