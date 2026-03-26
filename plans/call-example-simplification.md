# Example simplification

The CLI consolidation (`iroh-live-cli`) addressed the main problem:
examples were 800-1100 lines each with duplicated infrastructure. The CLI
extracted shared logic into `transport.rs`, `source.rs`, `ui.rs`, and `import.rs`.

## Remaining library-level improvements

- [ ] **A. Demo sources** — `demo-sources` feature in moq-media exposing
  `TestPatternSource` (animated SMPTE bars) and `TestToneSource`. Currently
  only `test-util` feature exists with basic test patterns.

- [ ] **B. Source discovery** — `discover_sources() -> Vec<DiscoveredSource>`
  in rusty-capture. Enumerates screens + cameras, deduplicates.

- [ ] **C. Egui helpers** — `FrameTimingStats` / `FpsCounter` not yet in
  moq-media-egui. `fit_to_aspect` and `overlay_bar` are already extracted.

- [ ] **D. Source replacement** — `VideoPublisher::replace_source()` that
  keeps current codec and presets. Same as api.md item.

- [ ] **E. Auto-resubscribe** — `MediaTracks::watch()` or `LiveMediaTracks`
  wrapper that resubscribes on catalog change internally. Same as api.md item.
