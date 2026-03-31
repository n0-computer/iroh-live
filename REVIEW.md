# Codebase review

Open items only. Every item has been verified against the current codebase
(2026-03-29). Items that have been fixed, completed, or no longer apply have
been removed. Short codes are prefixed by section abbreviation.

---

## Bugs and correctness

- [ ] **BUG-1** *(critical, waiting for macOS)*: VTB decoder leaks an `Arc` refcon. `create_session()` calls `Arc::into_raw(state.clone())` to pass the refcon to `VTDecompressionSession`. The error path reclaims it via `Arc::from_raw`, but `VtbDecoder::Drop` never reclaims it. On SPS-change session recreation, the old refcon also leaks. The encoder's Drop shows the correct pattern. Fix: store the refcon pointer as a field and reclaim in Drop and before session recreation (`vtb/decoder.rs:285, 236-244`). (Previously DR1.)

- [ ] **BUG-2**: `unimplemented!()` panic in `VideoFrame::rgba_image()` for NV12/I420 when `h264`/`av1` features are disabled. The conversion functions should be available without codec features. Returns `&RgbaImage` via `OnceLock::get_or_init`, so can't return `Result` without a signature change (`format.rs:787, 805`). (Previously ER21.) **Action: make conversion functions available without any codec features.**

- [ ] **BUG-3**: `Rc<Display>` cross-thread drop race in VAAPI decoder. `VaapiGpuFrame` clones `Rc<Display>` on the decode thread; if the frame is dropped on a different thread, the non-atomic refcount is a data race (UB). Blocked on cros-libva using `Rc<Display>` instead of `Arc<Display>` — needs upstream change or `Arc<Mutex<Rc<Display>>>` wrapper (`vaapi/decoder.rs`). (Previously ER5.)

- [x] **BUG-4**: ~~PipeWire DMA-BUF modifier~~ — Fixed in `e875a0e`: parses SPA_FORMAT_VIDEO_modifier.

- [x] **BUG-5**: ~~PipeWire NV12 single-plane~~ — Fixed in `e875a0e`: reports Y + UV planes with correct offsets.

- [ ] **BUG-6**: V4L2 NV12 size calculation ignores `bytesperline` stride padding. The code hardcodes `y_size = width * height` instead of using the driver-reported stride. Corrupted frames on drivers with row padding (common on hardware ISPs) (`v4l2.rs:415`). (Previously ER18.)

- [x] **BUG-7**: ~~Relay pull TOCTOU~~ — Fixed in `e875a0e`: Connecting sentinel + Notify.

- [x] **BUG-8**: ~~Relay pull cleanup~~ — Fixed in `8901059`: cluster lifecycle, no idle timeout.

- [ ] **BUG-9**: iroh-moq actor session map dedup race. `sessions.insert(remote, session)` replaces the old entry. The old session's close task returns `(remote, res)`, which could remove or update the new session's entry, prematurely marking it as closed (`iroh-moq/src/lib.rs:487`). (Previously DR14.)

- [ ] **BUG-10**: GPU download panic in `OnceLock::get_or_init`. `gpu.download_rgba().expect("GPU frame download failed")` in `rgba_image()` panics on GPU context loss, driver bugs, or FD exhaustion. Needs a fallible signature (`format.rs:768`). (Previously DR17.)

- [ ] **BUG-11**: `FrameData::I420` has no stride fields. `rgba_image()` hardcodes `y_stride = width` and `uv_stride = width.div_ceil(2)`. Current I420 producers strip strides during conversion so this works today, but a future producer storing padded I420 would get silent corruption. Add stride fields for parity with NV12 (`format.rs:553`). (Previously DR2.)

- [ ] **BUG-12**: Android MediaCodec `set_bitrate` never takes effect under normal operation. The new bitrate is stored but only applied on codec reset, which only triggers after `MAX_CONSECUTIVE_ERRORS` (3) consecutive errors. No API to force a reset (`android/encoder.rs:349`). (Previously ON13.)

---

## Performance

- [ ] **PERF-1**: Double YUV-RGB-YUV conversion on encode path. NV12 sources go NV12 -> RGBA (via `rgba_image()`) -> YUV420 via `pixel_format_to_yuv420` in the software H.264 encoder. Roughly 5 ms/frame at 1080p. The I420 fast path exists but NV12 input falls through to the RGBA conversion (`h264/encoder.rs:240-247`). (Previously P16.)

