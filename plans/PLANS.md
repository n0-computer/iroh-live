# Plans

Open plans for future work. Each file has a checklist and implementation
notes. For the current architecture, see [docs/](../docs/).

## API and examples

- [api.md](api.md) — remaining public API work: relay publishing, room
  participant model, mute/enable, source replacement, quality refinement
- [call-example-simplification.md](call-example-simplification.md) —
  demo sources, source discovery, egui helpers, auto-resubscribe
- [docs.md](docs.md) — documentation improvements checklist

## Media pipeline

- [adaptive-encoding.md](adaptive-encoding.md) — sender-side rate
  control driven by QUIC PathStats bandwidth estimation
- [opus-fec.md](opus-fec.md) — Opus in-band FEC, PLC, DTX
- [capture-pipeline.md](capture-pipeline.md) — deferred phase 2:
  VideoProcessor trait, pre-encoded audio, keyframe requests
- [perf.md](perf.md) — remaining per-frame allocation and hot-path
  optimizations

## Rendering and UI

- [rendering.md](rendering.md) — FrameRenderer trait, Android EGL in
  Rust, GLES viewer mode
- [debug-overlays.md](debug-overlays.md) — typed metrics refactor,
  timeline panel redesign, A/V sync measurement
- [devtools.md](devtools.md) — extract common_egui, build multi-endpoint
  dev tool

## Infrastructure

- [relay-browser.md](relay-browser.md) — ACME cert provisioning, shared
  Ticket trait
- [dependencies.md](dependencies.md) — bindgen alignment, image crate
  replacement, crypto unification

## Platforms

- [platform/android.md](platform/android.md) — surface-mode encode
  remaining
- [platform/raspberry-pi.md](platform/raspberry-pi.md) — complete for
  current hardware; stateless V4L2 for other SBCs is future work
- [platform/linux-grd-review.md](platform/linux-grd-review.md) —
  comparative analysis vs GNOME Remote Desktop pipeline
- [platform/linux-decode-render-review.md](platform/linux-decode-render-review.md) —
  comparative analysis vs mpv/libplacebo, GStreamer, Chromium, FFmpeg
- [platform/windows.md](platform/windows.md) — Media Foundation H.264,
  screen/camera capture (design only, no code)

## Reference

- [platforms.md](platforms.md) — codec, capture, and rendering support
  matrix
- [future.md](future.md) — feature gap analysis: recording, data
  channels, noise suppression, HLS egress, E2EE, virtual backgrounds
