# Disposition register: every iroh-live media module and its fate

> Campaign: upstream | Kind: reference | The standing register of every
> iroh-live media module. Read 0-overview.md first.

This register exists so that no module is silently dropped. Every media module
in the workspace has exactly one row and exactly one disposition, and the
register is updated whenever a cut lands, together with the deletion ledger in
`cut-plan.md` (coordination point 12 in `0-overview.md`). A module that is not
in this table is a bug in this table.

The disposition vocabulary:

- **upstream-ours**: we contribute our implementation to moq; the linked leaf
  plan owns the work, and the local module is deleted only after the upstream
  contribution merges and releases (the paired counterpart branch carries the
  cut).
- **adopt-theirs**: moq's implementation wins; we adopt it and delete ours.
  Where our version carries fixes or capabilities theirs lacks, the Notes name
  them and the porting is tracked in `capture/parity-ports.md`.
- **keep**: the module stays in iroh-live, with the reason stated. Rows owned
  by the sibling `plans/align-to-moq/` campaign are marked as such.
- **defer**: parked with a written plan and a stated re-entry condition.
- **drop**: deleted with no replacement, with the reason that is acceptable.

Inventory and LOC come from `comparisons/iroh-live-code-map.md` (`wc -l`,
2026-07-18); verdicts consolidate `cut-plan.md` and the `comparisons/`
documents. Scope: this register covers the media modules (rusty-codecs,
rusty-capture, moq-media, the render tree, the audio device layer, and the
media support crates). The room and transport layer (the `iroh-moq` and
`iroh-live` crates) is owned by `plans/align-to-moq/` and registered in
`cut-plan.md`; it has no rows here. `iroh-live-cli`, `iroh-live-relay`, and
`demos/` are applications composing these libraries; they change with every
adoption but contain no owned media implementation and have no rows.

## rusty-codecs (22,310 LOC)