- [ ] **PERF-2**: Per-frame Vulkan resource allocation in DMA-BUF import. The NV12 VkImage is re-created each frame (import + destroy cycle). The R8/RG8 copy-target textures are cached, but the source VkImage is not (`render/dmabuf_import.rs:288-311`). (Previously B6.)

- [ ] **PERF-3**: Per-frame DMA-BUF re-export in VAAPI decoder. `export_prime()` is called on every decoded frame. Surface export metadata (plane layouts, modifiers) could be cached when the surface format is stable (`vaapi/decoder.rs:230`). (Previously P18.)

- [ ] **PERF-4**: Per-frame TextureView + BindGroup in NV12 render path. `render_nv12_from_views` creates a new `wgpu::BindGroup` every frame. Should cache when fd/modifier/dimensions match (`render.rs:366`). (Previously RC12.)

- [ ] **PERF-5**: All encoder backends use CPU scaling even when the HW encoder API supports native scaling. VAAPI has VPP, V4L2 M2M can scale on OUTPUT queue, VideoToolbox accepts arbitrary input dimensions, MediaCodec Surface mode does GPU scaling. Per-backend investigation needed (`codec/*/encoder.rs`). (Previously DR50.)

- [ ] **PERF-6**: Android MediaCodec encoder uses ByteBuffer mode with CPU scaling. Should use Surface input mode for zero-copy (`android/encoder.rs`). (Previously RC15.)

---

## API and design

- [ ] **API-1**: Multiple methods for getting a `VideoTrack` from `RemoteBroadcast` (`video()`, `video_with()`, `video_rendition()`). The relationship between them is not obvious from type signatures alone. Could benefit from a builder pattern: `remote.video().with_quality(Quality::Best).build()` (`subscribe.rs:579-601`). (Previously E2.)

- [ ] **API-2**: `Call::dial(live, remote, local)` and `Call::accept(session, local)` have asymmetric first arguments (`&Live` vs `MoqSession`). A `live.accept_call(local)` convenience that waits for the next incoming session would simplify the callee path (`call.rs:54-74`). (Previously E3.)

- [ ] **API-3**: `LocalBroadcast::producer()` exposes internal `BroadcastProducer`. Bypasses catalog/rendition safety (`publish.rs:248`). (Previously D15.)

- [ ] **API-4**: No observability in `VideoTrack` adaptation. No way to query probe state or decision reasons (`adaptive.rs`). (Previously D16.)

- [ ] **API-5**: `Quality` enum is coarse — four fixed presets, no custom resolution/bitrate (`format.rs:893`). (Previously A3.)

- [ ] **API-6**: `DecodeConfig` minimal — only `pixel_format` and `backend`, no resolution/framerate constraints (`format.rs:922`). (Previously A4.)

- [ ] **API-7**: `AdaptiveConfig` has 12 fields, no builder methods. Callers must use struct literal with `..Default::default()`. Add builder methods for commonly-adjusted fields (`adaptive.rs:47-72`). (Previously ER16.)

- [ ] **API-8**: `block_on` inside egui `update()` for room peer subscription. `RemoteBroadcast::media()` is async but room events arrive synchronously. Room should either emit pre-decoded tracks or provide a sync subscription path (`iroh-live-cli/src/ui.rs:552`). (Previously ER19.)

- [ ] **API-9**: Two `VideoCodec` types across sibling modules (`rusty_codecs::codec::VideoCodec` for encoding at `codec.rs:84`, `rusty_codecs::config::VideoCodec` for decoder config at `config.rs:54`). Rename to avoid confusion before any external API stabilization. (Previously ER22.)

- [ ] **API-10**: `PublishError` and `SubscribeError` lack `std::error::Error` source chains. `EncoderFailed(anyhow::Error)` and `DecoderFailed(anyhow::Error)` variants wrap inner errors invisible to `source()`. Use `derive_more::Error` or `thiserror` (`publish.rs:55-77`, `subscribe.rs:182-207`). (Previously ER26.)

- [ ] **API-11**: `IncomingSessionStream` uses `broadcast` channel (capacity 16). The `broadcast` delivers all sessions to all receivers, which is wrong for a typical single-accept-loop pattern. Under connection storms (>16 pending), sessions are lost. Consider `mpsc` with a single receiver (`iroh-moq/src/lib.rs:86`). (Previously DR13.)

