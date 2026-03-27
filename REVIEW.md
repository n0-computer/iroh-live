# Codebase review

Open items only, grouped by severity.

---

## API ergonomics

- [ ] **E2**: Getting a `VideoTrack` from `RemoteBroadcast` has multiple methods (`video()`, `video_with()`, `video_with_decoder()`, `video_rendition()`). The simplest path `remote.video()` is good, but the relationship between these methods and when to use which is not obvious from the type signatures alone. Could benefit from a builder pattern: `remote.video().with_quality(Quality::Best).build()`.

- [ ] **E3**: `Call::dial(live, remote, local_broadcast)` and `Call::accept(session, local_broadcast)` have asymmetric first arguments (`&Live` vs `MoqSession`). The accept side requires the user to handle incoming sessions manually. A `live.accept_call(local_broadcast)` convenience that waits for the next incoming session would simplify the callee path.

---

### Bugs

- [ ] **B6**: Per-frame Vulkan resource allocation in DMA-BUF import — pool Y/UV plane images (`render/dmabuf_import.rs`)
- [ ] **B8**: No VAAPI-to-Vulkan sync for DMA-BUF — works on Intel implicit sync, may fail elsewhere (`render/dmabuf_import.rs`)
- [ ] **B10**: Opus encoder ignores requested input sample rate — hardcodes 48k per Opus spec; `processing/resample.rs` has a rubato-based resampler but it's not wired into the encoder path yet

### Design

- [ ] **D9**: VkImage coded vs display dimension mismatch — coded height rounded up to 16 but wgpu texture uses display height (`render/dmabuf_import.rs`)
- [ ] **D15**: `LocalBroadcast::producer()` exposes internal `BroadcastProducer` — bypasses catalog/rendition safety (`publish.rs`)
- [ ] **D16**: No observability in AdaptiveVideoTrack — no way to query probe state or decision reasons (`adaptive.rs`)

### Performance

- [ ] **P16**: Double YUV-RGB-YUV conversion on encode path — NV12 sources go NV12->RGBA->YUV420 (~5 ms/frame at 1080p)
- [ ] **P18**: Per-frame DMA-BUF re-export in VAAPI decoder — surface export metadata could be cached (`vaapi/decoder.rs:198`)
- [ ] **RC11**: Ash Device/Instance cloned in DMA-BUF importer — could use `Arc` or borrow (`render/dmabuf_import.rs:83, 85`)
- [ ] **RC12**: Per-frame TextureView + BindGroup in imported NV12 path — should cache when fd/modifier match (`render.rs:225-244`)
- [ ] **RC15**: Android MediaCodec encoder uses ByteBuffer mode with CPU scaling — should use Surface input mode for zero-copy

### Testing

- [ ] **T3**: No `render.rs` tests — `WgpuVideoRenderer` untested (needs GPU)
- [ ] **T5**: `PublishCaptureController` not tested — `set_opts` has no tests
- [ ] **T6**: No fuzz tests — codec decoders not fuzzed with malformed input

### API

- [ ] **A3**: Quality enum is coarse — four fixed presets, no custom resolution/bitrate
- [ ] **A4**: `DecodeConfig` minimal — only `pixel_format` and `backend`, no resolution/framerate constraints

### Audio Backend

- [ ] **AB3**: AEC Mutex on real-time thread — `AecProcessor` holds `Arc<Mutex<AudioProcessing>>` acquired on cpal input callback; becomes RT violation if `set_stream_delay` is wired up. Use `try_lock` with passthrough fallback.
- [ ] **AB5**: AEC processing errors silently discarded — `process_render_f32`/`process_capture_f32` results ignored; add atomic error counter
- [ ] **AB7**: No clock drift correction between input/output — cpal streams on independent hardware clocks; monitor fill level, micro-resample
- [ ] **AB8**: Device switch doesn't handle rebuild failure — drops old streams before starting new; start new before dropping old, or attempt restart with previous device IDs

### rusty-capture