| Module | Path | LOC | Disposition | Where the work lives | Notes |
|---|---|---:|---|---|---|
| Codec and device traits | `rusty-codecs/src/traits.rs` | 410 | keep | `counterpart/codec-remove.md` shrinks it | The codec trait half falls away with codec adoption (cut-plan traits.rs merge row). The device traits (`AudioSink`, `AudioSinkHandle`, `AudioSource`, `AudioStreamFactory`, `VideoSource`) stay serving the audio engine and capture until `audio/audio-device-unify.md` and the capture adoption settle their consumers. |
| Frame data model | `rusty-codecs/src/format.rs` | 1,292 | upstream-ours | `base/B1-frame-vocabulary.md` | `NativeFrameHandle` and `DmaBufInfo` are the donors for the public `Native` vocabulary (B1). The rest of the frame model is replaced by moq's `Frame` and deleted with codec adoption (`counterpart/codec-remove.md`). |
| Backend dispatch | `rusty-codecs/src/codec.rs`, `codec/dynamic.rs` | 522 | adopt-theirs | `counterpart/codec-remove.md` | moq's `Candidate`/`Kind` table wins (`Named` pinning, tried-list errors; codecs.md sec 8). Our `reset()` and `burst_size()` decoder needs are ported into moq's decode trait via `codec/vaapi-decode.md`, not lost. |
| Catalog config mirror | `rusty-codecs/src/config.rs` | 318 | adopt-theirs | align-to-moq stage 1 (cut-plan) | Replaced by direct `hang::catalog` types. The mirror no longer compiles against hang 0.19.5 (codecs.md, final section), so this cut is already forced. |
| openh264 encode and decode | `rusty-codecs/src/codec/h264/encoder.rs`, `decoder.rs` | ~906 | adopt-theirs | `counterpart/codec-remove.md` | Theirs adds tested live retune and per-frame IDR; ours adds nothing they need (codecs.md sec 1). Nothing to port; the avcC output mode is parked, recorded in `capture/parity-ports.md`. |
| Annex-B and avcC tooling | `rusty-codecs/src/codec/h264/annexb.rs` | 364 | adopt-theirs | `counterpart/codec-remove.md` | `moq_mux::codec` is more general (variable length size, multiple parameter sets, hvcC; codecs.md sec 7). `build_avcc` (avc1 production) is parked, recorded in `capture/parity-ports.md`. |
| SPS VUI patcher | `rusty-codecs/src/codec/h264/sps.rs` | 586 | upstream-ours | `codec/bitstream-sps-vui.md` | Offered upstream as an optional low-latency decode pass; dead code locally today. |
| VAAPI encode and decode | `rusty-codecs/src/codec/vaapi/` | 3,257 | upstream-ours | `codec/vaapi-encode.md`, `codec/vaapi-decode.md` | The strongest upstream candidate: zero-copy DMA-BUF import, VPP, PRIME export, and the only VAAPI decode on either side (codecs.md sec 1). |
| V4L2 M2M encode and decode | `rusty-codecs/src/codec/v4l2/` | 1,856 | upstream-ours | `codec/v4l2-encode.md`, `codec/v4l2-decode.md` | The ARM SoC and Raspberry Pi story; the driver stride and alignment handling (commit 625c16f) travels with the port. |
| VideoToolbox encoder | `rusty-codecs/src/codec/vtb/encoder.rs` | 895 | adopt-theirs | `counterpart/codec-remove.md` | Theirs wins on H.265, High profile, per-frame IDR, and ExpectedFrameRate (codecs.md sec 1). Nothing to port; the avcC output mode is parked, recorded in `capture/parity-ports.md`. |
| VideoToolbox decoder | `rusty-codecs/src/codec/vtb/decoder.rs` (+ mod) | ~599 | upstream-ours | `codec/vtb-mf-decode-surface.md` | moq's decoder wins on H.265 and shares session handling, but downloads every frame to CPU I420. Our GPU-resident `CVPixelBuffer` output is the contribution; the local module is deleted once the surface-retention path lands. |
| Android MediaCodec | `rusty-codecs/src/codec/android/` | 1,528 | upstream-ours | `codec/android-mediacodec.md` | Encoder plus both decoders including the zero-copy HardwareBuffer path. Gated on the B4 placement decision (coordination point 6 in `0-overview.md`). |
| Software AV1 | `rusty-codecs/src/codec/av1/` | 936 | defer | `codec/av1-software.md` (deferred) | rav1e is too slow at speed and the rav1d git-fork pin too heavy to carry now. The local backend is removed meanwhile (`counterpart/codec-remove.md`); revisit when a use case needs AV1 and the rav1d dependency is resolvable. |
| Opus wrapper | `rusty-codecs/src/codec/opus/` | 804 | upstream-ours | `audio/opus-improvements.md` | The wrapper shell is adopted from moq-audio; our control surface (runtime `set_bitrate`, lookahead pre-skip, FEC/DTX ctl plumbing, decoder remix) merges upstream first. The local module is deleted after the merge releases. |
| PCM codec | `rusty-codecs/src/codec/pcm/` | 559 | upstream-ours | `audio/pcm.md` | `Codec::Pcm` in moq-audio plus the hang catalog variant, same branch. Supersedes the earlier keep-local verdict in cut-plan; the pcm leaf is marked required in `0-overview.md`. |
| Audio resampler | `rusty-codecs/src/processing/resample.rs` | 123 | adopt-theirs | `counterpart/codec-remove.md` | Converges on `moq_audio::Resampler` (leaner, preallocated scratch; audio.md sec 2). Our channel remix helper is ported via `audio/opus-improvements.md`, not lost. |
| Scale and colorspace convert | `rusty-codecs/src/processing/scale.rs`, `convert.rs` | 958 | keep | n/a | Serve capture and render locally; moq leaves scaling to the caller and already uses the `yuv` crate. Shrinks as its consumers migrate; not an upstream candidate. |
| wgpu renderer | `rusty-codecs/src/render.rs` | 799 | upstream-ours | `render/moq-video-render.md` | Becomes the render crate's wgpu backend over the public `Native` handles. |
| Vulkan DMA-BUF import | `rusty-codecs/src/render/dmabuf_import.rs` | 1,452 | upstream-ours | `render/moq-video-render.md` | Includes the VppRetiler Y_TILED to CCS re-tile; the only decode-to-render GPU path on either side. |
| GLES renderer and EGLImage import | `rusty-codecs/src/render/gles.rs`, `gles_dmabuf.rs` | 938 | upstream-ours | `render/moq-video-render.md` | The GLES backend ships behind a feature flag beside wgpu in the render crate. |
| Metal CVPixelBuffer import | `rusty-codecs/src/render/metal_import.rs` | 274 | upstream-ours | `render/moq-video-render.md` | Depends on `codec/vtb-mf-decode-surface.md` for its decode-side input. |
| Test sources and conformance harness | `rusty-codecs/src/test_sources.rs`, `codec/test_util.rs`, `codec/tests/` | 2,880 | keep | n/a | Gates every cut (cut-plan P1). Shrinks as adopted backends fall under moq's own tests. |
| Crate root | `rusty-codecs/src/lib.rs` | 8 | keep | n/a | Module declarations; shrinks with the crate. |