- [ ] **API-12** *(design decision needed)*: `LiveTicket` URI format drops relay URLs on round-trip. `serialize()` encodes only `{SCHEME}{addr}/{name}`; `deserialize_url()` always returns `relay_urls: Vec::new()`. The `to_bytes`/`from_bytes` postcard path correctly preserves them. The URI format needs a relay URL encoding or should document the limitation (`ticket.rs`). (Previously DR8.)

- [ ] **API-13**: Rendition switches via `VideoTrack` adaptation not seamless. Old decoder dropped immediately, new decoder waits for keyframe, causing visible glitch. Fix: start new decoder first and switch once first frame of new decoder is ready (`adaptive.rs`). (Previously ER9.)

- [ ] **API-14** *(design decision needed)*: Asymmetric encoder/decoder factory pattern. Encoders have a separate `VideoEncoderFactory` trait; decoders put `new()` on the `VideoDecoder` trait with `Self: Sized` bound (preventing `dyn VideoDecoder::new()`). Confusing but functional (`traits.rs:345-367`). (Previously DR25.)

- [ ] **API-15**: `VideoOptions::resolve_quality` ignores `max_bitrate_kbps`. The `VideoTarget` struct has a `max_bitrate_kbps` field but `resolve_quality()` only consults `max_pixels`. Unused field (`subscribe.rs:59, 140-158`). (Previously DR28.)

- [ ] **API-16**: No timeout on `RemoteBroadcast::new` catalog wait. If the remote peer connects but never sends a catalog, the subscription hangs indefinitely. Add a configurable timeout with a reasonable default (`subscribe.rs:313-337`). (Previously ER25.)

---

## Audio

- [ ] **AUD-1**: AEC Mutex on real-time thread. `AecProcessor` holds `Arc<Mutex<AudioProcessing>>` acquired on the cpal input callback. Becomes an RT violation if `set_stream_delay` is wired up. Use `try_lock` with passthrough fallback (`audio_backend/aec.rs:72, 119`). (Previously AB3.)

- [ ] **AUD-2**: AEC processing errors silently discarded. `process_render_f32` / `process_capture_f32` results are ignored (line 311, 347 in `aec.rs`). Add an atomic error counter. (Previously AB5.)

- [ ] **AUD-3**: No clock drift correction between input/output. cpal streams run on independent hardware clocks. Monitor fill level, micro-resample (`audio_backend`). (Previously AB7.)

- [ ] **AUD-4**: Opus encoder ignores requested input sample rate — hardcodes 48 kHz per Opus spec. `processing/resample.rs` has a rubato-based resampler but it is not wired into the encoder path yet (`opus/encoder.rs:15`). (Previously B10.)

- [ ] **AUD-5**: Audio and video not synchronized — PTS timebase mismatch. Wall-clock audio PTS is implemented, but each encoder thread uses its own `Instant::now()`, so a 50-200 ms constant offset remains depending on capture startup order. **Remaining work:** pass a shared `Instant` to both audio and video encoder pipelines so PTS=0 means the same wall-clock moment (`pipeline/audio_encode.rs:92`). (Previously ER4.)

---

## Testing gaps

- [ ] **TEST-1**: No `render.rs` tests. `WgpuVideoRenderer` entirely untested (needs GPU) (`rusty-codecs/src/render.rs`). (Previously T3.)

- [ ] **TEST-2**: `PublishCaptureController` not tested. `set_opts` has no tests (`publish/controller.rs`). (Previously T5.)

- [ ] **TEST-3**: No fuzz tests. Codec decoders not fuzzed with malformed input (`rusty-codecs/src/codec/`). (Previously T6.)

- [ ] **TEST-4**: No test for V4L2 DMA-BUF export path (`VIDIOC_EXPBUF`). Regression risk on driver changes (`rusty-capture`). (Previously CAP5.)

- [ ] **TEST-5**: No unit tests in `moq-media-android`. Camera source, handle helpers, and EGL wrappers all untested (`moq-media-android/src/`). (Previously MA4.)

---

## Platform-specific

### Linux — PipeWire / V4L2 / X11

- [ ] **PLAT-1**: V4L2 DMA-BUF EXPBUF path not implemented. Docs claim zero-copy via EXPBUF but code always does CPU copies (`v4l2.rs`). (Previously ER3.)

- [ ] **PLAT-2**: V4L2 no `VIDIOC_S_PARM` for frame rate. Intervals enumerated but never set (`v4l2.rs`). (Previously ER11.)

