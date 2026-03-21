# Capture pipeline — deferred work

Phase 1 (unified `VideoInput` enum, `set()` entry point) is complete.
See [docs/architecture/publish.md](../docs/architecture/publish.md) and
[docs/architecture/capture.md](../docs/architecture/capture.md) for the
current architecture.

## Phase 2 checklist

- [ ] `VideoProcessor` trait + builder pattern — deferred until the first
  processing use case arrives (crop, overlay, background blur)
- [ ] `PreEncodedAudioSource` + `AudioInput` enum — mirror the video
  pattern for pre-encoded audio (e.g. Opus passthrough from file)
- [ ] Keyframe request signaling for pre-encoded sources — let the
  transport request an IDR when a new subscriber joins mid-stream
- [ ] Config-after-start for pre-encoded sources — allow `config()` to
  return `None` initially, update via watcher after first keyframe
  arrives (eliminates the priming-encode hack in VAAPI)
