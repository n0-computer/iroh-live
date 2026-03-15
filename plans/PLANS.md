# Plans

Index of all plans, grouped by completion status. Each section links to the
detailed plan document and summarizes what remains.

## Done

### Codec replacement (phases 1–2)

Replaced ffmpeg with pure-Rust codecs: openh264, rav1e/rav1d, unsafe-libopus.
All codecs are feature-gated and routed through `DynamicVideoDecoder`.

- [phase-1-codec-swap](media-pipeline/phase-1-codec-swap.md) — remove ffmpeg, add software codecs
- [phase-2-av1](media-pipeline/phase-2-av1.md) — AV1 encode/decode behind `av1` feature

### Hardware acceleration (phase 2b)

VAAPI (Linux), VideoToolbox (macOS), and V4L2 (RPi) encoder backends, each
behind a feature flag. Decoder side: VAAPI stateless H.264 decoder via
cros-codecs, V4L2 stateful decoder.

- [phase-2b-hw-accel](media-pipeline/phase-2b-hw-accel.md) — overview and architecture
- [phase-2b-linux-vaapi](media-pipeline/phase-2b-linux-vaapi.md) — VAAPI H.264 encoder via cros-codecs
- [phase-2b-macos-videotoolbox](media-pipeline/phase-2b-macos-videotoolbox.md) — VideoToolbox H.264 encoder

### Transport decoupling

`MediaPacket`, `PacketSource`/`PacketSink` traits decouple encode/decode
pipelines from the transport layer.

- [api-decoupling](media-pipeline/api-decoupling.md)

### GPU rendering and zero-copy decode

wgpu rendering with NV12 shader, DMA-BUF import from VAAPI decoder, VPP
retiler for Y-tiled to CCS conversion on Intel.

- [gpu-rendering](media-pipeline/gpu-rendering.md) — wgpu renderer, DMA-BUF import, VPP retiler
- [zerocopy-vaapi-wgpu](gpu-rendering/zerocopy-vaapi-wgpu.md) — VAAPI→Vulkan DMA-BUF deep dive

### Zero-copy capture→encode

PipeWire DMA-BUF negotiation and VAAPI VPP color-space conversion for
end-to-end GPU capture→encode without CPU round-trips.

- [zero-copy-encode](media-pipeline/zero-copy-encode.md)
- [zerocopy-capture](gpu-rendering/zerocopy-capture.md)

### Playout clock and A/V sync (phase 3b)

`PlayoutClock` maps PTS to wall-clock playout times with jitter measurement,
buffer underrun re-anchoring, and `PlayoutBuffer` for smoothing decoder bursts.

- [phase-3b-jitter-sync](media-pipeline/phase-3b-jitter-sync.md)

### Adaptive rendition switching (phase 3a)

`AdaptiveVideoTrack` switches renditions based on `NetworkSignals` (bandwidth,
RTT, loss). Bandwidth-primary selection with asymmetric hold timers.

- [phase-3a-rendition-switching](media-pipeline/phase-3a-rendition-switching.md)

### Standalone viewer example

Egui viewer with source, codec, decoder, and render backend selectors.

- [standalone-viewer-example](media-pipeline/standalone-viewer-example.md)

### API redesign (phases 1–4, 6–7, 9)

Renamed types (`LocalBroadcast`, `RemoteBroadcast`, `CatalogSnapshot`), added
`VideoPublisher`/`AudioPublisher` slot API, subscription options with
`VideoTarget`/`Quality`, domain error types, and `Live` builder in iroh-live.

- [0-overview](api/0-overview.md) — direction and design principles
- [1-review](api/1-review.md) — pre-redesign API review
- [2-research](api/2-research.md) — survey of LiveKit, WebRTC, GStreamer APIs
- [3-sketch](api/3-sketch.md) — target API sketch
- [4-impl](api/4-impl.md) — 9-phase plan with status checklist
- [3a-glossary](api/3a-glossary.md) — glossary of API terms and concepts
- [5-examples](api/5-examples.md) — example patterns against the proposed API
- [5-risks-and-future](api/5-risks-and-future.md) — risks, trade-offs, and future work
- [6-examples](api/6-examples.md) — example code rewrites against proposed API
- [6-relay](api/6-relay.md) — relay server integration design
- [7-relay](api/7-relay.md) — relay support for iroh-live sessions