- [ ] **CAP5**: No test for V4L2 DMA-BUF export path (`VIDIOC_EXPBUF`) — regression risk on driver changes
- [ ] **CAP6**: PipeWire portal thread panics are unobserved — `result_rx.recv_timeout()` catches timeout but not panic (`pipewire.rs:1027-1036`)

### xcap backend (`rusty-capture/src/platform/xcap_impl.rs`)

- [ ] **CAP10**: Monitor resolution change silently updates dimensions mid-stream — `pop_frame()` detects width/height change and updates `self.dimensions`, but downstream encoders cache `format()` at startup. Can cause encoder errors or corrupted frames on monitor hot-plug.
- [ ] **CAP11**: Monitor ID parse failure silently falls back to index 0 — `id.strip_prefix("xcap-").and_then(|s| s.parse().ok()).unwrap_or(0)` means invalid IDs open the wrong monitor with no error.
- [ ] **CAP12**: Duplicate `Monitor::all()` enumeration in `new()` — caller already enumerated to get `MonitorInfo`, constructor re-enumerates to find the same monitor by index. Redundant system call, and the two enumerations could return monitors in different order (race).
- [ ] **CAP13**: `monitors()` uses `unwrap_or(0)` for width/height — creates `MonitorInfo` with 0×0 dimensions that will fail downstream. Should skip the monitor or propagate the error.

### nokhwa backend (`rusty-capture/src/platform/nokhwa_impl.rs`)

- [x] **CAP14**: Camera resource leak in `enumerate_formats()` — opened `Camera` to query formats without closing. Fixed: explicit `stop_stream()` + `drop()` before returning.
- [x] **CAP15**: Camera opened twice during discovery — `enumerate_formats()` now releases the device before `NokhwaCameraCapturer::new()` opens it.
- [ ] **CAP16**: Format enumeration errors silently swallowed — `enumerate_formats()` failure produces an empty format list with a `warn!` log. User sees a camera with zero formats, no way to distinguish "camera has no formats" from "query failed".

---

## iroh-live

- [ ] **IL13**: Room gossip dependency is implicit — `Room::new()` fails at runtime if gossip not enabled, no type-level guard (`rooms.rs`)

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
- [ ] **ER9**: Rendition switches not seamless — old decoder dropped immediately, new decoder waits for keyframe, causing visible glitch (`adaptive.rs`). Fix should be: start new decoder first and switch once first frame of new decoder is ready.
- [ ] **ER11**: V4L2 no `VIDIOC_S_PARM` for frame rate — intervals enumerated but never set (`v4l2.rs`)
- [ ] **ER12**: X11 resolution changes not handled — width/height captured once, never updated; will crash or garble output on resolution change (`x11.rs`)
- [ ] **ER18**: V4L2 NV12 size calculation ignores V4L2 stride padding (`bytesperline`) — corrupted frames on drivers with row padding (common on hardware ISPs) (`v4l2.rs:407`)

### Low

- [ ] **ER16**: PipeWire stopper thread uses 50ms polling loop instead of condvar

---

## Expert review 2026-03-21

Senior Rust / audio-video streaming review. Focused on concurrency correctness, API soundness, and production robustness concerns not covered by prior reviews.

### Critical

- [ ] **ER21**: `unimplemented!()` in `VideoFrame::rgba_image()` for NV12/I420 when `h264`/`av1` features disabled — panics in library code that could crash downstream applications. Returns `&RgbaImage` via `OnceLock::get_or_init`, so can't return `Result`.  (`format.rs:813,831`)
--> FIX THIS by making the conversion functions available wihthout any features!!

### Important