- [ ] **PLAT-3**: X11 resolution changes not handled. Width/height captured once at construction, never updated. Will crash or garble output on resolution change (`x11.rs:174-175`). (Previously ER12.)

- [ ] **PLAT-4**: PipeWire stopper thread uses 50 ms polling loop instead of condvar (`pipewire.rs:835`). (Previously ER16.)

- [ ] **PLAT-5**: PipeWire portal thread panics are unobserved. `recv_timeout()` catches both timeout and channel disconnect, but the panic backtrace is lost and the error message does not distinguish the two cases (`pipewire.rs:1083`). (Previously CAP6.)

- [ ] **PLAT-6**: No VAAPI-to-Vulkan explicit sync for DMA-BUF import. `vaSyncSurface` is called before export (ensuring VAAPI writes are complete via CPU ordering), so this works on Intel implicit sync. May fail on GPUs that require explicit cross-API sync (`render/dmabuf_import.rs`). (Previously B8.)

- [ ] **PLAT-7**: V4L2 decoder discards timestamps. All decoded frames get `Duration::ZERO` instead of recovering timestamps from the V4L2 dequeued buffer (`v4l2/decoder.rs:394, 421`). (Previously DR39.)

### xcap backend

- [ ] **PLAT-8**: Monitor resolution change silently updates dimensions mid-stream. `pop_frame()` detects width/height change and updates `self.dimensions`, but downstream encoders cache `format()` at startup. Can cause encoder errors or corrupted frames on monitor hot-plug (`xcap_impl.rs:161-169`). (Previously CAP10.)

- [ ] **PLAT-9**: Monitor ID parse failure silently falls back to index 0. `id.strip_prefix("xcap-").and_then(|s| s.parse().ok()).unwrap_or(0)` means invalid IDs open the wrong monitor with no error (`xcap_impl.rs`). (Previously CAP11.)

- [ ] **PLAT-10**: Duplicate `Monitor::all()` enumeration in `new()`. Caller already enumerated to get `MonitorInfo`, constructor re-enumerates to find the same monitor by index. Redundant system call and potential race (`xcap_impl.rs`). (Previously CAP12.)

- [ ] **PLAT-11**: `monitors()` uses `unwrap_or(0)` for width/height. Creates `MonitorInfo` with 0x0 dimensions that will fail downstream. Should skip the monitor or propagate the error (`xcap_impl.rs:25-26`). (Previously CAP13.)

### nokhwa backend

- [ ] **PLAT-12**: Format enumeration errors silently swallowed. `enumerate_formats()` failure produces an empty format list with a `warn!` log. User sees a camera with zero formats, no way to distinguish "camera has no formats" from "query failed" (`nokhwa_impl.rs:50`). (Previously CAP16.)

### macOS *(waiting for hardware)*

- [ ] **PLAT-13**: `AppleCameraCapturer` entire backend is a stub. `new()` logs a warning and produces zero frames. `cameras()` returns a hardcoded placeholder (`rusty-capture/src/platform/apple/camera.rs`). (Previously DR5.)

- [ ] **PLAT-14**: Metal importer has no GPU fence in double-buffer scheme. Keeps previous frame's `CVMetalTextureRef` resources alive for one extra frame (~33 ms at 30 fps). Unlikely to cause issues in practice (`render/metal_import.rs:260`). (Previously DR32.)

### Android

- [ ] **PLAT-15**: `CameraFrameSource` allocates per-frame. Could pool frame buffers when resolution is stable (`moq-media-android/src/camera.rs`). (Previously MA1.)

- [ ] **PLAT-16**: `SharedCameraSource` uses `std::sync::Mutex`. `parking_lot::Mutex` would avoid poisoning (`moq-media-android/src/camera.rs:89`). (Previously MA2.)

- [ ] **PLAT-17**: EGL function pointers resolved lazily via `OnceLock` with no error recovery if `dlopen` fails (`moq-media-android/src/egl.rs:26-30`). (Previously MA3.)

- [ ] **PLAT-18**: `renderFrame` does GL calls without EGL context check. A debug assertion would catch misuse (`demos/android/rust/src/lib.rs:639`). (Previously AD2.)

- [ ] **PLAT-19**: `JNI_OnLoad` does not warm up cpal/Oboe. Lazy init causes a latency spike on first call (`demos/android/rust/src/lib.rs:68`). (Previously AD3.)

