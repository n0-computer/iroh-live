# CLI Tool — `irl`

**Crate:** `tools/iroh-live-cli` | **Binary:** `irl`

## Status: Implemented

The CLI consolidation is complete. All primary commands are functional:
`devices`, `publish` (capture + file), `play`, `call`, `room`.

Old examples (publish, push, watch, call, rooms, room-publish-file, common/)
have been removed. Kept examples: `split.rs`, `watch-wgpu.rs`, `frame_dump.rs`,
`subscribe_test.rs`.

## Remaining work

- [ ] `publish --preview` — egui window with local video preview and controls
- [ ] Room grid layout — self-view toggle (grid vs PiP)
- [ ] Auto-hide bars in play mode (appear on mouse, hide after ~2s)
- [ ] Shared control widgets: `source_selector()`, `codec_selector()`,
  `preset_selector()`, `rendition_selector()` in `ui.rs`

## Future: Raspberry Pi support

Once `libcamera` capture and GLES2 rendering are abstracted into library
crates, `irl` gains Pi support without platform-specific CLI code. Until then,
the pi-zero demo stays as a separate binary.

What works on Pi today: `irl devices` (V4L2 listing),
`irl publish --test-source` (software encode), `irl file` (no platform deps),
`irl play --video none` (audio only).
