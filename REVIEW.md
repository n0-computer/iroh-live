# Codebase review

Open items are at the top, grouped by crate. Completed items and architecture notes are at the bottom.

---

## USER REVIEW - prioritize these

- [x] AdaptiveVideoTrack returning dimensions [0, 0] — now returns current rendition dimensions from catalog via AtomicU64, updated on every switch (c11bd37)
- [x] Audio/Video sync initial alignment — audio decode loop now holds until video clock is anchored (Live mode only), with 500ms timeout for audio-only fallback (953395b). Meaningful testing with simulated late video still needed — see ER4 for the remaining drift correction work.

## API ergonomics

Issues noticed while writing minimal README code examples. The goal is that the simplest use case (publish a stream, subscribe to a stream) should be expressible in a few lines without needing to understand multiple crates. `moq_media` is re-exported as `iroh_live::media`, which helps, but some friction remains.

- [ ] **E1**: `live.subscribe(remote, name)` returns `(MoqSession, RemoteBroadcast)` as a tuple. The session handle is needed to keep the connection alive, but in most cases the caller only cares about the `RemoteBroadcast`. Consider having `RemoteBroadcast` hold the session internally (dropping the broadcast drops the session), or return a wrapper struct with named fields.

- [ ] **E2**: Getting a `VideoTrack` from `RemoteBroadcast` has multiple methods (`video()`, `video_with()`, `video_with_decoder()`, `video_rendition()`). The simplest path `remote.video()` is good, but the relationship between these methods and when to use which is not obvious from the type signatures alone. Could benefit from a builder pattern: `remote.video().with_quality(Quality::Best).build()`.

- [ ] **E3**: `Call::dial(live, remote, local_broadcast)` and `Call::accept(session, local_broadcast)` have asymmetric first arguments (`&Live` vs `MoqSession`). The accept side requires the user to handle incoming sessions manually. A `live.accept_call(local_broadcast)` convenience that waits for the next incoming session would simplify the callee path.

---

### Bugs

- [ ] **B6**: Per-frame Vulkan resource allocation in DMA-BUF import — pool Y/UV plane images (`render/dmabuf_import.rs`)
- [ ] **B8**: No VAAPI-to-Vulkan sync for DMA-BUF — works on Intel implicit sync, may fail elsewhere (`render/dmabuf_import.rs`)
- [ ] **B10**: Opus encoder ignores requested input sample rate — hardcodes 48k per Opus spec; `processing/resample.rs` has a rubato-based resampler but it's not wired into the encoder path yet

### Design

- [ ] **D9**: VkImage coded vs display dimension mismatch — coded height rounded up to 16 but wgpu texture uses display height (`render/dmabuf_import.rs`)

### Performance

- [ ] **P16**: Double YUV-RGB-YUV conversion on encode path — NV12 sources go NV12->RGBA->YUV420 (~5 ms/frame at 1080p)
- [ ] **P18**: Per-frame DMA-BUF re-export in VAAPI decoder — surface export metadata could be cached (`vaapi/decoder.rs:198`)

### Testing

- [x] **T7**: Patchbay A/V recovery on VAAPI now passes the enforced latency spike / blackout coverage after tightening catch-up late-frame handling, learning a bounded stable late bias for hardware decode, and turning sustained `NotEnoughOutputBuffers` into a reset-worthy decoder error with timestamp rollback (`iroh-live/tests/patchbay.rs`, `moq-media/src/playout.rs`, `moq-media/src/pipeline.rs`, `rusty-codecs/src/codec/vaapi/decoder.rs`)
- [ ] **T3**: No `render.rs` tests — `WgpuVideoRenderer` untested (needs GPU)
- [ ] **T5**: `PublishCaptureController` not tested — `set_opts` has no tests
- [ ] **T6**: No fuzz tests — codec decoders not fuzzed with malformed input

### API

- [ ] **A3**: Quality enum is coarse — four fixed presets, no custom resolution/bitrate
- [ ] **A4**: `DecodeConfig` minimal — only `pixel_format` and `backend`, no resolution/framerate constraints

### Audio Backend

- [ ] **AB3**: AEC Mutex on real-time thread — `AecProcessor` holds `Arc<Mutex<AudioProcessing>>` acquired on cpal input callback; becomes RT violation if `set_stream_delay` is wired up. Use `try_lock` with passthrough fallback.
- [x] **AB4**: AEC VecDeque can allocate on audio thread — capped render, capture, and output buffers at BUF_CAPACITY (8192); oldest samples discarded if consumer falls behind
- [ ] **AB5**: AEC processing errors silently discarded — `process_render_f32`/`process_capture_f32` results ignored; add atomic error counter
- [x] **AB6**: Output resampling latency 300ms — reduced to 50ms output / 30ms input; WebRTC targets 10-20ms but requires tighter clock management
- [ ] **AB7**: No clock drift correction between input/output — cpal streams on independent hardware clocks; monitor fill level, micro-resample
- [ ] **AB8**: Device switch doesn't handle rebuild failure — drops old streams before starting new; start new before dropping old, or attempt restart with previous device IDs

### New findings (2026-03-18)

- [x] **D14**: Playout clock no longer leaks into the public API. Playback is configured through `PlaybackPolicy` and live freshness tuning via `RemoteBroadcast::set_max_stale_duration()` (`subscribe.rs`, `playout.rs`)
- [ ] **D15**: `LocalBroadcast::producer()` exposes internal `BroadcastProducer` — bypasses catalog/rendition safety (`publish.rs`)
- [ ] **D16**: No observability in AdaptiveVideoTrack — no way to query probe state or decision reasons (`adaptive.rs`)

---

### New findings (2026-03-18)

- [ ] **IL13**: Room gossip dependency is implicit — `Room::new()` fails at runtime if gossip not enabled, no type-level guard (`rooms.rs`)
- [ ] **IL14**: `Live::subscribe()` returns tuple `(MoqSession, RemoteBroadcast)` — unclear ownership, should wrap (`live.rs`)
- [ ] **IL15**: `spawn()` vs `spawn_with_router()` fork is confusing — user can forget to mount protocols (`live.rs`)

---

### Performance

- [ ] **RC11**: Ash Device/Instance cloned in DMA-BUF importer — could use `Arc` or borrow (`render/dmabuf_import.rs:83, 85`)
- [ ] **RC12**: Per-frame TextureView + BindGroup in imported NV12 path — should cache when fd/modifier match (`render.rs:225-244`)
- [ ] **RC15**: Android MediaCodec encoder uses ByteBuffer mode with CPU scaling — should use Surface input mode for zero-copy

### rusty-capture

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

---

## Expert review findings (2026-03-20)

Findings from three expert reviews: capture/platform, playout/sync, and codec safety.

### Critical