- [ ] **PLAT-20**: No JNI exception checking. No `exception_check()` / `exception_clear()` after JNI calls; a pending exception combined with continued JNI calls is undefined behavior (`demos/android/rust/src/lib.rs`). (Previously AD4, ON14.)

### Windows / Android capture stubs

- [ ] **PLAT-21**: Android and Windows capture backends are pure documentation stubs with no runtime code (`rusty-capture/src/platform/android/mod.rs`, `windows/mod.rs`). (Previously DR6.)

---

## Minor and code hygiene

- [ ] **HYGIENE-1**: `CatalogSnapshot` `PartialEq` skips inner catalog (`#[eq(skip)]`). Equality based solely on `seq` number, which could surprise callers comparing snapshots from different broadcasts. Document this (`subscribe.rs:251`). (Previously ER29.)

- [ ] **HYGIENE-2**: `VideoTrack::from_video_source` hardcodes `fps = 30` and uses fixed-cadence pacing. Sources faster than 30 fps are throttled; sources slower spin on empty `pop_frame()`. Should use PTS-based pacing or accept fps as a parameter (`subscribe.rs:855`). (Previously ER30, NR7.)

- [ ] **HYGIENE-3**: Room actor event channel capacity 16. In rooms with many simultaneous joins, the actor blocks on sends and stops processing gossip. Consider increasing capacity or `try_send` with logged warning (`rooms.rs:104-105`). (Previously ER31.)

- [ ] **HYGIENE-4**: Room gossip dependency is implicit. `Room::new()` fails at runtime if gossip is not enabled; no type-level guard (`rooms.rs`). (Previously IL13.)

- [ ] **HYGIENE-6**: `LocalBroadcast::run` holds state mutex while calling `start_track`. Encoder factory calls can take tens of milliseconds for hardware codecs (VAAPI/VTB device negotiation), blocking all `has_video()`, `has_audio()`, `set_video()` callers. Factory call and thread spawn should happen after releasing the lock (`publish.rs:294-297`). (Previously ER24.)

- [x] **HYGIENE-7**: ~~Duplicate catalog publishes~~ — Fixed in `863be2e`: CatalogProducer caches last published string.

- [ ] **HYGIENE-8**: Room actor does not retry subscriptions when a remote broadcast disconnects. If the remote peer re-announces via gossip KV without a state change, the room may miss the reconnection opportunity (`rooms.rs:310-313`). (Previously NR8.)

- [ ] **HYGIENE-9**: `Subscription::new` spawns stats recorder and signal producer tasks without storing `AbortOnDropHandle`s. The orphaned tasks keep polling a stale connection if the subscription is moved and the token outlives it (`iroh-live/src/subscription.rs:28-36`). (Previously NR6.)

- [ ] **HYGIENE-10**: `Metric::record()` has non-atomic EMA update (load -> compute -> store on `AtomicU64` without CAS). Single-writer invariant enforced by architecture. Benign by design but worth noting (`stats.rs:110`). (Previously DR18.)

- [ ] **HYGIENE-11**: RGBA/BGRA conversion pairs are copy-paste — ~200 lines in `convert.rs` where each pair differs only in which `yuvutils_rs` function is called. (Previously DR33.)

- [ ] **HYGIENE-12**: `VideoCodec::available()` returns `Vec<Self>`. Should return `&'static [Self]` since values are known at compile time (`codec.rs:62, 112`). (Previously DR34.)

- [ ] **HYGIENE-13**: `VideoPreset::fps()` returns 30 for all presets, hardcoded regardless of variant. Doc should note this (`format.rs:856`). (Previously DR37.)

- [ ] **HYGIENE-14**: `PipeSink::finish()` is a silent no-op, unlike `MoqPacketSink::finish()` which signals stream end. Semantics differ between `PacketSink` implementations (`transport.rs:146`). (Previously DR38.)

- [ ] **HYGIENE-15**: All RGB-YUV conversions hardcode BT.601 Limited regardless of resolution. Technically wrong for HD (720p+) per ITU-R BT.709. Internally consistent since encoders signal BT.601 in metadata (`processing/convert.rs`). (Previously DR23.)

- [ ] **HYGIENE-16**: Room publisher fire-and-forget spawns. `RoomPublisherSync::new()` and `handle_new_screen()` spawn tokio tasks without storing handles. Errors are logged at `warn!` level (`rooms/publisher.rs`). (Previously DR30.)