- [ ] **ER24**: `LocalBroadcast::run` holds state mutex while calling `start_track` — encoder factory calls can take tens of milliseconds for hardware codecs (VAAPI/VTB device negotiation), blocking all `has_video()`, `has_audio()`, `set_video()` callers. Factory call and thread spawn should happen after releasing the lock (`publish.rs:261-264`)
- [ ] **ER25**: No timeout on `RemoteBroadcast::new` catalog wait — if the remote peer connects but never sends a catalog, the subscription hangs indefinitely. Doc recommends `tokio::time::timeout` but `RemoteBroadcast::new` doesn't follow its own advice. Add a configurable timeout with a reasonable default (e.g. 10s) (`subscribe.rs:319-323`)
- [ ] **ER26**: `PublishError` and `SubscribeError` lack `std::error::Error` source chains — `EncoderFailed(anyhow::Error)` and `DecoderFailed(anyhow::Error)` variants wrap inner errors invisible to `source()`. Use `derive_more::Error` or `thiserror` (`publish.rs:54-83`, `subscribe.rs:167-199`)

### Minor

- [ ] **ER27**: `select_rendition` fallback to `renditions.keys().next()` returns lexicographically first key, which may not be the lowest quality if rendition names don't sort by quality (`subscribe.rs:677-686`)
- [ ] **ER28**: `SharedVideoSource::stop` uses `SeqCst` ordering while `start` uses `Relaxed` — inconsistent; `AcqRel`/`Acquire` would be sufficient and consistent (`publish.rs:1063,1072-1083`)
- [ ] **ER29**: `CatalogSnapshot` `PartialEq` skips inner catalog (`#[eq(skip)]`) — equality based solely on `seq` number, which could surprise callers comparing snapshots from different broadcasts. Document this (`subscribe.rs:236-238`)
- [ ] **ER30**: `VideoTrack::from_video_source` hardcodes `fps = 30` — camera sources at 60fps or 15fps get doubled frames or drops. `VideoFormat` lacks a framerate field (`subscribe.rs:816`)
- [ ] **ER31**: Room actor event channel capacity 16 — in rooms with many simultaneous joins, actor blocks on sends and stops processing gossip. Consider increasing capacity or `try_send` with logged warning (`rooms.rs:89`)

### Already tracked (confirmed still open)

- ER5 / C1: `Rc<Display>` cross-thread drop race in VAAPI decoder — still blocked on cros-libva upstream
- C2: `SharedVideoSource` thread handle in `Arc<JoinHandle>` — thread detached on drop, panics silently lost. Consider joining with timeout in `VideoRenditions::drop`
- I4: `AdaptiveVideoTrack::next_frame` minor race between `check_swap` and `select!` — latency bump during rendition switches, not a correctness issue

---

## Expert review — ergonomics (2026-03-26)

Full API ergonomics review after building the `irl` CLI tool. Covers iroh-live, moq-media, and the CLI itself.

### High — publish flow

- [x] **ER8**: ~~`PlaybackPolicy` and `SyncMode` are dead weight.~~ Resolved: `SyncMode::Synced` enables the shared playout clock (ported from moq/js), `FreshnessPolicy` was inlined as `max_latency` on `PlaybackPolicy`.

### Medium — dead API

- [x] **ER14**: `VideoPublisher::set_enabled()` and `AudioPublisher::set_muted()` were documented no-ops. Commented out with TODO markers until encoder pipeline pause/resume is implemented.

### Medium — config ergonomics

- [ ] **ER16**: `AdaptiveConfig` has 11 fields, no builder methods. Callers must use struct literal with `..Default::default()`. Add builder methods for commonly-adjusted fields.

### Medium — CLI friction

- [ ] **ER19**: `block_on` inside egui `update()` for room peer subscription (room.rs:181). `RemoteBroadcast::media()` is async but room events arrive synchronously. Room should either emit pre-decoded tracks or provide a sync subscription path.

### Medium — naming

- [ ] **ER22**: Two `VideoCodec` types across sibling modules (`rusty_codecs::codec::VideoCodec` for encoding, `rusty_codecs::config::VideoCodec` for decoder config). Rename to `VideoEncoderCodec`/`VideoDecoderConfig` or similar. Large cross-crate rename — defer but do before any external API stabilization.

---

## Deep review 2026-03-21

Exhaustive file-by-file review covering: (a) Rust API idiomatics, (b) A/V industry best practices, (c) performance, (d) potential bugs, (e) stubs and hidden TODOs, (f) unnecessary complexity.

### Critical