- [ ] **ER1**: PipeWire DMA-BUF modifier hardcoded to 0 (LINEAR) — actual DRM modifier from compositor never read; tiled DMA-BUFs produce garbled frames on modern GPUs; VPP retiler partially works around this downstream. Fix requires parsing SPA_FORMAT_VIDEO_modifier from format negotiation and propagating through `PipeWireDmaBufFrame` — needs PipeWire testing (`pipewire.rs:223`)

### High

- [ ] **ER2**: PipeWire NV12 DMA-BUF reports single plane — NV12 is two-plane; single-plane report causes downstream importers to reject or corrupt chroma (`pipewire.rs:229`)
- [ ] **ER3**: V4L2 DMA-BUF EXPBUF path not implemented — docs claim zero-copy via EXPBUF but code always does CPU copies (`v4l2.rs`)
- [ ] **ER4**: Audio and video not synchronized — PTS timebase mismatch. **Partially fixed**: wall-clock audio PTS is implemented (`pipeline/audio_encode.rs:90` overrides `pkt.timestamp = start.elapsed()`). Shared epoch between audio and video encoder threads is NOT yet implemented — each thread uses its own `Instant::now()`, so 50–200ms constant offset remains depending on capture startup order. The `PlayoutClock` and `PlayoutBuffer` were removed in the av-sync simplification (9c72f6a), so no playout-side changes are needed.

  **Remaining work:** Pass a shared `Instant` to both audio and video encoder pipelines so PTS=0 means the same wall-clock moment.

### Medium

- [ ] **ER5**: `Rc<Display>` cross-thread drop race in VAAPI decoder — `VaapiGpuFrame` clones `Rc<Display>` on decode thread; if frame dropped on different thread, non-atomic refcount is a data race (UB). Blocked on cros-libva using `Rc<Display>` instead of `Arc<Display>` — would need upstream change or `Arc<Mutex<Rc<Display>>>` wrapper (`vaapi/decoder.rs`)
- [x] **ER7**: No playout clock drift correction — moot: `PlayoutClock` and `PlayoutBuffer` were removed in the av-sync simplification (9c72f6a). Will need to be reconsidered when A/V sync is re-added.
- [ ] **ER9**: Rendition switches not seamless — old decoder dropped immediately, new decoder waits for keyframe, causing visible glitch (`adaptive.rs`). Fix should be: start new decoder first and switch once first frame of new decoder is ready.
- [ ] **ER11**: V4L2 no `VIDIOC_S_PARM` for frame rate — intervals enumerated but never set (`v4l2.rs`)
- [ ] **ER12**: X11 resolution changes not handled — width/height captured once, never updated; will crash or garble output on resolution change (`x11.rs`)
- [ ] **ER18**: V4L2 NV12 size calculation ignores V4L2 stride padding (`bytesperline`) — corrupted frames on drivers with row padding (common on hardware ISPs) (`v4l2.rs:407`)

### Low

- [ ] **ER16**: PipeWire stopper thread uses 50ms polling loop instead of condvar

---

## Expert review 2026-03-21

Senior Rust / audio-video streaming review. Focused on concurrency correctness, API soundness, and production robustness concerns not covered by prior reviews.

### Executive summary

The crate boundaries are clean and deliberate: `rusty-codecs` handles codec abstraction and GPU rendering, `moq-media` orchestrates publish/subscribe pipelines without transport dependency, `iroh-moq` provides MoQ transport, and `iroh-live` ties everything together. The threading model is appropriate — OS threads for blocking codec work, tokio tasks for async transport, channels bridging the two. The playout clock, adaptive bitrate, and pipeline lifecycle management are all thoughtfully designed. Test coverage is strong for codec roundtrips and pipeline integration. The main concerns center on a few concurrency edge cases, some missing backpressure paths, and places where error recovery could be more robust.

### Critical

- [ ] **ER21**: `unimplemented!()` in `VideoFrame::rgba_image()` for NV12/I420 when `h264`/`av1` features disabled — panics in library code that could crash downstream applications. Returns `&RgbaImage` via `OnceLock::get_or_init`, so can't return `Result`.  (`format.rs:813,831`)
--> FIX THIS by making the conversion functions available wihthout any features!!

### Important

- [x] **ER22**: Audio decode loop tick counter is `usize` — fixed: now `u64` (`pipeline/audio_decode.rs:148`: `for tick_num in 0u64..`)
- [x] **ER23**: `recv_timeout` spin-polls — moot: `PlayoutBuffer` removed entirely in av-sync simplification (9c72f6a). Video decode now uses PTS-based pacing without a playout buffer.
- [ ] **ER24**: `LocalBroadcast::run` holds state mutex while calling `start_track` — encoder factory calls can take tens of milliseconds for hardware codecs (VAAPI/VTB device negotiation), blocking all `has_video()`, `has_audio()`, `set_video()` callers. Factory call and thread spawn should happen after releasing the lock (`publish.rs:261-264`)
- [ ] **ER25**: No timeout on `RemoteBroadcast::new` catalog wait — if the remote peer connects but never sends a catalog, the subscription hangs indefinitely. Doc recommends `tokio::time::timeout` but `RemoteBroadcast::new` doesn't follow its own advice. Add a configurable timeout with a reasonable default (e.g. 10s) (`subscribe.rs:319-323`)
- [ ] **ER26**: `PublishError` and `SubscribeError` lack `std::error::Error` source chains — `EncoderFailed(anyhow::Error)` and `DecoderFailed(anyhow::Error)` variants wrap inner errors invisible to `source()`. Use `derive_more::Error` or `thiserror` (`publish.rs:54-83`, `subscribe.rs:167-199`)

### Minor

- [ ] **ER27**: `select_rendition` fallback to `renditions.keys().next()` returns lexicographically first key, which may not be the lowest quality if rendition names don't sort by quality (`subscribe.rs:677-686`)
- [ ] **ER28**: `SharedVideoSource::stop` uses `SeqCst` ordering while `start` uses `Relaxed` — inconsistent; `AcqRel`/`Acquire` would be sufficient and consistent (`publish.rs:1063,1072-1083`)
- [ ] **ER29**: `CatalogSnapshot` `PartialEq` skips inner catalog (`#[eq(skip)]`) — equality based solely on `seq` number, which could surprise callers comparing snapshots from different broadcasts. Document this (`subscribe.rs:236-238`)
- [ ] **ER30**: `VideoTrack::from_video_source` hardcodes `fps = 30` — camera sources at 60fps or 15fps get doubled frames or drops. `VideoFormat` lacks a framerate field (`subscribe.rs:816`)
- [ ] **ER31**: Room actor event channel capacity 16 — in rooms with many simultaneous joins, actor blocks on sends and stops processing gossip. Consider increasing capacity or `try_send` with logged warning (`rooms.rs:89`)
- [x] **ER32**: `PlayoutBuffer` `max_frames` hardcoded at 30 — moot: `PlayoutBuffer` removed entirely in av-sync simplification (9c72f6a)
- [ ] **ER33**: Missing `#[must_use]` on builder methods — `VideoTarget::max_pixels`, `VideoOptions::target`, `AudioOptions::rendition` return `Self` but callers could accidentally discard the builder