- [ ] **HYGIENE-17**: `controller.rs::apply_audio` spawns fire-and-forget tokio task. No handle tracking, rapid `set_opts` calls race, errors only logged at `warn!` (`publish/controller.rs:295`). (Previously DR48.)

- [ ] **HYGIENE-18**: `decode_loop` is ~230 lines. Could benefit from extracting helpers (`pipeline/video_decode.rs:183`). (Previously DR43.)

- [ ] **HYGIENE-19**: `overlay.rs` `show()` and `show_publish()` share ~100 lines of duplicated bottom bar rendering code (`moq-media-egui/src/overlay.rs`). (Previously DR44.)

- [ ] **HYGIENE-20**: `DiscoveredVideoSource` enum and `discover_video_sources()` duplicated across `iroh-live-cli/src/ui.rs` and `iroh-live/examples/split.rs`. Extract to `examples/common/` or a shared utility module. (Previously DR45.)

- [ ] **HYGIENE-21**: H.264 SPS patcher module is effectively dead code. `patch_sps_low_latency` is `pub(crate)` with tests but commented out at all call sites in `vaapi/decoder.rs` (`codec/h264/sps.rs`). (Previously DR46.)

- [ ] **HYGIENE-22**: `select_rendition` fallback to `renditions.keys().next()` returns the lexicographically first key, which may not be the lowest quality if rendition names don't sort by quality. Currently moot because BUG-1 means this fallback *always* fires, but will matter once BUG-1 is fixed (`subscribe.rs:743`). (Previously ER27.)

- [ ] **HYGIENE-23**: `MoqPacketSource` sequence counter starts at 0 and increments before use, so the first packet is logged as `seq=1`. Minor off-by-one in logging only (`transport.rs:49`). (Previously NR11.)

- [ ] **HYGIENE-24**: `moq-media-dioxus::DioxusVideoRenderer::render` directly accesses `self.track.0` (the internal `Arc<Mutex<>>`) instead of going through a public method. Couples the renderer to the handle's internal representation. Add a `VideoTrackHandle::current_frame()` method (`moq-media-dioxus/src/lib.rs:154-160`). (Previously NR12.)

- [ ] **HYGIENE-25**: `VideoDecoderPipeline` initializes viewport to `(1u32, 1u32)`. If the subscriber never calls `set_viewport`, the decoder sees a 1x1 viewport for its lifetime. Currently harmless but could become a problem if viewport-aware downscaling is added (`pipeline/video_decode.rs:98`). (Previously NR13.)

- [ ] **HYGIENE-26**: Relay `serve_fingerprint` panics on lock poisoning. Acceptable in dev mode; a production relay would use real TLS certificates (`iroh-live-relay/src/lib.rs:221`). (Previously DR41.)

- [ ] **HYGIENE-27**: `AecProcessor::set_stream_delay()` and `set_enabled()` are `pub(crate)` internal methods, `#[allow(unused)]`. Internal dead code reserved for future AEC work (`audio_backend/aec.rs:110, 164`). (Previously DR7.)

---

## Dev branch review (origin/dev..dev)

Review of 21 commits on the `dev` branch ahead of `origin/dev`, covering
bug fixes, performance work, new features (rooms chat, record command,
headless demo), and API additions. Reviewed 2026-03-27.

### Bugs and correctness

- [ ] **DEV-1** *(medium)*: Rendition suffix match is ambiguous. `select_rendition` uses `ends_with` matching (`4456d77`), so a preset string `"p"` would match `"video/h264-720p"`, `"video/av1-1080p"`, etc. More critically, if rendition keys like `"video/h264-1080p"` and `"video/h264-360p"` are both present and the suffix is `"0p"`, both match and `find` returns whichever appears first in `BTreeMap` iteration order. Current `VideoPreset` values (`"180p"`, `"360p"`, `"720p"`, `"1080p"`) happen to be unique suffixes in practice, but the matching is fragile: a custom rendition name ending in the same suffix would confuse selection. Consider matching on the segment after the last `-` instead of bare `ends_with` (`subscribe.rs:737`).

- [ ] **DEV-2** *(low)*: VAAPI SPS/PPS retry loop has no iteration limit. The `loop` in `VaapiDecoder::new` (`e8c6a9f`) handles `CheckEvents` and `NotEnoughOutputBuffers` by draining events and retrying, but if cros-codecs returns these errors indefinitely (e.g., broken SPS data triggers repeated `FormatChanged` events), the loop never exits. In practice the `remaining` slice shrinks on each successful `Ok(consumed)`, and `consumed == 0` breaks, so infinite loops require the decoder to consume zero bytes repeatedly on errors. Low risk, but a max-iteration guard (say, 32) would make it defensive (`vaapi/decoder.rs:516-540`).