- [ ] **DR1** *(waiting for macOS)*: VTB decoder leaks Arc — `create_session()` calls `Arc::into_raw(state.clone())` at line 285 to pass the refcon to VTDecompressionSession. The error path at line 305 correctly reclaims via `Arc::from_raw`. But `VtbDecoder::Drop` (lines 236-244) only calls `wait_for_asynchronous_frames` + `invalidate` — it never reclaims the refcon. The encoder's Drop (lines 372-391) shows the correct pattern: `Arc::from_raw(self.callback_refcon)` after invalidation. Additionally, on SPS-change session recreation (lines 151-163), the old session is invalidated but its refcon Arc is also not reclaimed, leaking one more strong count each time. Fix: store the refcon pointer as a field (like the encoder does) and reclaim in Drop and before session recreation (`vtb/decoder.rs`)

### Important — data model

- [ ] **DR2**: `FrameData::I420` has no stride fields — `rgba_image()` hardcodes `y_stride = width` and `uv_stride = width.div_ceil(2)` (lines 819-820). The `Nv12Planes` variant correctly stores `y_stride` and `uv_stride`. Current I420 producers (rav1d AV1 decoder, libcamera) strip strides during conversion so the data is contiguous, meaning this does not currently produce garbled output. But the data model is incomplete — if a future producer stores padded I420, the conversion will be wrong with no compile-time guard. Add stride fields to `FrameData::I420` for parity with NV12 (`format.rs`)

### Stubs — public APIs that silently do nothing

Doc comments clearly say "Unimplemented" on DR3/DR4, and DR5 logs a `warn!`, so callers with tracing enabled will notice. DR6 are explicitly platform stubs with detailed implementation plans. DR7 are `pub(crate)` internal dead code, not public API.

- [x] **DR3**: `VideoPublisher::set_enabled()` — was public no-op, now commented out with TODO (`publish.rs`)
- [x] **DR4**: `AudioPublisher::set_muted()` — was public no-op, now commented out with TODO (`publish.rs`)
- [ ] **DR5** *(waiting for macOS)*: `AppleCameraCapturer` — entire backend is a stub. `new()` logs `warn!("AVFoundation camera capture not yet fully implemented")`, produces zero frames. `cameras()` returns a hardcoded placeholder (`rusty-capture/src/platform/apple/camera.rs`)
- [ ] **DR6** *(waiting for platform)*: Android and Windows capture backends — pure documentation stubs with detailed implementation plans, no runtime code (`rusty-capture/src/platform/android/mod.rs`, `windows/mod.rs`)
- [ ] **DR7**: `AecProcessor::set_stream_delay()` and `set_enabled()` — `pub(crate)` internal methods, `#[allow(unused, reason = "API reserved for future use")]`. Internal dead code reserved for phase 3 AEC work (`audio_backend/aec.rs:110,163`)

### Important — bugs and correctness

