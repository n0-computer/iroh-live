# Plans

Open plans for future work. Each file has a checklist and implementation
notes. For the current architecture, see [docs/](../docs/).

## API and examples

- [api.md](api.md) — remaining public API work: relay publishing, room
  participant model, mute/enable, source replacement, quality refinement
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
- [adaptive-track-refactor.md](adaptive-track-refactor.md) — refactor
  `AdaptiveVideoTrack` into `VideoTrack::enable_adaptation()`
- [capture-backend-trait.md](capture-backend-trait.md) — unify capture
  backend cfg flags behind a trait
- [data-channels.md](data-channels.md) — data channel support alongside
  media tracks

## Rendering and UI

- [rendering.md](rendering.md) — FrameRenderer trait, Android EGL in
  Rust, GLES viewer mode
- [debug-overlays.md](debug-overlays.md) — typed metrics refactor,
  timeline panel redesign, A/V sync measurement
- [bevy-demo.md](bevy-demo.md) — multiplayer 3D spatial video gallery
  using Bevy

## Infrastructure

- [relay-browser.md](relay-browser.md) — ACME cert provisioning, shared
  Ticket trait
- [dependencies.md](dependencies.md) — bindgen alignment, image crate
  replacement, crypto unification
- [perf-pass.md](perf-pass.md) — full-pass audit of hot paths and
  per-frame allocations
- [rooms-overhaul.md](rooms-overhaul.md) — feature-complete AV rooms
  with text chat

## Platforms

- [platform/android.md](platform/android.md) — surface-mode encode
  remaining
- [platform/raspberry-pi.md](platform/raspberry-pi.md) — stateless V4L2
  decoder for other SBCs is future work
- [pi-zero-refactor.md](pi-zero-refactor.md) — refactor Pi Zero demo to
  share patterns with the `irl` CLI
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