## rusty-capture (5,507 LOC)

| Module | Path | LOC | Disposition | Where the work lives | Notes |
|---|---|---:|---|---|---|
| Facades, cascade, and types | `rusty-capture/src/lib.rs`, `types.rs` | 1,107 | keep | `counterpart/capture-remove.md` shrinks it | The `CameraCapturer`/`ScreenCapturer` cascades and the `CaptureBackend` enum lose their Apple and Windows arms as those backends are adopted (about 250 LOC). |
| PipeWire screen and camera | `rusty-capture/src/platform/linux/pipewire.rs` | 1,655 | upstream-ours | `capture/pipewire-dmabuf.md` | The DMA-BUF zero-copy delivery upstreams into moq's CPU-only backend; the leaf also carries the portal camera source as an in-scope sibling. Three moq behaviors (token replay, static-screen re-pacing, open-per-demand) are ported into our kept backend meanwhile (cut-plan row). |
| V4L2 camera | `rusty-capture/src/platform/linux/v4l2.rs` | 552 | upstream-ours | `capture/v4l2-camera-enum.md` | Enumeration and format negotiation fill moq's macOS-only `cameras()`; adopts moq's zune-jpeg MJPEG shortcut in the port. |
| libcamera H.264 pre-encoded | `rusty-capture/src/platform/linux/libcamera_h264.rs` | 522 | upstream-ours | `capture/libcamera-preencoded.md` | The only pre-encoded source on either side; required outcome per `0-overview.md`. Open question: the `publish_preencoded` shape, discussed in the leaf; current proposal mirrors `publish_capture` minus `encode::Options`. |
| libcamera raw | `rusty-capture/src/platform/linux/libcamera.rs` | 268 | upstream-ours | `capture/libcamera-preencoded.md` | Travels as the raw companion of the pre-encoded leaf. |
| X11 MIT-SHM screen | `rusty-capture/src/platform/linux/x11.rs` | 373 | keep | n/a | Covers portal-less and X11-only Linux, where moq's story is `Error::Unsupported` (capture.md sec 2). Not an upstream candidate. |
| macOS screen (ScreenCaptureKit) | `rusty-capture/src/platform/apple/screen.rs` | 394 | adopt-theirs | `counterpart/capture-remove.md` | Theirs wins narrowly: app capture, NV12 surfaces, fail-fast TCC (capture.md sec 2). Ours is strictly a subset plus BGRA; nothing to port, recorded in `capture/parity-ports.md`. |
| macOS camera (AVFoundation) | `rusty-capture/src/platform/apple/camera.rs` | 81 | adopt-theirs | `counterpart/capture-remove.md` | Ours is a non-functional stub; theirs is a complete zero-copy backend with TCC handling. Nothing to port. |
| Windows camera and screen stubs | `rusty-capture/src/platform/windows/mod.rs` (+ `capture_fallback` cfg) | ~100 | adopt-theirs | `counterpart/capture-remove.md` | Documentation-only stubs; moq's Media Foundation and Desktop Duplication backends replace them outright. Nothing to port. |
| Android capture stub | `rusty-capture/src/platform/android/` | ~34 | keep | n/a | The MediaProjection plus Camera2 plan; moq has no Android capture surface. Stays as the plan of record until the Android codec placement (B4) settles and a capture leaf is worth writing. |
| nokhwa fallback | `rusty-capture/src/platform/nokhwa_impl.rs` | 246 | drop | `counterpart/capture-remove.md` | Deleted once the adopted macOS camera and Windows backends are proven; its only role today is standing in for those. Acceptable because Linux cameras are covered by V4L2, PipeWire, and libcamera. |
| xcap fallback | `rusty-capture/src/platform/xcap_impl.rs` | 175 | drop | `counterpart/capture-remove.md` | Deleted on the same gate; X11 remains the portal-less Linux screen fallback and the adopted backends cover macOS and Windows. |

## moq-media (11,441 LOC)

The pubsub, adaptive, and sync rows below are owned by the sibling
`plans/align-to-moq/` campaign; they appear here because they share the crate
with the audio device layer and completeness is the point of this register.

