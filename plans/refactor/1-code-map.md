# Code Map: What iroh-live Owns

This document is the full inventory of media code owned by the iroh-live workspace, measured
against the goal of cutting most of it by aligning to the moq stack (moq-net, moq-mux, hang,
moq-native). LOC figures are `wc -l` over `src/**/*.rs` per crate, taken 2026-07-18. Detailed
per-module evidence lives in the four map files under `plans/refactor/maps/`:
[rusty-codecs.md](maps/rusty-codecs.md), [rusty-capture.md](maps/rusty-capture.md),
[moq-media.md](maps/moq-media.md), and [room-layer.md](maps/room-layer.md).

## 1. Workspace Overview

| Crate | LOC | Purpose | Sibling deps | moq deps |
|---|---:|---|---|---|
| rusty-codecs | 22,310 | Native encode/decode implementations, codec traits, frame data model, GPU render, and processing. No transport. | none | `hang` (optional, catalog interop only) |
| rusty-capture | 5,507 | Camera and screen capture backends producing `VideoFrame`s (one backend yields pre-encoded H.264). | rusty-codecs | none |
| moq-media | 11,441 | Transport-agnostic publish, subscribe, adaptive, and A/V sync layer; audio device I/O. | rusty-codecs, rusty-capture (optional) | hang, moq-lite (= moq-net), moq-mux |
| iroh-moq | 572 | MoQ-over-iroh transport adapter plus per-node session manager (dedup, fan-out, `ProtocolHandler`). | none | moq-lite, web-transport-iroh |
| iroh-live | 1,734 | Top-level API: `Live`, rooms over gossip + KV, calls, subscriptions, tickets. | moq-media, iroh-moq | hang (re-export), moq-lite |
| moq-media-android | 955 | Android integration: JNI camera `VideoSource`, EGL HardwareBuffer rendering (`src/lib.rs:1-10`). | moq-media | via moq-media |
| moq-media-egui | 1,488 | Egui video rendering (`FrameView`, `VideoTrackView`) plus a 1,080-line debug stats overlay. | moq-media | via moq-media |
| moq-media-dioxus | 208 | Dioxus-native video renderer hook. Not inventoried further. | moq-media | via moq-media |
| iroh-live-cli | 4,549 | CLI application: publish, play, call, room, record, import, egui UI. | iroh-live, moq-media, moq-media-egui, iroh-live-relay | hang, moq-lite, moq-mux |
| iroh-live-relay | 498 | Relay server bridging iroh P2P and browser WebTransport; built directly on `moq-relay` and `moq-native` (`src/lib.rs:17`). | iroh-live, iroh-moq | moq-relay, moq-native, moq-lite |
| demos/ | 3,734 | Example apps (android, headless, opengl, pi-zero, pi-zero-minimal). Not inventoried further. | various | various |

The five core crates (rusty-codecs, rusty-capture, moq-media, iroh-moq, and iroh-live) total
**41,564 LOC**. Support crates add 7,698 LOC, and demos add 3,734 LOC, for roughly 53,000 LOC of
workspace code overall. The dependency direction is strictly layered: rusty-codecs at the bottom
with no siblings, rusty-capture and moq-media above it, and iroh-live composing moq-media with
iroh-moq at the top. Only moq-media, iroh-moq, and iroh-live touch moq crates; the two rusty-*
crates are transport-free by design. Note that `moq-lite` is a workspace alias for `moq-net`
(root `Cargo.toml:55`), the same crate moq-native is built on (see maps/room-layer.md section 7).

## 2. Per-Crate Inventory

Categories: codec-impl, codec-trait, capture-backend, gpu-zerocopy, processing, pubsub-glue,
adaptive, sync, audio-device, catalog, transport, room, ticket, stats, chat, util, test.

### rusty-codecs (22,310 LOC)

Detail: [maps/rusty-codecs.md](maps/rusty-codecs.md).