- [ ] **DR8** *(design decision needed)*: `LiveTicket` URI format drops relay URLs on round-trip — `serialize()` encodes only `{SCHEME}{addr}/{name}`, no relay URLs. `deserialize_url()` always returns `relay_urls: Vec::new()`. The `to_bytes`/`from_bytes` postcard path correctly preserves them via serde. The URI format needs a relay URL encoding (e.g. query params) or should document the limitation (`ticket.rs`)
- [ ] **DR11**: Relay `PullState::pull` has TOCTOU race — lock acquired, checked, dropped. Async connect (no lock). Re-acquire to insert. Two concurrent pulls for the same ticket both pass the check and connect; one overwrites the other. Fix: use `tokio::sync::Mutex` held across the async gap, or insert a sentinel value before connecting (`pull.rs`)
- [ ] **DR12**: Relay pull handles never cleaned up — `active` map only grows via `insert`. No background task monitors session health or removes entries when the remote disconnects. On a long-running relay with many transient pulls, this leaks memory indefinitely (`pull.rs`)
- [ ] **DR13** *(design decision needed — changes session dispatch model)*: `IncomingSessionStream` uses `broadcast` channel (capacity 16) — lagged sessions are logged at `info!` level and skipped, not silently dropped. But `broadcast` delivers all sessions to all receivers, which is wrong for a typical single-accept-loop pattern. Under connection storms (>16 pending), sessions are lost. Consider `mpsc` with a single receiver (`iroh-moq/src/lib.rs:86,196`)
- [ ] **DR14**: iroh-moq actor session map dedup — `self.sessions.insert(remote, session.clone())` replaces the old entry. The old session's close task returns `(remote, res)` keyed on the same `remote` PublicKey. When the old session closes, the actor would remove or update `sessions[remote]`, which now points to the new session. This could prematurely mark the new session as closed (`iroh-moq/src/lib.rs`)
- [ ] **DR16** *(design decision needed — changes SharedVideoSource semantics)*: `SharedVideoSource::pop_frame` returns stale frames — `watch::Receiver` has no "consumed" state. If the encoder polls faster than the source produces, it re-encodes the same frame. Fix: call `changed().await` first, or track a generation counter (`publish.rs`)
- [ ] **DR17** *(design decision needed — changes public API signature)*: GPU download panic in `OnceLock::get_or_init` — `gpu.download_rgba().expect("GPU frame download failed")`. GPU context loss, driver bugs, or FD exhaustion cause panics on the render thread. Change `rgba_image()` signature to return `Option<&RgbaImage>` or `Result<&RgbaImage>` (`format.rs`)

### Important — performance

- [ ] **DR20** *(design decision needed — changes Scaler API)*: `Scaler::scale_rgba` clones entire output buffer — the clone allocates a fresh `Vec<u8>` every frame (potentially megabytes at 1080p). Fix: return the buffer and have the caller return it, or use `Bytes`/`Arc<[u8]>` (`processing/scale.rs`)
- [ ] **DR50**: All encoder backends use CPU scaling even when the HW encoder API supports native scaling. VAAPI supports VPP scaling, V4L2 M2M can scale on OUTPUT queue, VideoToolbox accepts arbitrary input dimensions, and MediaCodec Surface mode does GPU scaling. Requires per-backend investigation *(design decision needed)*

### Important — design and API

- [ ] **DR25** *(design decision needed — trait redesign)*: Asymmetric encoder/decoder factory pattern — encoders have a separate `VideoEncoderFactory` trait; decoders put `new()` on the `VideoDecoder` trait with `Self: Sized` bound (preventing `dyn VideoDecoder::new()`). Confusing but functional (`traits.rs:345-367`)
- [ ] **DR28**: `VideoOptions::resolve_quality` ignores `max_bitrate_kbps` — the `VideoTarget` struct has a `max_bitrate_kbps` field but `resolve_quality()` only consults `max_pixels`. Unused field (`subscribe.rs`)
- [ ] **DR30**: Room publisher fire-and-forget spawns — `RoomPublisherSync::new()` and `handle_new_screen()` spawn tokio tasks without storing handles. Errors ARE logged at `warn!` level. Acceptable but worth noting (`rooms/publisher.rs`)
- [ ] **DR48**: `controller.rs::apply_audio` spawns fire-and-forget tokio task — no handle tracking, rapid `set_opts` calls race, errors only logged at `warn!`. Confirmed acceptable (`publish/controller.rs:294-306`)

### Minor — design

- [ ] **DR32** *(waiting for macOS)*: Metal importer has no GPU fence in double-buffer scheme — keeps previous frame's `CVMetalTextureRef` resources alive for one extra frame (~33ms at 30fps). Unlikely to cause issues in practice (`render/metal_import.rs:260-262`)

### Minor — code hygiene