### Already tracked (confirmed still open)

- ER5 / C1: `Rc<Display>` cross-thread drop race in VAAPI decoder — still blocked on cros-libva upstream
- C2: `SharedVideoSource` thread handle in `Arc<JoinHandle>` — thread detached on drop, panics silently lost. Consider joining with timeout in `VideoRenditions::drop`
- I4: `AdaptiveVideoTrack::next_frame` minor race between `check_swap` and `select!` — latency bump during rendition switches, not a correctness issue

### Positive observations

- **Architecture**: The `moq-media` / `iroh-live` boundary is well-maintained. `PacketSource`/`PacketSink` traits enable clean testing without network infrastructure.
- **Playout clock**: RFC 3550 jitter measurement, adaptive re-anchoring with jitter-proportional buffers, `MIN_JITTER_BUFFER` floor — reflects real operational experience.
- **Error recovery**: Video decode loop skip-to-keyframe with cross-track sync (audio flushes on video skip generation change) avoids tight coupling.
- **Tracing**: Structured fields, appropriate log levels, `throttled-tracing` for hot paths, per-pipeline `info_span!` naming. Would be invaluable at 3 AM two years from now.
- **Resource lifecycle**: `CancellationToken` + `DropGuard` + `AbortOnDropHandle` + `spawn_thread` with `catch_unwind` provides reliable cleanup.
- **Adaptive bitrate**: Asymmetric timers, bandwidth headroom, probe mechanism with congestion baseline tracking — matches production ABR implementations.

---

## Expert review — ergonomics (2026-03-26)

Full API ergonomics review after building the `irl` CLI tool. Covers iroh-live, moq-media, and the CLI itself. Findings grouped by impact.

### High — subscribe flow

- [ ] **ER1**: `live.subscribe_with_stats()` returns a 3-tuple `(MoqSession, RemoteBroadcast, Receiver<NetworkSignals>)`. Every caller must keep the session alive with `#[allow(dead_code)]` — a footgun. Create a `Subscription` struct that owns session + broadcast + signals. Dropping it closes the session. Subsumes existing E1.

- [ ] **ER2**: Stats/signals wiring is manual outside of `subscribe_with_stats`. The `Call` and `Room` paths both manually call `spawn_stats_recorder` + `spawn_signal_producer` (identical boilerplate). These should merge into one function, or better, auto-wire on `Subscription`/`RemoteBroadcast` when a connection is available.

- [x] **ER3**: `RemoteBroadcast::media::<D>()` requires a turbofish — fixed: non-generic `media()` convenience added that uses `DefaultDecoders` (gated on `#[cfg(any_codec)]`). Generic version available as `media_with_decoders::<D>()` (`subscribe.rs:475-482`).

- [ ] **ER4**: `subscribe_preview_from_consumer` is a free function in `publish.rs` (subscribe code living in the publish module). Move to `subscribe.rs` and add a non-generic convenience.

### High — publish flow

- [ ] **ER5**: `Live::publish()` takes `&LocalBroadcast` but `Room::publish()` takes `BroadcastProducer`. Inconsistent — every room caller does `broadcast.producer()` manually. `Room::publish` should accept `&LocalBroadcast` too.

- [ ] **ER6**: `publish_to_room` in CLI transport.rs uses `std::mem::forget(room)` to keep the room handle alive. `Live` should own joined rooms internally, or provide `Live::publish_to_room(name, producer, ticket)` as a single method.

### High — construction and lifecycle

- [ ] **ER7**: Three different `Live` construction patterns across CLI commands. `Live::from_env()` always enables gossip+router; `setup_live()` does manual endpoint construction; `room.rs` does its own construction with `enable_gossip()`. `LiveBuilder` should expose `from_env()` returning a builder (not a built `Live`) so callers can chain `.no_router()`, `.enable_gossip()`, `.serve(false)` before `.build()`.

- [ ] **ER8**: `PlaybackPolicy` and `SyncMode` are dead weight — `SyncMode` has one variant (`Unmanaged`), and `PlaybackPolicy` wraps `FreshnessPolicy` with nothing else. Collapse to just `FreshnessPolicy` until A/V sync returns.

### Medium — naming and dead API

- [ ] **ER9**: `CaptureStats` is misnamed — it covers encoding too (`encode_ms`, `bitrate_kbps`). Rename to `PublishStats` or `EncodeStats`.

- [ ] **ER10**: `DecodeOpts` vs `DecodeConfig` — near-identical names for internal pipeline plumbing vs user-facing config. Rename `DecodeOpts` to `PipelineContext` or similar.

- [ ] **ER11**: `subscribe_media_track` (singular) returns `MediaTracks` (plural). Rename to `subscribe_media`.

- [ ] **ER12**: `ParticipantId`, `TrackName`, `TrackKind` in iroh-live types.rs appear unused. Remove or mark `#[doc(hidden)]` until needed.

- [ ] **ER13**: `RoomEvent::RemoteConnected` is documented as "reserved, not emitted". Dead variants in public enums force unreachable match arms. Remove until needed.

- [ ] **ER14**: `VideoPublisher::set_enabled()` and `AudioPublisher::set_muted()` are documented no-ops. Remove from public API until implemented.

### Medium — config ergonomics

- [ ] **ER15**: `PlaybackConfig` wraps `DecodeConfig` + `Quality`, but callers always construct nested structs: `PlaybackConfig { decode_config: DecodeConfig { backend, ..Default::default() }, ..Default::default() }`. Flatten or add builder methods.

- [ ] **ER16**: `AdaptiveConfig` has 11 fields, no builder methods. Callers must use struct literal with `..Default::default()`. Add builder methods for commonly-adjusted fields.

- [ ] **ER17**: `VideoOptions` / `AudioOptions` have `pub` fields but no `#[non_exhaustive]`. Adding a field is a silent breaking change.

- [ ] **ER18**: `DecoderBackend` and `StreamFormat` (moq-mux) lack `FromStr`/`ValueEnum`. CLI manually matches strings. Derive these upstream.

### Medium — CLI friction

- [ ] **ER19**: `block_on` inside egui `update()` for room peer subscription (room.rs:181). `RemoteBroadcast::media()` is async but room events arrive synchronously. Room should either emit pre-decoded tracks or provide a sync subscription path.

- [ ] **ER20**: `CaptureArgs` default construction is manual (fragile if fields are added). Should derive `Default` with `audio_preset` defaulting to `"hq"`.

- [ ] **ER21**: `NetworkSignals` receiver is required by `RemoteControls` even for local file preview (creates a dummy channel). Make it `Option`.

