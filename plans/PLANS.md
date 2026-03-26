# Plans

Open plans for future work. Each file has a checklist and implementation
notes. For the current architecture, see [docs/](../docs/).

## API and examples

- [api.md](api.md) — remaining public API work: relay publishing, room
  participant model, mute/enable, source replacement, quality refinement
- [api-ergonomics.md](api-ergonomics.md) — two remaining ergonomic
  improvements (prelude module, subscribe_with_stats)
- [call-example-simplification.md](call-example-simplification.md) —
  library-level improvements: demo sources, source discovery, auto-resubscribe
- [cli.md](cli.md) — CLI is implemented; remaining: preview mode, grid layout,
  shared control widgets
- [docs.md](docs.md) — documentation improvements checklist

## Media pipeline

- [adaptive-encoding.md](adaptive-encoding.md) — sender-side rate
  control driven by QUIC PathStats bandwidth estimation (step 1 partial)
- [opus-fec.md](opus-fec.md) — Opus in-band FEC, PLC, DTX
- [capture-pipeline.md](capture-pipeline.md) — deferred phase 2:
  VideoProcessor trait, pre-encoded audio, keyframe requests
- [perf.md](perf.md) — remaining per-frame allocation and hot-path
  optimizations
- [av-sync.md](av-sync.md) — research notes on A/V sync (disabled;
  unsynchronized PTS pacing works well enough)
- [codesize.md](codesize.md) — remaining deduplication opportunities

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
- [platform/raspberry-pi.md](platform/raspberry-pi.md) — stateless V4L2
  decoder for other SBCs is future work
- [platform/windows.md](platform/windows.md) — Media Foundation H.264,
  screen/camera capture (design only, no code)

## Reference

- [platforms.md](platforms.md) — codec, capture, and rendering support
  matrix
- [future.md](future.md) — feature gap analysis: recording, data
  channels, noise suppression, HLS egress, E2EE, virtual backgrounds
- [platform/linux-grd-review.md](platform/linux-grd-review.md) —
  comparative analysis vs GNOME Remote Desktop pipeline
- [platform/linux-decode-render-review.md](platform/linux-decode-render-review.md) —
  comparative analysis vs mpv/libplacebo, GStreamer, Chromium, FFmpeg