## Partial

### API redesign — remaining phases

The core API is in place. Several advanced features remain as stubs or
unimplemented.

- [ ] Phase 5: relay support — `LocalBroadcast::relay()`, zero-transcode forwarding
- [ ] Phase 7.4: relay convenience methods on `Live`
- [ ] Phase 8: room participant model — `LocalParticipant`, `RemoteParticipant`, `RoomEvent`
- [ ] `set_enabled()` / `set_muted()` — stubs, need encoder pipeline pause/resume
- [ ] `BroadcastStatus` watcher, `VideoTrack::frames()` stream

### Audio device switching (phase 3e)

Runtime audio input/output device switching is partially implemented.
`SwitchDevice` message and `AudioDriverOpts` exist; hot-swap on disconnect
and auto-fallback to default device are missing.

- [phase-3e-audio-device-switching](media-pipeline/phase-3e-audio-device-switching.md)
- [ ] Hot-swap on device disconnect
- [ ] Auto-fallback to default device

### Windows Media Foundation (phase 2b)

Design document and implementation plan exist. No code has been written.
Crate research complete: `windows` (MIT), `windows-capture` (MIT),
`nokhwa` (Apache-2.0).

- [phase-2b-windows-media-foundation](media-pipeline/phase-2b-windows-media-foundation.md) — MFT encoder design
- [phase-2b-windows-impl](media-pipeline/phase-2b-windows-impl.md) — full implementation plan with crate selection
- [ ] Screen capture via `windows-capture`
- [ ] Camera capture via `nokhwa`
- [ ] MFT H.264 encoder
- [ ] MFT H.264 decoder

## Open

### Opus FEC and PLC (phase 3c)

Opus in-band FEC, packet loss concealment with comfort noise, and DTX
bandwidth savings.

- [phase-3c-fec](media-pipeline/phase-3c-fec.md)

### Adaptive encoding (phase 3d)

Encoder rate control driven by QUIC `PathStats` bandwidth estimation. Quality
state machine for dynamic bitrate adjustment.

- [phase-3d-adaptive-encoding](media-pipeline/phase-3d-adaptive-encoding.md)

### Android MediaCodec

NDK `AMediaCodec` H.264 encode and decode, Camera2 capture via JNI, and
an Android demo app skeleton.

- [android-mediacodec](media-pipeline/android-mediacodec.md) — codec integration plan
- [android-demo-app](media-pipeline/android-demo-app.md) — Kotlin/JNI demo app architecture
- [ ] ByteBuffer encoder and decoder
- [ ] Camera2 capture via JNI
- [ ] Surface mode for zero-copy

### Stateless V4L2 decoder

Stateless V4L2 decoder for Rockchip, Allwinner, and MediaTek SBCs. The
stateful V4L2 encoder and decoder already exist in `rusty-codecs/src/codec/v4l2/`.

- [v4l2-future](media-pipeline/v4l2-future.md)

### Devtools

Extract shared `common_egui` module from examples, build multi-endpoint
splitscreen dev example with debug overlay.

- [devtools](devtools/00-main.md)

### Network simulation testing

Patchbay integration for realistic link impairment, NAT traversal, and
relay fallback testing. Synthetic signal injection tests exist; real
kernel-level impairment does not.

- [netsim-testing](media-pipeline/netsim-testing.md) — patchbay plan and integration sketch
- [ ] Patchbay lab setup with publisher/subscriber namespaces
- [ ] Dynamic link degradation mid-session
- [ ] NAT traversal and relay fallback tests

### Future features

Feature gap analysis covering recording, data channels, noise suppression,
HLS egress, relay/SFU, E2EE, virtual backgrounds, stats API, SVC, and
multi-codec negotiation.

- [future](future.md) — prioritized feature list with effort estimates

## Reference

Architecture overviews and context documents, not actionable work items.

- [platforms](platforms.md) — platform support matrix for codecs, capture, and rendering
- [media-pipeline master](media-pipeline/00-master-plan.md) — full phase overview
- [media-pipeline quick ref](media-pipeline/00-main.md) — phase table with links
- [phase-3-av-resilience](media-pipeline/phase-3-av-resilience.md) — phases 3a–3d overview