- [ ] **DR18**: `Metric::record()` has non-atomic EMA update — load→compute→store on `AtomicU64` without CAS. Single-writer invariant enforced by architecture. Benign by design (`stats.rs`)
- [ ] **DR33**: RGBA/BGRA conversion pairs are copy-paste — ~200 lines in `convert.rs` where each pair differs only in which `yuvutils_rs` function is called
- [ ] **DR34**: `codec.rs::VideoCodec::available()` returns `Vec<Self>` — should return `&'static [Self]` since values are known at compile time
- [ ] **DR37**: `VideoPreset::fps()` returns 30 for all presets — hardcoded regardless of variant. Doc should note this (`format.rs:882`)
- [ ] **DR38**: `PipeSink::finish()` is a silent no-op — unlike `MoqPacketSink::finish()` which signals stream end. Semantics differ between `PacketSink` implementations (`transport.rs:120`)
- [ ] **DR39**: V4L2 decoder discards timestamps — all decoded frames get `Duration::ZERO`. Should recover timestamps from the V4L2 dequeued buffer (`v4l2/decoder.rs:394,421`)
- [ ] **DR41**: Relay `serve_fingerprint` panics on lock poisoning — acceptable in dev mode; a production relay would use real TLS certificates (`iroh-live-relay/src/main.rs`)
- [ ] **DR43** *(design decision needed)*: `decode_loop` ~163 lines after av-sync removal — could benefit from extracting helpers but is manageable (`pipeline/video_decode.rs:152-315`)
- [ ] **DR44**: `overlay.rs` `show()` and `show_publish()` share ~100 lines of duplicated bottom bar rendering code
- [ ] **DR45**: `DiscoveredVideoSource` enum and `discover_video_sources()` duplicated across `split.rs`, `call.rs`, and `viewer.rs` (~200 lines total). Extract to `examples/common/`
- [ ] **DR46**: H.264 SPS patcher module is dead code — `#[allow(dead_code)]` on entire module, `patch_sps_low_latency` commented out at call sites (`codec/h264/sps.rs`)
- [ ] **DR23** *(design decision needed)*: All RGB-YUV conversions hardcode BT.601 Limited regardless of resolution — technically wrong for HD (720p+) per ITU-R BT.709. Internally consistent since encoders signal BT.601 in metadata (`processing/convert.rs`)

---

## Overnight review 2026-03-27

Full workspace review. Findings are new items not tracked in previous reviews.

### Critical

- [ ] **NR1**: `Live::shutdown` skips `endpoint.close()` when router shutdown succeeds. The `if let Some(router) ... && let Err(err) ... { error } else { endpoint.close() }` structure means the endpoint is only closed in the else branch: when there is no router, OR when the router shutdown fails. When a router is present and shuts down successfully, the endpoint is left open. Fix: unconditionally close the endpoint after router shutdown (`iroh-live/src/live.rs:290-299`).
NO! router shutdown shuts the endpoint down. it's fine as is.

### Important

- [ ] **NR2**: Audio decode tick counter overflows after ~12 hours. `tick_num` is `u64` but cast to `u32` at `TICK * tick_num as u32` (line 265). After 4294967295 ticks at 10ms each (~11.9 hours), the `as u32` wraps to 0, making `target` zero. The sleep calculation produces `Duration::ZERO.saturating_sub(elapsed)` = zero, so the decode loop loses all pacing and spins at CPU speed. Fix: use `Duration::from_millis(tick_num * TICK.as_millis() as u64)` or switch the counter to `u32` and reset periodically (`pipeline/audio_decode.rs:265`).

- [ ] **NR3**: Audio encode tick counter uses default integer type from `for tick in 0..`, which infers as `i32`. `(tick + 1) * INTERVAL` panics on overflow in debug mode (after `i32::MAX` ticks at 20ms = ~248 days) or wraps in release mode (silent sleep miscalculation). Fix: `for tick in 0u64..` (`pipeline/audio_encode.rs:78`).

- [ ] **NR4**: `RemoteBroadcast` Drop does not close the shared `Sync` clock. `RemoteBroadcast::shutdown()` calls `self.sync.close()`, but plain `drop()` does not. If the `RemoteBroadcast` is dropped without `shutdown()` (common in error paths, early returns, and scope exits), any decode thread blocked in `sync.wait()` will remain blocked in `Condvar::wait_timeout` until the timeout expires. The `CancellationToken` drop guard cancels the token, but the decode thread may be in a `Condvar::wait_timeout` that is not woken by the token — it only checks `shutdown.is_cancelled()` when the condvar returns. Fix: impl `Drop` for `RemoteBroadcast` that calls `self.sync.close()`, or add a shutdown guard alongside the existing `_catalog_task` (`subscribe.rs:230-243`).