| Module | LOC | Purpose | Category |
|---|---:|---|---|
| traits.rs | 410 | Push/pop encoder, decoder, source, and sink traits (`VideoEncoder`, `AudioDecoder`, `VideoSource`, `AudioSink`, ...) | codec-trait |
| format.rs | 1,292 | Frame data model: `VideoFrame`/`FrameData` (Packed, I420, NV12, Gpu), `EncodedFrame`, `MediaPacket`, `NativeFrameHandle`, encoder configs, presets | codec-trait |
| codec.rs | 287 | Runtime backend enums (`codec::VideoCodec`, `codec::AudioCodec`), `best_available()`, encoder dispatch | codec-trait |
| codec/dynamic.rs | 235 | `DynamicVideoDecoder`/`DynamicAudioDecoder` with HW probe cascade (VAAPI, V4L2, VTB, Android, then software) | codec-trait |
| config.rs | 318 | Catalog config types (`VideoConfig`, `AudioConfig`, `H264`, `AV1`) mirroring hang, plus hang interop `From` impls | catalog |
| codec/h264/ (5 files) | 1,897 | Software H.264 via openh264, plus shared Annex B/avcC tooling (annexb.rs, 364) and an SPS VUI patcher (sps.rs, 586, currently dead code) | codec-impl |
| codec/av1/ (4 files) | 936 | Software AV1: rav1e encode, rav1d decode via safe wrapper | codec-impl |
| codec/opus/ (3 files) | 804 | Opus via unsafe-libopus, 48 kHz internal | codec-impl |
| codec/pcm/ (3 files) | 559 | Raw f32 PCM passthrough codec | codec-impl |
| codec/vaapi/ (4 files) | 3,257 | Linux HW H.264 via cros-codecs, DMA-BUF output; `VppScaler` (529) uses raw libva FFI | codec-impl |
| codec/v4l2/ (3 files) | 1,856 | Linux ARM SoC H.264: encoder via raw libc ioctls, decoder via v4l2r | codec-impl |
| codec/vtb/ (3 files) | 1,494 | macOS VideoToolbox H.264, CVPixelBuffer GPU frames | codec-impl |
| codec/android/ (6 files) | 1,528 | Android MediaCodec: encoder plus two decoders (ByteBuffer and zero-copy HardwareBuffer) | codec-impl |
| processing/ (4 files) | 1,086 | Scaling via pic-scale (scale.rs, 360), colorspace via yuv (convert.rs, 598), audio resample via rubato (resample.rs, 123) | processing |
| render.rs | 799 | `WgpuVideoRenderer`: renders any `FrameData` variant to a wgpu texture, per-frame path selection with zero-copy fallback counters | gpu-zerocopy |
| render/dmabuf_import.rs | 1,452 | Zero-copy DMA-BUF to wgpu via raw Vulkan (ash), including VAAPI VPP re-tile for incompatible modifiers | gpu-zerocopy |
| render/gles.rs | 536 | GLES2 renderer with NV12 fragment-shader convert | gpu-zerocopy |
| render/gles_dmabuf.rs | 402 | Zero-copy DMA-BUF to EGL/GLES via EGLImage | gpu-zerocopy |
| render/metal_import.rs | 274 | Zero-copy CVPixelBuffer to wgpu via CVMetalTextureCache | gpu-zerocopy |
| test_sources.rs, codec/test_util.rs, codec/tests/ (5 files) | 2,880 | SMPTE-bar test sources, conformance harness (1,284), vectors, latency, and metrics tests | test |
| lib.rs | 8 | Module declarations, no re-export flattening | util |

### rusty-capture (5,507 LOC)

Detail: [maps/rusty-capture.md](maps/rusty-capture.md). The whole crate is capture; every backend
implements `rusty_codecs::traits::VideoSource` (pull-based `pop_frame`).

| Module | LOC | Purpose | Category |
|---|---:|---|---|
| lib.rs | 713 | `CameraCapturer`/`ScreenCapturer` facades, backend selection cascades, PipeWire runtime detection | capture-backend |
| types.rs | 394 | `CaptureBackend`, `MonitorInfo`, `CameraInfo`, `CameraSelector`, `CapturePixelFormat` with V4L2 FourCC mapping | capture-backend |
| platform/linux/pipewire.rs | 1,655 | PipeWire screen and camera capture via XDG portal, NV12 DMA-BUF zero-copy path | capture-backend |
| platform/linux/v4l2.rs | 552 | V4L2 MMAP camera capture via v4l2r | capture-backend |
| platform/linux/libcamera_h264.rs | 522 | `rpicam-vid --codec h264` subprocess; the only `PreEncodedVideoSource` (parses Annex B into `EncodedFrame`s plus avcC config) | capture-backend |
| platform/apple/screen.rs | 394 | ScreenCaptureKit screen and window capture, zero-copy CVPixelBuffer | capture-backend |
| platform/linux/x11.rs | 373 | X11 MIT-SHM screen capture, CPU only | capture-backend |
| platform/linux/libcamera.rs | 268 | `rpicam-vid --codec yuv420` raw I420 pipe capture | capture-backend |
| platform/nokhwa_impl.rs | 246 | Cross-platform camera via nokhwa, CPU RGBA | capture-backend |
| platform/xcap_impl.rs | 175 | Cross-platform screenshot capture via xcap with sleep-based fps limit | capture-backend |
| platform/apple/camera.rs | 81 | AVFoundation camera, non-functional stub | capture-backend |
| platform mod files, windows and android stubs | 134 | cfg gating plus documentation-only Windows (WGC/DXGI) and Android (MediaProjection) plans | capture-backend |