### Medium — naming

- [ ] **ER22**: Two `VideoCodec` types across sibling modules (`rusty_codecs::codec::VideoCodec` for encoding, `rusty_codecs::config::VideoCodec` for decoder config). Rename to `VideoEncoderCodec`/`VideoDecoderConfig` or similar. Large cross-crate rename — defer but do before any external API stabilization.

- [ ] **ER23**: `CallTicket` is essentially a `LiveTicket` with broadcast name "call". Consider collapsing.

- [ ] **ER24**: `LagTracker::record()` returns unsigned `Duration` (can't represent "ahead"), while `record_ms()` returns signed `f64`. Asymmetric — standardize on signed for both.

- [ ] **ER25**: Common eframe lifecycle boilerplate (ctrl-c handler, repaint scheduling, shutdown in on_exit) repeats across all GUI commands. Could be a shared helper or trait in `moq-media-egui`.

---

## Deep review 2026-03-21

Exhaustive file-by-file review of every function in the codebase, covering: (a) Rust API idiomatics, (b) A/V industry best practices, (c) performance, (d) potential bugs, (e) stubs and hidden TODOs, (f) unnecessary complexity. Items already tracked above are not repeated.

### Critical

- [ ] **DR1** *(waiting for macOS)*: VTB decoder leaks Arc — `create_session()` calls `Arc::into_raw(state.clone())` at line 285 to pass the refcon to VTDecompressionSession. The error path at line 305 correctly reclaims via `Arc::from_raw`. But `VtbDecoder::Drop` (lines 236-244) only calls `wait_for_asynchronous_frames` + `invalidate` — it never reclaims the refcon. The encoder's Drop (lines 372-391) shows the correct pattern: `Arc::from_raw(self.callback_refcon)` after invalidation. Additionally, on SPS-change session recreation (lines 151-163), the old session is invalidated but its refcon Arc is also not reclaimed, leaking one more strong count each time. Fix: store the refcon pointer as a field (like the encoder does) and reclaim in Drop and before session recreation (`vtb/decoder.rs`)

### Important — data model

- [ ] **DR2**: `FrameData::I420` has no stride fields — `rgba_image()` hardcodes `y_stride = width` and `uv_stride = width.div_ceil(2)` (lines 819-820). The `Nv12Planes` variant correctly stores `y_stride` and `uv_stride`. Current I420 producers (rav1d AV1 decoder, libcamera) strip strides during conversion so the data is contiguous, meaning this does not currently produce garbled output. But the data model is incomplete — if a future producer stores padded I420, the conversion will be wrong with no compile-time guard. Add stride fields to `FrameData::I420` for parity with NV12 (`format.rs`)

### Stubs — public APIs that silently do nothing

Doc comments clearly say "Unimplemented" on DR3/DR4, and DR5 logs a `warn!`, so callers with tracing enabled will notice. DR6 are explicitly platform stubs with detailed implementation plans. DR7 are `pub(crate)` internal dead code, not public API.

- [ ] **DR3**: `VideoPublisher::set_enabled()` — public no-op. Doc says "Unimplemented. Currently a no-op." (`publish.rs:393-395`). Already tracked as ST1
- [ ] **DR4**: `AudioPublisher::set_muted()` — public no-op. Doc says "Unimplemented. Currently a no-op." (`publish.rs:442-445`). Already tracked as ST2
- [ ] **DR5** *(waiting for macOS)*: `AppleCameraCapturer` — entire backend is a stub. `new()` logs `warn!("AVFoundation camera capture not yet fully implemented")`, produces zero frames. `cameras()` returns a hardcoded placeholder. Detailed implementation plan in doc comments at lines 112-165 (`rusty-capture/src/platform/apple/camera.rs`)
- [ ] **DR6** *(waiting for platform)*: Android and Windows capture backends — pure documentation stubs with detailed implementation plans, no runtime code. Compile but produce nothing (`rusty-capture/src/platform/android/mod.rs`, `windows/mod.rs`)
- [ ] **DR7**: `AecProcessor::set_stream_delay()` and `set_enabled()` — `pub(crate)` internal methods, `#[allow(unused, reason = "API reserved for future use")]`. Not public API; internal dead code reserved for phase 3 AEC work (`audio_backend/aec.rs:110,163`)

### Important — bugs and correctness

- [ ] **DR8** *(design decision needed)*: `LiveTicket` URI format drops relay URLs on round-trip — `serialize()` (line 63) encodes only `{SCHEME}{addr}/{name}`, no relay URLs. `deserialize_url()` (line 93) always returns `relay_urls: Vec::new()`. `CallTicket::serialize` (line 86) delegates via `self.clone().into_live_ticket().serialize()`, so relay URLs are lost there too. The `to_bytes`/`from_bytes` postcard path correctly preserves them via serde. The URI format needs a relay URL encoding (e.g. query params) or should document the limitation (`ticket.rs`)
- [x] **DR9**: Room actor silently ignores publish failure — now logs warning and skips KV announcement on failure (2b64350) — `self.live.publish_producer(name.clone(), producer).await.ok()` (line 302-303) swallows the error. The broadcast name is inserted into `active_publish` (line 304) and announced via KV gossip (line 309) regardless. Remote peers subscribe to a nonexistent broadcast (`rooms.rs`)
- [x] **DR10**: `RoomTicket::new_from_env` prints wrong env var — fixed to print `IROH_LIVE_TOPIC` (6ccc328) — line 418: `"Created new topic. Reuse with IROH_TOPIC={}"` but line 406: `env::var("IROH_LIVE_TOPIC")`. Users following the printed instructions set `IROH_TOPIC` which is ignored (`rooms.rs`)
- [ ] **DR11**: Relay `PullState::pull` has TOCTOU race — lock acquired at line 52, checked, dropped at line 60. Async connect at lines 68-78 (no lock). Re-acquire at line 100 to insert. Two concurrent pulls for the same ticket both pass the check and connect; one overwrites the other. The first session is dropped, disconnecting the stream. Fix: use `tokio::sync::Mutex` held across the async gap, or insert a sentinel value before connecting (`pull.rs`)
- [ ] **DR12**: Relay pull handles never cleaned up — `active` map only grows via `insert` at line 100. No background task monitors session health or removes entries when the remote disconnects. `_session: MoqSession` goes dead but `PullHandle` persists. On a long-running relay with many transient pulls, this leaks memory indefinitely (`pull.rs`)
- [ ] **DR13** *(design decision needed — changes session dispatch model)*: `IncomingSessionStream` uses `broadcast` channel (capacity 16) — lagged sessions are logged at `info!` level and skipped (line 206-209), not silently dropped. But `broadcast` delivers all sessions to all receivers, which is wrong for a typical single-accept-loop pattern. Under connection storms (>16 pending), sessions are lost. Consider `mpsc` with a single receiver (`iroh-moq/src/lib.rs:86,196`)
- [ ] **DR14**: iroh-moq actor session map dedup — `self.sessions.insert(remote, session.clone())` at line 485 replaces the old entry. The old session's close task (spawned at lines 492-507) returns `(remote, res)` keyed on the same `remote` PublicKey. When the old session closes, the actor would remove or update `sessions[remote]`, which now points to the new session. This could prematurely mark the new session as closed (`iroh-moq/src/lib.rs`)
- [x] **DR15**: `build.rs` missing `v4l2` and `android` in `any_video_codec` — added both features to the check (6ccc328) — line 10: `let any_video_codec = has_h264 || has_av1 || has_videotoolbox || has_vaapi;`. Features `v4l2` and `android` both provide H.264 encoder/decoder but are not checked. Enabling only `v4l2` results in `any_video_codec = false`, and all code behind `#[cfg(any_video_codec)]` is excluded (`moq-media/build.rs`)
- [ ] **DR16** *(design decision needed — changes SharedVideoSource semantics)*: `SharedVideoSource::pop_frame` returns stale frames — line 1086: `self.frames_rx.borrow_and_update().clone()` returns the current value even if the encoder already consumed it. `watch::Receiver` has no "consumed" state. If the encoder polls faster than the source produces, it re-encodes the same frame. Fix: call `self.frames_rx.changed().await` first, or track a generation counter (`publish.rs`)
- [ ] **DR17** *(design decision needed — changes public API signature)*: GPU download panic in `OnceLock::get_or_init` — line 794: `gpu.download_rgba().expect("GPU frame download failed")`. GPU context loss, driver bugs, or FD exhaustion cause panics on the render thread. `OnceLock` retries on panic, hitting the same error repeatedly. Change `rgba_image()` signature to return `Option<&RgbaImage>` or `Result<&RgbaImage>` (`format.rs`)
- [x] **DR19**: AV1 encoder uses synthetic timestamps — now stores input timestamps in a HashMap keyed by input_frameno, recovered after rav1e lookahead reordering (2b64350) — line 119: `timestamp_us = (self.frame_count * 1_000_000) / self.framerate`. The input `frame.timestamp` is never used. The H.264 encoder correctly propagates the input timestamp. This causes AV1 streams to have monotonic but wrong timestamps when frames arrive at irregular intervals (`av1/encoder.rs`)

### Important — performance

- [ ] **DR20** *(design decision needed — changes Scaler API)*: `Scaler::scale_rgba` clones entire output buffer — line 93: `let out = buf.clone();`. The buffer is then stored back in `self.dst_buf` for reuse, but the clone allocates a fresh `Vec<u8>` every frame (potentially megabytes at 1080p). The "reuse" only avoids zero-initialization vs copying existing data — both allocate. Fix: return the buffer and have the caller return it, or use `Bytes`/`Arc<[u8]>` (`processing/scale.rs`)
- [x] **DR21**: `scale_if_needed` duplicated verbatim in 6 encoder backends — extracted `scale_frame_if_needed` in `processing/scale.rs`. VAAPI uses `ScaleMode::Cover` to match old force-scaling behavior. Net -46 lines (f8a261e)
- [x] **DR22**: GLES renderer UV plane always calls `gl_tex_image_2d` (full GPU texture reallocation) instead of `gl_tex_sub_image_2d` (fast update) when dimensions haven't changed. The Y plane uses the cached `upload_tex` function with dimension tracking, but the UV plane uses `upload_tex_uncached`. On the Pi Zero target hardware where GLES2 is the primary renderer, per-frame texture reallocation is wasteful. Fix: add separate `uv_tex_width`/`uv_tex_height` fields to `GlesRenderer` (`render/gles.rs:516-538`)
- [ ] **DR50**: All encoder backends use CPU scaling (`scale_frame_if_needed` in `processing/scale.rs`) even when the HW encoder API supports native scaling. VAAPI supports VPP scaling, V4L2 M2M can scale on OUTPUT queue, VideoToolbox accepts arbitrary input dimensions, and MediaCodec Surface mode does GPU scaling. Using HW scaling would eliminate a per-frame CPU RGBA scale pass. Requires per-backend investigation of API support and quality tradeoffs *(design decision needed)*
- [ ] **DR23** *(design decision needed — affects all encode/decode paths)*: All RGB↔YUV conversions hardcode BT.601 Limited regardless of resolution — technically wrong for HD (720p+) per ITU-R BT.709. However, the encoders signal BT.601 in their metadata (H.264 SPS VUI and AV1 sequence header), so standards-compliant decoders receiving our streams will use the correct matrix. The issue is limited to: (a) receiving external BT.709 content for transcoding, (b) interop testing with tools that assume BT.709 for HD. Internally consistent, so this is a design note rather than a bug (`processing/convert.rs`)

### Important — design and API

- [x] **DR24**: `DynamicVideoDecoder` match-arm duplication — replaced with `dispatch_video!` macro (f8d15d2) — 5 trait methods (`name`, `push_packet`, `pop_frame`, `set_viewport`, `burst_size`) x 7 cfg-gated variants, each arm a one-line delegation. ~110 lines of pure boilerplate. Every new codec backend (HEVC, etc.) requires adding 5 match arms. `enum_dispatch` crate or a `delegate_all!` macro would eliminate this entirely (`codec/dynamic.rs:100-205`)
- [x] **DR26**: `codec::test_util` module was unconditionally `pub` and compiled in release builds — now gated behind `#[cfg(any(test, feature = "test-util"))]`. Added `test-util` feature to rusty-codecs; moq-media activates it via dev-dependencies (6ae4050) — test pattern generators (`make_rgba_frame`, `make_test_pattern`, `video_encode`) ship in the release binary. Gate behind `#[cfg(any(test, feature = "test-util"))]` like the `moq-media::test_util` module already does (`codec.rs:47`)
- [x] **DR27**: Duplicated `secret_key_from_env` — deleted private copy in `live.rs` (used `println!`), callers now use `crate::util::secret_key_from_env()` which uses `tracing::info!` (2b64350) — `live.rs:217-229` is a private copy that uses `println!` (wrong for a library). `util.rs:14-26` is the public version that uses `tracing::info!`. The private copy should be deleted; callers should use `crate::util::secret_key_from_env()` (`iroh-live/src/live.rs`)
- [x] **DR29**: Room event handling inconsistency — `handle_gossip_update` now returns `bool`, actor loop breaks on `false`. Consistent with `BroadcastSubscribed` (2b64350) — `BroadcastSubscribed` (line 269-271) checks `event_tx.send(...).await.is_err()` and stops the actor when the receiver is dropped. `RemoteAnnounced` (line 334-337) uses `.ok()` and silently continues. If the event receiver is dropped, the actor keeps spawning connect tasks that nobody will consume

### Minor — design

- [ ] **DR25** *(design decision needed — trait redesign)*: Asymmetric encoder/decoder factory pattern — encoders have a separate `VideoEncoderFactory` trait with `with_preset`/`with_config`; decoders put `new()` on the `VideoDecoder` trait with `Self: Sized` bound (preventing `dyn VideoDecoder::new()`). The `DynamicVideoDecoder` works around this. Confusing but functional (`traits.rs:345-367`)
- [ ] **DR28**: `VideoOptions::resolve_quality` ignores `max_bitrate_kbps` — the `VideoTarget` struct has a `max_bitrate_kbps` field but `resolve_quality()` (lines 128-146) only consults `max_pixels`. Unused field (`subscribe.rs`)
- [ ] **DR30**: Room publisher fire-and-forget spawns — `RoomPublisherSync::new()` (line 35-42) and `handle_new_screen()` (line 65-69) spawn tokio tasks without storing handles. Errors ARE logged at `warn!` level. This is a limitation of the synchronous constructor pattern — there's no clean way to propagate async publish errors. Acceptable but worth noting (`rooms/publisher.rs`)
- [ ] **DR48**: `controller.rs::apply_audio` spawns fire-and-forget tokio task — no handle tracking, rapid `set_opts` calls race, errors only logged at `warn!`. Already marked as "by design" in D6 (audio device enumeration is async, failures logged, task holds Arc so outliving controller is safe). Confirmed acceptable (`publish/controller.rs:294-306`)

### Minor — render

- [ ] **DR31**: NV12 render pass code duplicated between DMA-BUF and Metal paths — `render_imported_nv12` and `render_imported_metal_nv12` share ~60 lines of identical code (bind group creation, command encoder, render pass). Extract a shared helper that takes the Y/UV texture views (`render.rs:330-460`)
- [ ] **DR32** *(waiting for macOS)*: Metal importer has no GPU fence in double-buffer scheme — keeps previous frame's `CVMetalTextureRef` resources alive for one extra frame (~33ms at 30fps). At normal frame rates the GPU finishes reading well within this window. Under heavy GPU load or frame bursts, previous resources could theoretically be released while still in flight, but this would require processing latency exceeding one full frame interval — unlikely in practice (`render/metal_import.rs:260-262`)
- [x] **DR49**: `render.rs` I420 render path doesn't set `last_render_path` — fixed, now sets `RenderPath::CpuRgba` (6ccc328)

### Minor — code hygiene

- [ ] **DR18**: `Metric::record()` has non-atomic EMA update — load→compute→store on `AtomicU64` without CAS. Comment at lines 107-109 explicitly acknowledges this and states "each Metric has a single writer (one pipeline thread)." The single-writer invariant is enforced by architecture (each metric belongs to one pipeline). Benign by design (`stats.rs`)
- [ ] **DR33**: RGBA/BGRA conversion pairs are copy-paste — ~200 lines in `convert.rs` where each pair differs only in which `yuvutils_rs` function is called. A generic helper parameterized on the conversion function would reduce to ~60 lines
- [ ] **DR34**: `codec.rs::VideoCodec::available()` returns `Vec<Self>` — should return `&'static [Self]` since values are known at compile time. Minor allocation on a cold path
- [x] **DR35**: `AudioFormat` missing `PartialEq`, `Eq`, `Hash` — added (6ccc328) — simple value type with `u32` fields that should be comparable (`format.rs:15`)
- [x] **DR36**: `PixelFormat` missing `Eq` and `Hash` — added (6ccc328) — has `PartialEq` but not `Eq` despite being a simple enum (`format.rs:50`)
- [ ] **DR37**: `VideoPreset::fps()` returns 30 for all presets — hardcoded regardless of variant. Doc should note this (`format.rs:882`)
- [ ] **DR38**: `PipeSink::finish()` is a silent no-op — unlike `MoqPacketSink::finish()` which signals stream end. Semantics differ between `PacketSink` implementations (`transport.rs:120`)
- [ ] **DR39**: V4L2 decoder discards timestamps — all decoded frames get `Duration::ZERO`. Should recover timestamps from the V4L2 dequeued buffer (`v4l2/decoder.rs:394,421`)
- [x] **DR40**: `CallTicket::serialize` clones self unnecessarily — now constructs LiveTicket directly from borrowed fields (f8d15d2) — `into_live_ticket` takes `self` by value, forcing a clone at line 86. Could construct `LiveTicket` directly from `&self` fields (`call.rs`)
- [ ] **DR41**: Relay `serve_fingerprint` panics on lock poisoning — `.expect("tls_info lock poisoned")` in a web handler at line 267. In dev mode (only use case for fingerprint endpoint) this is acceptable; a production relay would use real TLS certificates and skip this endpoint (`iroh-live-relay/src/main.rs`)
- [x] **DR42**: Dead commented-out code in relay main — removed (6ccc328) — old query-param extraction at lines 257-262 should be removed (`iroh-live-relay/src/main.rs`)
- [ ] **DR43** *(design decision needed — refactoring complex control flow)*: `decode_loop` was 280+ lines — reduced to ~163 lines after av-sync removal (9c72f6a) which eliminated playout buffer interaction and skip logic. Now in `pipeline/video_decode.rs:152-315`. Still could benefit from extracting helpers but is much more manageable.
- [ ] **DR44**: `overlay.rs` `show()` and `show_publish()` share ~100 lines of duplicated bottom bar rendering code
- [ ] **DR45**: `DiscoveredVideoSource` enum and `discover_video_sources()` duplicated across `split.rs`, `call.rs`, and `viewer.rs` (~200 lines total). Extract to `examples/common/`
- [ ] **DR46**: H.264 SPS patcher module is dead code — `#[allow(dead_code)]` on entire module, `patch_sps_low_latency` commented out at call sites (`codec/h264/sps.rs`)
- [x] **DR47**: `H264Encoder::frame_count` field was incremented but never read — removed (2b64350) — dead state (`h264/encoder.rs:30`)

### TODO/FIXME inventory

All known TODO comments in the codebase, for completeness:

| Location | Content | Status |
|----------|---------|--------|
| `codec.rs:15` | `TODO(CI1-macos): fix clippy lints in vtb decoder` | Tracked |
| `render.rs:13` | `TODO(CI1-macos): fix clippy lints in metal_import` | Tracked |
| `platform/mod.rs:6` | `TODO(CI1-macos): fix clippy lints in apple module` | Tracked |
| `opus/encoder.rs:78` | `TODO(phase3): enable FEC` | Deferred to phase 3 |
| `opus/encoder.rs:84` | `TODO(phase3): enable DTX` | Deferred to phase 3 |
| `subscribe.rs:815` | `TODO: Make configurable` (fps=30) | Open: DR/ER30 |
| `publish.rs:394` | `TODO: implement pause/resume on encoder pipeline` | Open: DR3 |
| `publish.rs:444` | `TODO: implement mute on audio pipeline` | Open: DR4 |
| `apple/camera.rs:48,112` | AVFoundation camera implementation plan | Open: DR5 |
| `iroh-moq/src/lib.rs:30` | `TODO: Use export from moq_lite after next update` | Waiting on upstream |

---

# Completed

- [x] **B5**: `watch_local` hardcoded 30 fps — same root cause as B12; pipeline.rs fallback now warns
- [x] **B9**: `SharedVideoSource` pacing accumulates paused time — frame clock reset on wake from park
- [x] **D2a**: Dual NAL format support — already implemented: all H.264 decoders detect NalFormat from catalog config and convert between avcC/Annex B as needed; encoders output in the configured format
- [x] **D3**: Encoder config by instantiation — already fixed: `config_for()` is a static method that returns config without instantiating the encoder
- [x] **D4**: `SharedVideoSource` park/unpark — restructured to call stop once before parking and start once after confirmed wake; spurious wakeups no longer cause start/stop churn
- [x] **D6**: `apply_audio` fire-and-forget task — by design: audio device enumeration is async, failures logged at warn level; task holds Arc to broadcast so outliving controller is safe
- [x] **D7**: No backpressure encoder-to-transport — by design for live streaming: encoder maintains source rate for keyframe cadence; transport-level group skipping (hang max_latency) handles congestion; matches WebRTC/OBS pattern
- [x] **P4**: `parse_annex_b` allocates Vec — `AnnexBNalIter` iterator added, `annex_b_to_length_prefixed` streams directly
- [x] **P11**: Annex-B / length-prefixed round-trip — `annex_b_to_length_prefixed` no longer collects intermediate Vec
- [x] **P12**: Payload copy in all decoders — `copy_to_bytes()` is zero-copy for single-chunk `Bytes` (common case); multi-chunk copy is unavoidable since decoders need contiguous `&[u8]`
- [x] **P17**: VAAPI decoder opens three VA Displays per instance — by design: decoder, frame ops (avoids driver serialization), and frame pool (Send wrapper). Could consolidate ops+pool but requires refactoring Rc→Arc
- [x] **P19**: `frame.raw.to_vec()` in video source passthrough — code no longer present; passthrough path now moves frame directly
- [x] **P20**: VAAPI encoder `query_image_formats` per frame upload — cached in thread_local
- [x] **S1**: `unsafe impl Send` on 22 types — audited: all have SAFETY comments, all use single-thread-access or internal-locking pattern consistent with pipeline architecture
- [x] **S2**: rav1d_safe FFI bounds — audited: `copy_nonoverlapping` matches allocation size from `dav1d_data_create`; `assume_init` follows `dav1d_default_settings` which fills the struct; SAFETY comments present
- [x] **S3**: DMA-BUF fd ownership — correct: all dup'd fds are closed in error paths via explicit `libc::close` or Vulkan import (which takes ownership); RAII wrapper would be nice but not needed for correctness
- [x] **S4**: VTB encoder Arc callback dance — fixed: stored refcon pointer, reclaimed in Drop after session invalidation to prevent Arc leak
- [x] **A6**: No runtime `set_keyframe_interval` on `VideoEncoder` trait — by design: `VideoEncoderConfig::keyframe_interval()` sets it at construction; changing mid-stream would require encoder reconfiguration
- [x] **A7**: `AudioEncoderFactory::with_preset` contract ambiguous — documented on `with_config` that encoders may operate at a different internal sample rate (e.g. Opus at 48kHz)
- [x] **AB1**: ~30 `.unwrap()` calls — reduced to ~3 in production code (infallible `NonZeroUsize::new(2)`), rest in tests
- [x] **AB2**: Firewheel removed; `OutputStream` mutex is now uncontended (single-writer, swapped only on device switch)
- [x] **TR1**: Doc comments on `media_pipe()`, `PipeSink`, `PipeSource` — already present
- [x] **ST1**: `VideoPublisher::set_enabled()` — documented as unimplemented stub
- [x] **ST2**: `AudioPublisher::set_muted()` — documented as unimplemented stub
- [x] **CC1**: `SharedVideoSource` park/unpark race — non-issue: `VideoRenditions::drop` cancels shutdown + unparks; park/unpark is sticky so no race
- [x] **CC2**: `PlayoutClock` mutex acquired every frame — non-issue: decode thread and playout buffer are on same OS thread, no actual contention
- [x] **CC3**: AEC VecDeque `.pop_front().unwrap()` in callback — `.expect()` with context + match pattern for output buffer L/R desync
- [x] **PP1**: `PlayoutClock::mode()` clones entire `PlayoutMode` on every call — non-issue: enum is 32 bytes (two Durations), clone is effectively a memcpy
- [x] **PP2**: `AudioBackendOpts` cloning eliminated in new audio backend
- [x] **EH1**: Adaptive rendition switch failures — addressed by D17 (failure cooldown timer prevents retry thrashing)
- [x] **EH2**: `AudioDriver::new()` now logs error and continues if initial stream start fails (no panic)
- [x] **EH3**: Inconsistent lock panic messages — normalized to `.expect("poisoned")`
- [x] **CI1**: rusty-codecs crate-level clippy suppression — moved to workspace `Cargo.toml` lints and fixed file-by-file; only 2 remaining `#[allow]` with `reason` on YUV conversion functions
- [x] **B12**: `VideoEncoderPipeline` hardcodes 30 fps frame duration — added `warn!` on fallback; all encoders populate framerate in practice
- [x] **B13**: `AdaptiveVideoTrack::format()` returns zeroed `[0, 0]` dimensions — documented with doc comment; `[0,0]` is correct for dynamic resolution
- [x] **D11**: `set_enabled()` / `set_muted()` are public no-ops — already documented as unimplemented stubs (see ST1, ST2)
- [x] **D12**: Pipeline thread panics are invisible — `spawn_thread` now wraps body in `catch_unwind`, logs panic message at `error!`, then re-raises
- [x] **D13**: `VideoPublisher::set()` and `replace()` — non-issue: `replace()` does not exist; `set()` handles both initial and replacement
- [x] **D17**: Adaptive rendition switch failure loops forever with no backoff — `last_switch_failure` timer added
- [x] **MQ1**: Actor run loop `.expect()` — replaced with match + `error!` log so task panics don't crash the actor
- [x] **MQ4**: `MoqSession::subscribe(&mut self)` takes `&mut self` — required by moq-lite's `origin.announced(&mut self)`; upstream constraint, not our API choice
- [x] **MQ5**: `publish()` takes `String` by value — changed to `impl ToString` for consistency
- [x] **MQ6**: Channel capacities hardcoded at 16 — reasonable for actor mailbox; broadcast only drops if all receivers are 16 messages behind, which requires sustained unprocessed backlog
- [x] **MQ7**: Error context lost in `Error` → `AnyError` → `AcceptError` — imposed by iroh's `ProtocolHandler::accept` which returns `AcceptError(AnyError)`; Display/Debug messages preserved, only downcasting lost
- [x] **MQ8**: `handle_publish_broadcast` clones name per session — eliminated one redundant clone by moving into insert
- [x] **MQ9**: Module-level doc comment added to `lib.rs`
- [x] **MQ10**: `session_connect()` and `session_accept()` — already documented
- [x] **MQ11**: `published_broadcasts()` swallows actor death — by design; empty vec on actor death is documented behavior, not a silent failure
- [x] **MQ12**: Silent `.ok()` on critical oneshot sends — non-issue: `.ok()` on oneshot reply channels is standard; receiver drop indicates caller lost interest
- [x] **MQ13**: `subscribe()` loops forever if name never announced — non-issue: exits on session close; doc recommends `tokio::time::timeout` for caller-side timeout
- [x] **MQ14**: Empty `#[derive()]` on Actor struct — removed
- [x] **IL1**: `Call::closed()` — now inspects `conn().close_reason()` to distinguish local/remote/transport
- [x] **IL3**: `postcard::to_stdvec().unwrap()` — replaced with `.expect()` with infallibility messages
- [x] **IL5**: Room actor silently drops events on send failure — now detects receiver drop and stops the actor
- [x] **IL6**: `broadcasts.clone()` in room actor — code no longer present at referenced line
- [x] **IL8**: `RoomEvent::RemoteConnected` — documented as reserved/not emitted
- [x] **IL9**: `Call::dial()` and `Call::accept()` — extracted shared `setup()` helper
- [x] **IL10**: `spawn_thread()` panics on failure — acceptable: thread spawn failure indicates system resource exhaustion (OOM, ulimit), and all callers are in constructors that return Result (changing the API would require updating 5+ call sites for a condition that means the system is already failing)
- [x] **IL7**: Missing docs — added doc comments to `Room`, `RoomHandle`, `RoomTicket`, `RoomEvent` variants, `RoomPublisherSync`, `LiveTicket::from_bytes`
- [x] **IL11**: `LiveTicket::to_bytes()` and `serialize()` `.expect()` on postcard — acceptable: postcard serialization of fixed-size types (PublicKey + String) only fails on OOM, which is already unrecoverable; changing serialize() to Result would require manual Display impl since derive_more::Display can't propagate errors
- [x] **IL12**: Two serialization APIs on tickets — by design: `to_bytes`/`from_bytes` for compact binary, `serialize`/`deserialize` for URI strings; different use cases
- [x] **RC8**: `NonNull::new().unwrap()` in rav1d_safe — replaced with `.expect()` and SAFETY comment
- [x] **RC9**: V4L2 decoder `.lock().unwrap()` — normalized to `.expect("poisoned")`
- [x] **RC10**: VAAPI decoder `.lock().unwrap()` — normalized to `.expect("poisoned")`
- [x] **RC13**: `Timestamp::from_micros().expect()` — timestamp overflow requires >584 years of stream time; acceptable risk
- [x] **RC14**: SAFETY comments added to all four VAAPI encoder unsafe blocks
- [x] **RC16**: VAAPI device path hardcoded — VPP code now enumerates renderD128–135 instead of hardcoding two paths; main encoder/decoder uses cros-libva's Display::open which has its own enumeration
- [x] **RC17**: Opus pre-skip always zero — set to 312 samples (standard Opus encoder lookahead at 48kHz)
- [x] **RC18**: Vulkan command pool not destroyed on DmaBufImporter drop — verified: already cleaned up in Drop impl (`render/dmabuf_import.rs:949`)
- [x] **RC19**: AV1 decoder stride assumption — `anyhow::ensure!` validates Y/U/V strides before conversion
- [x] **RC20**: FFmpeg `get_extradata()` — ffmpeg encoder code no longer exists in the codebase
- [x] **RC21**: V4L2 decoder `copy_plane()` allocates Vec even when stride == width — fixed: returns `Cow<[u8]>` for zero-copy fast path
- [x] **RC22**: VAAPI VPP retiler — non-issue: already lazy-initialized once via `get_or_insert_with`; subsequent frames reuse cached retiler
- [x] **CAP1**: PipeWire `parse_format_pod()` raw pointer dereference — replaced with safe `Pod::as_bytes()` API (verified: returns header+body, same as old raw pointer approach)
- [x] **CAP2**: X11 `stop_capture()` ignores `shm::detach()` and `shmdt()` errors — now logs at warn level
- [x] **CAP3**: `CameraConfig::select_format()` falls back silently — now documented in doc comment and logs at debug level when preferred format unavailable
- [x] **CAP4**: PipeWire frame drop logging at power-of-two intervals — acceptable: logs at 1,2,4,8,...,64,100,128,200,...; `throttled-tracing` not in rusty-capture deps
- [x] **ON21**: Commit 83d0b16 bundles unrelated changes — historical, noted for future practice
- [x] **ON23**: UI thread blocking in `split.rs` `resubscribe()` — acceptable for example code; production app would use an async event loop
- [x] **ER6**: Opus `pop_samples` returns stale data on double-call — now swaps into secondary buffer and clears primary; second call returns None
- [x] **ER8**: Zero default playout buffer for software decoders — mitigated: `observe_arrival` enforces MIN_JITTER_BUFFER (66ms) floor on both initial anchor and re-anchors regardless of configured buffer; only the PlayoutMode::buffer field is zero
- [x] **ER10**: V4L2 `format()` always returns RGBA — now returns Bgra for BGRA captures; NV12/I420 can't be represented in PixelFormat (only packed formats), documented in comment
- [x] **ER19**: `with_window()` panics on non-macOS — replaced `unimplemented!()` with `anyhow::bail!`
- [x] **ER20**: Camera docs say "last camera" but code picks first — fixed docs to match code
- [x] **ER13**: Encoder timestamps synthesized from counter — H.264 (openh264) and VAAPI encoders now propagate input frame timestamp; AV1 (rav1e) keeps synthetic timestamps because lookahead reordering prevents simple propagation
- [x] **ER14**: FD leak in VAAPI VPP error path — false positive: `close(fd_dup)` always runs before error checks in both encoder and retiler paths; RAII wrapper would be nice but not required
- [x] **ER15**: BT.601/BT.709 metadata mismatch — AV1 encoder now reports matrix_coefficients=6 (BT.601) to match the actual YUV conversion in convert.rs; primaries and transfer stay BT.709 (correct for screen/camera capture)
- [x] **ER17**: Playout `recv_timeout` spin-sleep — improved: sleeps for min(remaining/2, 1ms) instead of fixed 1ms, reducing CPU usage while keeping worst-case latency bounded

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