- [ ] **NR5**: Quality-based video rendition selection is effectively broken. `select_video_rendition` builds an order array from `VideoPreset` variants (`P1080`, `P720`, etc.), then calls `ToString` on them to produce strings like `"1080p"`. But catalog rendition keys look like `"video/h264-1080p"`, so the `renditions.contains_key()` check never matches. The function always falls through to `renditions.keys().next()` which returns the lexicographically first key. All quality levels (`Highest`, `High`, `Mid`, `Low`) produce the same result. Fix: compare by parsing resolution from `VideoConfig.coded_width/coded_height` rather than matching by name (`subscribe.rs:730-760`).

### Medium

- [ ] **NR6**: `Subscription::new` spawns stats recorder and signal producer tasks without storing `AbortOnDropHandle`s. The tasks run until the `shutdown` token is cancelled, but if the subscription is moved and the token outlives it (e.g., the `RemoteBroadcast` is cloned elsewhere), the orphaned tasks keep polling a stale connection. Not a leak in normal usage but wastes resources in edge cases (`iroh-live/src/subscription.rs:28-36`).

- [ ] **NR7**: `VideoTrack::from_video_source` hardcodes `fps = 30` and uses fixed-cadence pacing that conflicts with the `SharedVideoSource` PTS-cadence pacing, causing double-pacing. Sources faster than 30fps are throttled; sources slower than 30fps spin on empty `pop_frame()` calls. The comment says "TODO: Make configurable". Fix: use PTS-based pacing consistent with `SharedVideoSource`, or pass fps as a parameter (`subscribe.rs:854-857`).

- [ ] **NR8**: Room actor does not retry subscriptions when a remote broadcast disconnects. When `subscribe_closed` fires, the broadcast ID is removed from `active_subscribe`, but no retry is scheduled. If the remote peer's connection drops and the peer re-announces the same broadcast via gossip KV, the room will attempt a new subscription (because `active_subscribe` no longer contains it). However, gossip KV values are set with `put` (not append), so if the peer's state hasn't changed, the KV layer may not emit a new update event. The room could miss the reconnection opportunity. Consider re-subscribing on close with exponential backoff (`rooms.rs:310-313`).

- [ ] **NR9**: `CatalogProducer::publish` creates a new hang group on every catalog update with no deduplication. Setting the same catalog twice (e.g., calling `set_video` with identical input) creates unnecessary transport overhead. Consider comparing with the previous catalog state before publishing (`publish.rs:546-553`).

- [ ] **NR10**: `iroh-live-cli/src/main.rs` uses `tracing_subscriber::fmt::init()` without `EnvFilter`, so `RUST_LOG` has no effect. The relay server correctly uses `EnvFilter::try_from_default_env()`. The CLI should do the same for consistency and debuggability (`iroh-live-cli/src/main.rs:54`).

### Minor

- [ ] **NR11**: `MoqPacketSource` sequence counter starts at 0 and increments before use, so the first packet is logged as `seq=1`. Minor off-by-one in logging only (`transport.rs:48-49`).

- [ ] **NR12**: `moq-media-dioxus::DioxusVideoRenderer::render` directly accesses `self.track.0` (the internal `Arc<Mutex<>>`) instead of going through a public method. Couples the renderer to the handle's internal representation. Add a `VideoTrackHandle::current_frame()` method (`moq-media-dioxus/src/lib.rs:154-160`).

- [ ] **NR13**: `VideoDecoderPipeline` initializes viewport to `(1u32, 1u32)`. If the subscriber never calls `set_viewport`, the decoder sees a 1x1 viewport for its lifetime. Currently harmless because most decoders ignore viewport, but could become a problem if viewport-aware downscaling is added to the decode path (`pipeline/video_decode.rs:98`).
