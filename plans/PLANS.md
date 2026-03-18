# Plans

## Open

### Public API
Relay publishing, room participant model, mute/enable, source replacement, quality refinement.
- [api.md](api.md)

### Adaptive encoding
Sender-side rate control driven by QUIC `PathStats` bandwidth estimation. Quality state machine for dynamic bitrate and resolution adjustment.
- [adaptive-encoding.md](adaptive-encoding.md)

### Opus FEC and PLC
Opus in-band FEC, packet loss concealment, DTX bandwidth savings. Depends on adaptive encoding for loss rate feedback.
- [opus-fec.md](opus-fec.md)

### Audio refactor
Replace firewheel audio graph with direct cpal + fixed-resample. Inline AEC, peak meters, simpler threading.
- [audio-refactor.md](audio-refactor.md)

### Capture pipeline
Unify raw and pre-encoded video source paths, add backpressure signaling, consider pipeline builder pattern.
- [capture-pipeline.md](capture-pipeline.md)

### Rendering
Unified `FrameRenderer` trait across wgpu, GLES2, and Android EGL. NV12 shader for Pi GLES2 path.
- [rendering.md](rendering.md)

### Relay and browser
`iroh-live-relay` binary bridging iroh P2P with WebTransport browser clients. ACME certs, Playwright E2E tests.
- [relay-browser.md](relay-browser.md)

### Network simulation testing
Patchbay integration for kernel-level link impairment. NAT traversal and relay fallback testing.
- [netsim-testing.md](netsim-testing.md)

### Call example simplification
Extract reusable demo sources, source discovery, and egui helpers from examples.
- [call-example-simplification.md](call-example-simplification.md)

### Debug overlays
Consolidated stats tracking (`MetricsCollector` in moq-media) and overlay UI (collapsible stat bars, timeline panel) in moq-media-egui. Replaces ad-hoc per-example stats code.
- [debug-overlays.md](debug-overlays.md)

### Devtools
Extract shared egui module from examples, build multi-endpoint splitscreen dev tool.
- [devtools.md](devtools.md)

### PipeWire drop timeout
Timed thread join in PipeWire capturer `Drop` to prevent indefinite hangs.
- [pipewire-drop-timeout.md](pipewire-drop-timeout.md)

## Platforms

### Android
MediaCodec H.264, CameraX capture, zero-copy HardwareBuffer rendering, demo app. Mostly complete — surface-mode encode remaining.
- [platform/android.md](platform/android.md)

### Raspberry Pi
V4L2 stateful H.264, DRM rendering, Pi Zero demo. Complete for current hardware. Stateless V4L2 for other SBCs is future work.
- [platform/raspberry-pi.md](platform/raspberry-pi.md)

### Linux — GNOME Remote Desktop review
Comparative analysis of GRD's capture/encode pipeline against ours. Actionable items: drain-all-keep-latest dequeue, explicit sync, modifier negotiation, damage detection, encoder pacing, resource pooling.
- [platform/linux-grd-review.md](platform/linux-grd-review.md)

### Linux — decode→render pipeline review
Comparative analysis against mpv/libplacebo, GStreamer, Chromium, FFmpeg. Actionable items: texture reuse, AMD size=0 fix, vaSyncSurface placement, command buffer reuse, format probing, dedicated allocations.
- [platform/linux-decode-render-review.md](platform/linux-decode-render-review.md)

### Windows
Media Foundation H.264, screen/camera capture. Design complete, no code yet.
- [platform/windows.md](platform/windows.md)

## Reference

### Platform support matrix
Codec, capture, and rendering support across all platforms and hardware.
- [platforms.md](platforms.md)

### Future features
Feature gap analysis: recording, data channels, noise suppression, HLS egress, E2EE, virtual backgrounds, stats API, SVC.
- [future.md](future.md)

## Completed

Detailed plans for completed work are in `completed/`. These are kept for historical context.

- `completed/api/` — API redesign phases 1–4, 6–7, 9 (overview, review, research, sketch, impl, examples, relay design)
- `completed/media-pipeline/` — codec swap (phases 1–2), hardware acceleration (phase 2b), transport decoupling, GPU rendering, zero-copy encode, playout clock (phase 3b), adaptive renditions (phase 3a), audio device switching (phase 3e), standalone viewer
- `completed/gpu-rendering/` — VAAPI to wgpu DMA-BUF zero-copy, PipeWire capture zero-copy