| Module | Path | LOC | Disposition | Where the work lives | Notes |
|---|---|---:|---|---|---|
| Audio engine (duplex, mixing, AEC) | `moq-media/src/audio_backend.rs`, `audio_backend/aec.rs` | 2,837 | upstream-ours | `audio/audio-device-unify.md` | Playback sink, mixing, declicker fades, metering, device switching, recovery, and sonora AEC have zero moq counterpart (audio.md sec 3.4). Unifies into moq-audio behind features. |
| Audio file sources (symphonia) | `moq-media/src/audio_file_source.rs`, `audio_file_symphonia.rs` | 472 | keep | n/a | Decoded-PCM sources and moq-mux container importers are complementary (audio.md sec 4). Open question: whether the symphonia source folds into the unified moq-audio device layer or stays local, discussed in `audio/audio-device-unify.md`; current proposal: keep local. |
| MJPEG helper processing | `moq-media/src/processing.rs`, `processing/mjpg.rs` | 87 | keep | n/a | Small local helper serving capture paths. |
| Publish layer | `moq-media/src/publish.rs`, `publish/controller.rs` | 1,830 | keep | align-to-moq | Simulcast registry, `SharedVideoSource`, and leasing have no moq equivalent; the per-track encode wiring collapses onto `encode::Producer` per the cut-plan merge row. |
| Subscribe layer | `moq-media/src/subscribe.rs` | 1,566 | keep | align-to-moq | Quality selection, decoder hot-swap, and the adaptation driver have no upstream counterpart; internals swap on codec adoption. |
| Encode and decode pipelines | `moq-media/src/pipeline/` | 1,212 | keep | align-to-moq | The encode half collapses onto moq producers; the decode loops stay on our OS threads over the sans-IO decoders. |
| Packet transport seam | `moq-media/src/transport.rs` | 204 | keep | align-to-moq | The sink half is replaced by `encode::Producer`; `MoqPacketSource` stays feeding the decoders. |
| Adaptive bitrate policy | `moq-media/src/adaptive.rs`, `net.rs` | 621 | keep | align-to-moq | The only Rust subscriber-side ABR on either side; upstream-later candidate building on moq-mux `Metrics`. |
| Playout clock and sync | `moq-media/src/sync.rs`, `playout.rs` | 512 | keep | align-to-moq | No Rust playout clock exists upstream; upstream-later candidate next to `container::Consumer`. |
| Stats | `moq-media/src/stats.rs` | 494 | keep | n/a | Does not overlap moq-stats or moq-net session stats. |
| Source spec parsing | `moq-media/src/source_spec.rs` | 499 | keep | n/a | CLI parsing, ours alone. |
| Frame channel | `moq-media/src/frame_channel.rs` | 299 | keep | n/a | Latest-wins channel enabling decoder hot-swap; no counterpart. |
| Chat | `moq-media/src/chat.rs` | 182 | keep | n/a | No chat exists anywhere upstream; deliberately not upstreamed. |
| Catalog extension | `moq-media/src/catalog.rs` | 75 | keep | n/a | `IrohLiveExt` uses the sanctioned `CatalogExt` mechanism; already the floor. |
| Crate root, helpers, test fixtures | `moq-media/src/lib.rs`, `util.rs`, `capture.rs`, `test_util.rs` | 551 | keep | n/a | Re-exports, small helpers, and test fixtures; shrink with their consumers. |

## Support crates

| Module | Path | LOC | Disposition | Where the work lives | Notes |
|---|---|---:|---|---|---|
| Egui rendering and debug overlay | `moq-media-egui/src/` | 1,488 | upstream-ours | `render/moq-egui.md` | Moves to moq as `moq-egui` over `moq-video-render`; the leaf decides how much of the 1,080-line stats overlay travels versus stays with our stats types. |
| Dioxus renderer hook | `moq-media-dioxus/src/` | 208 | keep | n/a | Kept for now; too small and too coupled to our API to upstream before the render crate exists. Revisit after `render/moq-video-render.md` lands. |
| Android integration (JNI camera, EGL render) | `moq-media-android/src/` | 955 | keep | n/a | Judgment call: this is iroh-live's Android app glue, not a codec or capture backend. Its codec dependency travels via `codec/android-mediacodec.md` and its render half via the GLES backend of `render/moq-video-render.md`; the JNI and EGL wiring stays ours. |
| GPU debug harness | `gpu-debug/` | 0 | drop | n/a | Already absent from the working tree and the workspace members (verified against `Cargo.toml`); its DMA-BUF modifier findings live on in `rusty-codecs/src/render/dmabuf_import.rs` and travel with the render leaf. Acceptable because the render leaf's host-validation scripts replace its role. |

## Totals

54 rows: 18 upstream-ours, 9 adopt-theirs, 23 keep, 1 defer (AV1), 3 drop
(nokhwa, xcap, gpu-debug). Every adopt-theirs row's port-or-park record is in
`capture/parity-ports.md`; every upstream-ours row links its leaf plan; every
drop states why the deletion loses nothing we need.
