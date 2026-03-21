# Call example simplification

The `split.rs` example is ~1100 lines. Target: 200–300 lines of
application logic with the rest in library crates.

## Checklist

- [ ] **A. Demo sources** — add `demo-sources` feature to moq-media
  exposing `TestPatternSource` (SMPTE bars + bouncing ball) and
  `TestToneSource`. Keep `test-util` for test infrastructure. Animation
  makes codec/latency/framerate issues visible.

- [ ] **B. Source discovery** — add `discover_sources() -> Vec<DiscoveredSource>`
  to rusty-capture. Enumerates screens + cameras, deduplicates. Reduces
  example boilerplate from ~75 lines to one call.

- [ ] **C. Egui helpers** — move `fit_to_aspect`, `overlay_bar`,
  `FrameTimingStats` into moq-media-egui. Rename `FrameStats` →
  `FpsCounter`.

- [ ] **D. Source replacement** — add `replace_source(source)` on
  `VideoPublisher` / `AudioPublisher` that keeps current codec and
  presets. Reduces dynamic source change from six lines to one.

- [ ] **E. Auto-resubscribe** — add `LiveMediaTracks` wrapper (or
  `MediaTracks::watch()` async method) that resubscribes on catalog
  change internally. Eliminates the 60-line `resubscribe` method and
  `catalog_watcher.update()` polling in examples.