### moq-media (11,441 LOC)

Detail: [maps/moq-media.md](maps/moq-media.md). All container framing and catalog production is
delegated to moq-mux and hang; the map found no duplication of hang container or moq-lite
track/group logic.

| Module | LOC | Purpose | Category |
|---|---:|---|---|
| subscribe.rs | 1,566 | `RemoteBroadcast`, `VideoTrack`/`AudioTrack`, rendition selection by quality, adaptation task with seamless decoder swap | pubsub-glue |
| publish.rs | 1,508 | `LocalBroadcast` with lazy on-demand track start, simulcast rendition registries, `SharedVideoSource` fan-out with camera parking | pubsub-glue |
| pipeline/ (5 files) | 1,212 | OS-thread encode/decode loops bridging sync codecs to async transport; keyframe gating, `FramePacer`, audio silence insertion | pubsub-glue |
| publish/controller.rs | 322 | `PublishCaptureController`: diffs capture opts and wires rusty-capture into `LocalBroadcast` | pubsub-glue |
| audio_backend.rs | 2,445 | cpal device I/O: `AudioDriver` thread, stream negotiation, mixing, fades, device switching, error recovery; largest single file in the workspace | audio-device |
| audio_backend/aec.rs | 392 | Echo cancellation via sonora with lock-free render-reference ring buffer | audio-device |
| adaptive.rs | 592 | Pure ABR decision logic: emergency/downgrade/probe state machine with bandwidth-primary thresholds | adaptive |
| net.rs | 29 | `NetworkSignals` POD fed from QUIC stats into adaptive.rs | adaptive |
| sync.rs | 420 | Playout clock, a direct port of moq-js `sync.ts`; no Rust hang/moq equivalent exists to wrap | sync |
| playout.rs | 92 | `SyncMode` and `PlaybackPolicy` (max_latency maps to the moq-mux ordered-consumer group-skip threshold) | sync |
| audio_file_source.rs + audio_file_symphonia.rs | 472 | WAV/MP3/FLAC import via symphonia plus rubato resample to 48 kHz | processing |
| processing.rs + processing/mjpg.rs | 87 | MJPEG helper processing | processing |
| stats.rs | 494 | Typed atomic metrics (EMA plus history) for debug overlays | stats |
| source_spec.rs | 499 | CLI source-string parsing (`VideoSourceSpec`, `AudioSourceSpec`) | util |
| frame_channel.rs | 299 | Latest-wins single-slot channel enabling decoder swaps under a stable consumer | util |
| transport.rs | 204 | `PacketSource`/`PacketSink` traits; `MoqPacketSink`/`MoqPacketSource` wrap moq-mux container producer/consumer. The intended refactor seam | transport |
| chat.rs | 182 | Text chat over a moq-lite track, one group per message | chat |
| catalog.rs | 75 | `IrohLiveExt` (chat, user) extending the hang catalog via the sanctioned `CatalogExt` mechanism | catalog |
| lib.rs, util.rs, capture.rs | 103 | Re-exports and small helpers | util |
| test_util.rs | 448 | Test fixtures | test |

### iroh-moq (572 LOC)

