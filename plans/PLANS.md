# Plans Overview

Index of all plans with implementation status.

## Complete

Plans where all work is done.

### Media Pipeline — Track A: Codec Replacement

| Plan | Description |
|------|-------------|
| [phase-1-codec-swap](media-pipeline/phase-1-codec-swap.md) | Remove ffmpeg, add openh264 + unsafe-libopus + yuvutils + pic-scale + rubato. 72 tests. |
| [phase-2-av1](media-pipeline/phase-2-av1.md) | AV1 encode (rav1e) + decode (rav1d) behind `av1` feature. DynamicVideoDecoder auto-routing. 84 tests. |
| [phase-2b-hw-accel](media-pipeline/phase-2b-hw-accel.md) | HW encoder backends: VAAPI (Linux), VideoToolbox (macOS), V4L2 (RPi). Feature-gated. |
| [phase-2b-linux-vaapi](media-pipeline/phase-2b-linux-vaapi.md) | VAAPI H.264 stateless encoder via cros-codecs. |
| [phase-2b-macos-videotoolbox](media-pipeline/phase-2b-macos-videotoolbox.md) | VideoToolbox H.264 encoder via objc2-video-toolbox. |
| [phase-2b-windows-media-foundation](media-pipeline/phase-2b-windows-media-foundation.md) | Windows MFT H.264 encoder (design only, not yet built). |
| [api-decoupling](media-pipeline/api-decoupling.md) | MediaPacket, PacketSource/PacketSink traits for transport-independent pipelines. |
| [gpu-rendering](media-pipeline/gpu-rendering.md) | wgpu rendering, DMA-BUF zero-copy import, VAAPI VPP retiler for Y-tiled→CCS. |
| [standalone-viewer-example](media-pipeline/standalone-viewer-example.md) | Egui viewer with source/codec/decoder/render backend selectors. |
| [zero-copy-encode](media-pipeline/zero-copy-encode.md) | PipeWire DMA-BUF negotiation (phase 1 done). VAAPI VPP color-space conversion (phase 2 design). |

## Partial

Plans where some work is done but items remain.

### API Redesign

**Summary**: Redesign public API across moq-media and iroh-live for ergonomic, idiomatic Rust. Rename types, add sub-handles, options, domain errors, builder patterns.

**Missing**:
- Phase 5: Relay support — `LocalBroadcast::relay()` zero-transcode relay, `VideoTrack` as `VideoSource` for transcode relay
- Phase 7.4: Relay URL convenience methods on Live (`connect_relay`, `publish_to_relay`, `subscribe_from_relay`)
- Phase 8: Room participant model redesign (LocalParticipant, RemoteParticipant, RoomEvent stream)
- `set_enabled()` / `set_muted()` — stubs exist, need encoder pipeline support

| Plan | Description |
|------|-------------|
| [0-overview](api/0-overview.md) | Direction, naming, design principles, use case matrix |
| [1-review](api/1-review.md) | Detailed review of pre-redesign API with inline comments |
| [2-research](api/2-research.md) | Survey of LiveKit, Hang, WebRTC, GStreamer, OBS APIs |
| [3-sketch](api/3-sketch.md) | Points to `iroh-live/examples/api_sketch.rs` (compiling) |
| [4-impl](api/4-impl.md) | 9-phase implementation plan with detailed status checklist |
| [5-examples](api/5-examples.md) | 16 example patterns against the proposed API |
| [6-relay](api/6-relay.md) | Relay server integration design (moq-relay, SFU, CDN) |

### Media Pipeline — Audio Device Switching

**Summary**: Runtime audio input/output device switching with hot-swap and auto-fallback.

**Missing**: Hot-swap on device disconnect, auto-fallback to default device.

| Plan | Description |
|------|-------------|
| [phase-3e-audio-device-switching](media-pipeline/phase-3e-audio-device-switching.md) | AudioDriverOpts, SwitchDevice message, swappable handle indirection |

## Open

Plans that have not been started yet.

### Media Pipeline — Track B: AV Resilience

| Plan | Description |
|------|-------------|
| [phase-3-av-resilience](media-pipeline/phase-3-av-resilience.md) | Overview of phases 3a–3d, architecture diagram, dependency graph |
| [phase-3a-rendition-switching](media-pipeline/phase-3a-rendition-switching.md) | Adaptive video/audio rendition switching based on network signals. Bandwidth-primary selection, asymmetric timers, seamless decoder handoff. |
| [phase-3b-jitter-sync](media-pipeline/phase-3b-jitter-sync.md) | Frame-level playout timing with PlayoutClock. PlayoutBuffer for scheduling. A/V sync via audio master. |
| [phase-3c-fec](media-pipeline/phase-3c-fec.md) | Opus in-band FEC, PLC comfort noise, DTX bandwidth savings. Future. |
| [phase-3d-adaptive-encoding](media-pipeline/phase-3d-adaptive-encoding.md) | Encoder rate control, bandwidth estimation from QUIC PathStats, quality state machine. Future. |

### GPU, Capture & V4L2

| Plan | Description |
|------|-------------|
| [zerocopy-vaapi-wgpu](gpu-rendering/zerocopy-vaapi-wgpu.md) | Deep dive on Y-tiled NV12 VAAPI→Vulkan import via VK_EXT_image_drm_format_modifier |
| [zerocopy-capture](gpu-rendering/zerocopy-capture.md) | Zero-copy screen/camera capture via PipeWire DMA-BUF, ScreenCaptureKit IOSurface |
| [v4l2-future](media-pipeline/v4l2-future.md) | Stateless V4L2 decoder for Rockchip/Allwinner/MediaTek SBCs. Stateful encoder/decoder already done (`rusty-codecs/src/codec/v4l2/`). |

### Devtools

| Plan | Description |
|------|-------------|
| [devtools](devtools/00-main.md) | Egui examples refactor: extract common_egui module, multi-endpoint splitscreen dev example with rich debug overlay |

### Master Plans (reference)

| Plan | Description |
|------|-------------|
| [media-pipeline master](media-pipeline/00-master-plan.md) | Full context, phase overview, architecture, module structure |
| [media-pipeline quick ref](media-pipeline/00-main.md) | Phase table with status and plan links |