- [ ] **DEV-3** *(low)*: `render_to_owned_texture` does not handle NV12 GPU frames. The method (`568af84`) calls `self.render(frame)` which handles all formats, then copies from `output_texture`. However, the owned texture is created with `Rgba8UnormSrgb` format. If the internal render path ever changes to output a non-sRGB format, the `copy_texture_to_texture` would fail silently (textures with mismatched formats fail the wgpu validation). Currently safe because `render()` always writes to an `Rgba8UnormSrgb` output texture, but the format is hardcoded in two places (`render.rs:547, 712`).

- [ ] **DEV-4** *(low)*: `VideoSourceSpec` parsing changes semantics of single-segment `cam:<string>`. The old code in `args.rs` tried to parse `parts[0]` as `CaptureBackend` first, then fell back to device index/name. The new code in `source_spec.rs` treats single non-numeric segments as backend names (via `BackendRef::Name`). This means `cam:my-webcam` used to be parsed as `DeviceRef::Name("my-webcam")` but now parses as `BackendRef::Name("my-webcam")`. The `resolve_backend` in `source.rs` calls `name.parse::<CaptureBackend>()` which will fail with a helpful error, so this does not silently break, but it is a behavior change for users who specified devices by name without a backend prefix. Document or restore the old disambiguation heuristic (`source_spec.rs:153-159`).

- [ ] **DEV-5** *(info)*: `SyncInner::Drop` calls `get_mut().unwrap()` on a `Mutex`. If any thread panicked while holding the Sync lock, this unwrap panics during drop, which in turn aborts the process (panic in Drop during another panic). The only code that holds the lock is `Sync::wait()` inside the decode thread, and panics there would already be a critical failure, so the risk is academic. Using `get_mut().unwrap_or_else(|e| e.into_inner())` to recover from poisoning would be strictly safer (`sync.rs:67`).

### Performance

- [x] **DEV-6** *(done)*: Scaler double-buffer eliminates per-frame clone (PERF-5/DR20 fixed). Marks PERF-5 above as resolved.

- [x] **DEV-7** *(done)*: Decoder pixel buffer reuse eliminates per-frame YUV conversion allocation. Good improvement, no issues found.

- [ ] **DEV-8** *(low)*: Scaler double-buffer loses reuse when resolution changes. When `target_width/target_height` change (e.g., viewport resize), both buffers in `dst_bufs` will have stale dimensions and allocate fresh. The old single-buffer scheme had the same behavior, so this is not a regression, just an observation.

- [ ] **DEV-9** *(info)*: `mul_f64` in audio tick timing (`7966429`) introduces floating-point imprecision. At `tick_num = 2^53` (about 2.85 billion years at 10ms), the f64 conversion loses precision and tick cadence becomes irregular. Not a practical concern but worth noting the fix trades overflow correctness for floating-point imprecision -- a good tradeoff.

### API and compatibility

- [ ] **DEV-10** *(medium)*: `AudioSinkHandle` now requires `Sync` (`0ffcd95`). This is a breaking change for any downstream `AudioSinkHandle` implementor that uses interior mutability with non-Sync types (e.g., `Cell`, `RefCell`). All in-tree implementations use `Arc<Mutex<>>` or atomics, so they are fine, but external implementations may break. The trait was previously `Send + 'static`; now it is `Send + Sync + 'static`. This should be called out in release notes.

- [ ] **DEV-11** *(low)*: `h264` module and `annexb` module changed from `pub(crate)` to `pub` (`71bde95`). This exposes internal H.264 parsing functions (`avcc_to_annex_b`, `length_prefixed_to_annex_b`, `parse_annex_b`, `extract_sps_pps`, `build_avcc`) as public API of `rusty-codecs`. These are useful for the record command but create a new public API surface that needs stability consideration. Consider `#[doc(hidden)]` or a separate public utility module if this is not intended as a stable API.

- [ ] **DEV-12** *(info)*: `LocalBroadcast::enable_chat` takes `&mut self` (`fab3327`), but most other broadcast methods take `&self`. The `setup` function in `room.rs` needed to change the local binding to `let mut broadcast`. This is consistent with the method's "can only be called once" semantics, but the asymmetry may surprise callers.