Detail: [maps/room-layer.md](maps/room-layer.md) section 1. A single lib.rs. The `MoqSession`
handshake code (roughly 120 lines) duplicates `moq-native/src/iroh.rs` over the identical
underlying `moq-net` crate, including the hardcoded `b"moq-lite-04"` ALPN
(`iroh-moq/src/lib.rs:35`). The `Actor` (dedup, node-wide fan-out, iroh `Router`
`ProtocolHandler`, roughly 200 lines) has no moq-native equivalent. The remaining roughly 250
lines are the public `Moq` handle, the incoming-session plumbing around the actor, and error
types.

| Module | LOC | Purpose | Category |
|---|---:|---|---|
| lib.rs | 572 | `Moq` handle, `MoqSession` handshake over web-transport-iroh, `MoqProtocolHandler`, session-manager actor | transport |

### iroh-live (1,734 LOC)

Detail: [maps/room-layer.md](maps/room-layer.md) sections 2 to 6.

| Module | LOC | Purpose | Category |
|---|---:|---|---|
| rooms.rs | 695 | Room actor: gossip topic plus iroh-smol-kv signed peer state for announce/discover/membership, auto MoQ dial on discovery; includes `RoomTicket` | room |
| live.rs | 300 | `Live`/`LiveBuilder`: composes Endpoint, Moq, optional Gossip, and Router; publish/subscribe/join_room entry points | room |
| call.rs | 158 | 1:1 `Call` sugar over fixed broadcast name "call"; self-described as pure sugar (`call.rs:23-37`) | room |
| subscription.rs | 87 | `Subscription` bundle of session, broadcast, and network signals | room |
| rooms/publisher.rs | 72 | Bridges `PublishCaptureController` producers into a `RoomHandle` | room |
| ticket.rs | 186 | `LiveTicket`: point-to-point broadcast ticket embedding a postcard-encoded `EndpointAddr` | ticket |
| util.rs | 185 | `spawn_signal_producer` (QUIC path stats to `NetworkSignals`) and `spawn_stats_recorder` | stats |
| lib.rs, types.rs | 51 | Re-exports and `DisconnectReason` | util |

## 3. Ownership Totals

LOC by category across the five core crates. This is the denominator the cut plan works against.

| Category | LOC | Share | Where it lives |
|---|---:|---:|---|
| codec-impl | 12,331 | 29.7% | rusty-codecs backends (vaapi 3,257; h264 1,897; v4l2 1,856; android 1,528; vtb 1,494; av1 936; opus 804; pcm 559) |
| capture-backend | 5,507 | 13.2% | all of rusty-capture |
| pubsub-glue | 4,608 | 11.1% | moq-media publish, subscribe, pipelines, controller |
| gpu-zerocopy | 3,463 | 8.3% | rusty-codecs render/ (wgpu, Vulkan DMA-BUF, GLES, Metal) |
| test | 3,328 | 8.0% | rusty-codecs conformance harness and vectors; moq-media test_util |
| audio-device | 2,837 | 6.8% | moq-media audio_backend plus AEC |
| codec-trait | 2,224 | 5.4% | rusty-codecs traits, format, dispatch |
| processing | 1,645 | 4.0% | rusty-codecs scale/convert/resample; moq-media file import and mjpg |
| room | 1,312 | 3.2% | iroh-live rooms, live, call, subscription |
| util | 960 | 2.3% | frame_channel, source_spec, small helpers |
| transport | 776 | 1.9% | iroh-moq (572) plus moq-media transport.rs (204) |
| stats | 679 | 1.6% | moq-media stats.rs; iroh-live util.rs |
| adaptive | 621 | 1.5% | moq-media adaptive.rs plus net.rs |
| sync | 512 | 1.2% | moq-media sync.rs plus playout.rs |
| catalog | 393 | 0.9% | rusty-codecs config.rs; moq-media catalog.rs |
| ticket | 186 | 0.4% | iroh-live ticket.rs |
| chat | 182 | 0.4% | moq-media chat.rs |
| **Total** | **41,564** | 100% | |

