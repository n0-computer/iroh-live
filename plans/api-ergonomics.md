# API Ergonomics Plan

Concrete proposals for improving iroh-live/moq-media/rusty-* APIs to reduce
boilerplate in the CLI and any future applications.

See also `REVIEW.md` section "Expert review — ergonomics (2026-03-26)" for the
full findings list (ER1–ER25).

## Done

- [x] **1. Unified device selection** — `CameraCapturer::open()` and
  `ScreenCapturer::open()` in `rusty-capture/src/lib.rs`
- [x] **2. `VideoPublisher::set_source()`** — in `moq-media/src/publish.rs`
- [x] **3. `AudioBackend::*_blocking()` helpers** — `default_input_blocking`,
  `switch_input_blocking`, `switch_output_blocking` in `audio_backend.rs`
- [x] **4. Codec/preset `parse_or_list()` / `parse_or_best()`** — in
  `rusty-codecs/src/codec.rs` and `rusty-codecs/src/format.rs`
- [x] **5. Default presets** — use `VideoPreset::all()` (already available)
- [x] **7. File preview** — `LocalBroadcast::subscribe_preview()` and
  `subscribe_preview_from_consumer()` in `moq-media/src/publish.rs`

- [x] **8. Auto-wired subscribe** — `Live::subscribe()` returns `Subscription`
  which auto-wires stats + signals. `subscribe_with_stats()` removed.

## Remaining

- [ ] **6. Prelude module** — add `moq_media::prelude` re-exporting common
  types. Low effort, reduces 6-8 nested import paths to 1-2 per file.
