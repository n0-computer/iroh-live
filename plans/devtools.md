# Devtools — egui extraction + dev example

The debug overlay is implemented (see
[docs/architecture/devtools.md](../docs/architecture/devtools.md)). This
plan covers extracting shared egui code and building a multi-endpoint
development tool.

## Extract common_egui modules

Move shared code from `rooms.rs` and `watch.rs` examples into
`moq-media-egui`:

- [ ] `video_view.rs` — VideoView texture rendering
- [ ] `remote_track_view.rs` — remote peer video + overlay
- [ ] `grid.rs` — video grid layout
- [ ] `room_view.rs` — complete room view widget
- [ ] `debug_overlay.rs` — toggleable debug overlay (stats bars, paths,
  catalog, buffering)
- [ ] `env_helpers.rs` — shared env/key helpers

## Create dev.rs example

Multi-endpoint splitscreen development tool for testing pub/sub flows
without running separate processes.

- [ ] Multi-endpoint support (A, B, C labels)
- [ ] Setup screen: ticket source, media source, codec selection
- [ ] Per-endpoint RoomView + DebugOverlay tiles
- [ ] Full-width "Add Endpoint" button, tile grid (1–6+)
- [ ] Import file support (sync test video + custom path)
- [ ] Copy ticket button per endpoint

## Future work

- [ ] A. Network condition simulation (latency/loss/BW sliders per link)
- [ ] B. Timeline scrubber + frame inspector (arrival/decode/gaps)
- [ ] C. Recording + playback (record MOQ streams, replay)
- [ ] D. Side-by-side A/B comparison (same stream, different renditions)
- [ ] E. Automated test scenarios (script-driven sequences)
- [ ] F. Prometheus/metrics export (Grafana dashboards)
- [ ] G. Audio waveform visualization (real-time spectrum)
- [ ] H. Adaptive bitrate visualization (rendition selection history)