Two observations frame the cut plan. First, codec-impl, capture-backend, and gpu-zerocopy
together are 21,301 LOC, just over half of everything owned. Current moq main carries the full
native stack: moq-video has full encode and decode with hardware backends, and moq has six
native capture backends, an internal GPU frame union, and no ffmpeg (see 2-moq-inventory.md and
maps/moq-video.md). The codec-impl and capture-backend halves therefore overlap heavily with
moq. What moq main still lacks is VAAPI decode, V4L2 and Android codec backends, a decode-to-render
zero-copy path, a GPU renderer, and DMA-BUF or Metal import (maps/rusty-codecs.md section 3),
so the genuinely unmatched area is gpu-zerocopy render and import. Second, the glue layers that a
moq alignment can actually shrink (pubsub-glue, transport, sync, catalog, chat) total about
6,500 LOC, and the maps already
identify the seams: `transport.rs` as the container boundary, `iroh-moq`'s handshake as a
moq-native duplicate, and `sync.rs` as a moq-js port whose dead `audio_ms`/`video_ms` fields flag
a missing per-codec jitter field in the Rust hang catalog (maps/moq-media.md, summary).

## 4. External Dependency Inventory

Notable native and heavyweight dependencies, and the module that pulls each in. Feature flags per
maps/rusty-codecs.md section 5 and maps/rusty-capture.md section 4.

| Dependency | Pulled in by | Notes |
|---|---|---|
| openh264 0.9 (`source`) | rusty-codecs codec/h264/ | Builds the Cisco C library from source; `h264` feature (default) |
| rav1e 0.8 | rusty-codecs codec/av1/encoder.rs | Software AV1 encode; `av1` feature |
| rav1d (git, memorysafety fork) | rusty-codecs codec/av1/decoder.rs | Software AV1 decode with asm; git pin, `av1` feature |
| unsafe-libopus 0.2 | rusty-codecs codec/opus/ | Pure-Rust libopus port; `opus` feature (default) |
| cros-codecs 0.0.6 | rusty-codecs codec/vaapi/ | Linux VAAPI H.264; raw libva FFI additionally in vpp_scaler.rs and render/dmabuf_import.rs |
| v4l2r 0.0.7 | rusty-codecs codec/v4l2/decoder.rs; rusty-capture platform/linux/v4l2.rs | Encoder side uses raw libc ioctls instead |
| ndk 0.9 (media) | rusty-codecs codec/android/ | MediaCodec; `android` feature |
| objc2-* 0.3 family | rusty-codecs codec/vtb/ and format.rs (AppleGpuFrame); rusty-capture platform/apple/ | VideoToolbox, CoreMedia, CoreVideo, AVFoundation |
| screencapturekit 1.5 | rusty-capture platform/apple/screen.rs | macOS 12.3+ screen capture |
| pipewire 0.9 + libspa + ashpd + nix | rusty-capture platform/linux/pipewire.rs | XDG portal screen/camera, DMA-BUF path |
| x11rb 0.13 | rusty-capture platform/linux/x11.rs | MIT-SHM capture; non-default `x11` feature |
| nokhwa 0.10 / xcap 0.9 | rusty-capture platform/{nokhwa,xcap}_impl.rs | Cross-platform CPU fallbacks |
| wgpu 27 + wgpu-hal | rusty-codecs render.rs; re-exported through moq-media and moq-media-egui | `wgpu` feature |
| ash 0.38 | rusty-codecs render/dmabuf_import.rs | Raw Vulkan for DMA-BUF import; `dmabuf-import` feature |
| glow 0.16 | rusty-codecs render/gles*.rs; moq-media-android | GLES renderer; `gles` feature |
| metal 0.32 | rusty-codecs render/metal_import.rs | `metal-import` feature |
| pic-scale | rusty-codecs processing/scale.rs | Always-on |
| yuv | rusty-codecs processing/convert.rs | Always-on dep, module gated on `h264`/`av1` |
| rubato 1.0 + audioadapter-buffers | rusty-codecs processing/resample.rs; moq-media audio_file_symphonia.rs | Audio resampling |
| symphonia | moq-media audio_file_symphonia.rs | WAV/MP3/FLAC import, replaced an ffmpeg runtime dep |
| cpal (git pin) | moq-media audio_backend.rs | Device I/O; moq-audio uses the same crate upstream |
| sonora | moq-media audio_backend/aec.rs | Echo cancellation |
| fixed-resample + ringbuf | moq-media audio_backend.rs | Lock-free stream buffering |
| web-transport-iroh | iroh-moq lib.rs | The iroh-to-WebTransport bridge moq-native's iroh module also uses |
| iroh-gossip + iroh-smol-kv | iroh-live rooms.rs | Room discovery and membership state |
| moq-relay + moq-native | iroh-live-relay | Already consumed directly by the relay, bypassed by iroh-moq |