- [ ] **DEV-13** *(info)*: `Room::split()` returns `(RoomEvents, RoomHandle)` -- new public API that separates event consumption from command sending. Good pattern for UI integration. No issues found.

### Rooms and chat

- [x] **DEV-14** *(done)*: Postcard serialization roundtrip tested with `peer_state_serialization_roundtrip`. The comment about `skip_serializing_if` is correctly documented in the struct. Good fix.

- [ ] **DEV-15** *(medium)*: Chat messages are not authenticated. Any peer in the room can claim to be any other peer by forging the `remote` field in `RoomEvent::ChatReceived`. The `remote` comes from the gossip endpoint ID, which is authenticated by the transport layer, but the chat text comes from the broadcast's track data which is also keyed by endpoint. This is probably fine -- the transport already authenticates the sender. However, the comment in `chat.rs` says "sender's identity comes from the broadcast context" which is correct.

- [ ] **DEV-16** *(low)*: `Room::send_chat` silently succeeds when no chat publisher is set -- it sends an `ApiMessage::SendChat` to the actor which logs a warning, but the caller gets `Ok(())`. The error path is a `warn!` log in the actor, not a returned error. This could confuse callers who forgot to call `enable_chat`/`set_chat_publisher`.

- [ ] **DEV-17** *(low)*: Chat subscriber task in room actor is not cleaned up when a broadcast closes. When `broadcast.chat()` returns a `ChatSubscriber` and the subscriber's `recv()` returns `None` (track closed), the future completes and is not re-enqueued. This is correct. But if the `BroadcastSubscribed` event is sent for a new broadcast from the same peer, a second chat subscriber is created. Multiple chat subscribers for the same peer are harmless (they read the same track), but the `chat_messages` FuturesUnordered grows without bound across reconnections.

### Record command

- [ ] **DEV-18** *(low)*: `record_raw_track` writes raw Opus packets without any framing. The output `.opus` file contains concatenated Opus packets with no length delimiters or timestamps. This cannot be played back correctly since Opus packets are variable-length. The remux hint printed after recording suggests `ffmpeg -f opus -i file.opus`, but ffmpeg's raw Opus demuxer expects self-delimiting Opus packets (RFC 6716 appendix B). Standard Opus packets without OggS framing will fail. The H.264 path is fine because Annex B start codes provide framing.

- [ ] **DEV-19** *(info)*: `record_video_track` uses `copy_to_bytes(payload.remaining())` which allocates a contiguous copy even when the payload is already contiguous (single-chunk Bytes). Minor allocation overhead per frame -- could check `payload.chunk().len() == payload.remaining()` first.

### Testing

- [x] **DEV-20** *(done)*: Room integration tests cover the exact scenarios the postcard bug broke: `two_peers_see_each_other`, `subscribe_and_receive_video_frames`, `chat_messages_flow`, `peer_disconnect_detected`, `peer_joined_fires`, and `peer_state_serialization_roundtrip`. Good coverage.

- [x] **DEV-21** *(done)*: VAAPI AVCC decode path has both a software test (`decode_avcc_file_import_path` in `h264/decoder.rs`) and a hardware-gated test (`vaapi_decode_avcc_file_import_path` in `vaapi/decoder.rs`). Good coverage.

- [ ] **DEV-22** *(low)*: `VideoTrack::try_recv` has no dedicated test. It is an alias for `current_frame` which is tested, so the risk is minimal, but a basic test would verify the alias works.

- [ ] **DEV-23** *(low)*: `render_to_owned_texture` has no test (TEST-1 already notes the broader render.rs testing gap). Would need GPU hardware to test.

### Code hygiene

- [x] **DEV-24** *(done)*: `SharedVideoSource` atomic ordering standardized on `AcqRel`/`Acquire`/`Release` (marks HYGIENE-1 as resolved).

- [ ] **DEV-25** *(info)*: Headless demo added to `default-members` in workspace `Cargo.toml`. This means `cargo build` in the workspace root now builds the demo, which depends on `test-util` feature. Not a problem, but `test-util` pulls in test-only source generators that are not needed for production builds.

- [ ] **DEV-26** *(info)*: `iroh-live-cli` now depends on `hang` directly (`71bde95`) for `hang::catalog::{AudioCodec, VideoCodec, VideoConfig}` in the record command. Previously it accessed hang types only through moq-media re-exports. This is fine but adds a direct dependency edge.
