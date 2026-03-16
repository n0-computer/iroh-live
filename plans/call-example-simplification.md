# Call example simplification

The `split.rs` example is 1120 lines. A reasonable call or split-screen demo
should be 200--300 lines of actual application logic — the rest is
infrastructure that belongs in library crates. This document identifies where
the lines go, why they ended up in example code, and what changes to
iroh-live and moq-media would let a call example focus on its real job:
managing a call.

## Where the lines go

| Category | Lines | Belongs in lib? |
|---|---|---|
| `TestPatternSource` (SMPTE bars + bouncing ball) | ~140 | Yes |
| `TestToneSource` (880 Hz beep) | ~55 | Yes |
| `discover_video_sources` + `DiscoveredVideoSource` | ~75 | Yes |
| `configure_publish` (wire source to renditions to broadcast) | ~45 | Partially |
| Catalog resubscribe logic (`resubscribe` method) | ~60 | Yes |
| `FrameStats` | ~30 | Yes |
| `fit_to_aspect` + `overlay_bar` + `OVERLAY_BAR_H` | ~30 | Yes (egui crate) |
| `AudioSourceKind` + `RenderMode` enums | ~15 | Not directly |
| Actual application logic (state, UI, main) | ~250 | No |
| egui boilerplate (controls, overlay painting) | ~420 | Partially |

The application-specific 250 lines are unavoidable — that is the example.
The remaining ~870 lines are reusable infrastructure repeated across
`split.rs`, `watch.rs`, and future examples.

## Root causes

### 1. No shareable demo sources

`moq-media` already has `TestVideoSource` and `SineAudioSource` in
`test_util.rs`, but they are gated behind `#[cfg(any(test, feature =
"test-util"))]`. The test sources are also deliberately minimal: no
animation, no visual indicators. Every example that wants a visible test
pattern copies its own `TestPatternSource` with SMPTE bars, a bouncing ball,
and a beep indicator.

The `test-util` feature is inappropriate for examples — it also exposes
`NullAudioBackend`, `CapturingAudioBackend`, and other testing internals that
should not appear in demo builds. A separate feature is needed.

### 2. No source discovery function

`discover_video_sources()` enumerates screen and camera backends, deduplicates
them, and returns a list. Every egui example reimplements this. The function
belongs in `rusty-capture` or `moq-media::capture` as a public API, since it
only depends on `CameraCapturer` and `ScreenCapturer`.

### 3. Publishing requires too many intermediate types

A minimal camera + microphone publish currently looks like this:

```rust
let broadcast = LocalBroadcast::new();
let camera = CameraCapturer::new()?;
let video = VideoRenditions::new(camera, VideoCodec::best_available(), [VideoPreset::P720]);
broadcast.video().set_renditions(video)?;
let mic = audio_ctx.default_input().await?;
let audio = AudioRenditions::new(mic, AudioCodec::Opus, [AudioPreset::Hq]);
broadcast.audio().set_renditions(audio)?;
```

Seven lines touching five types. The `publish.rs` example is already trimmed
to the minimum — it uses `broadcast.video().set(source, codec, presets)` and
`broadcast.audio().set(source, codec, presets)`, which is better. But `split.rs`
predates that API and still uses `VideoRenditions::new` explicitly.

The deeper issue is that replacing a source requires re-creating
`VideoRenditions` and calling `set_renditions` again — there is no
`replace_source` or `republish` method that swaps the source while keeping
codec and preset settings intact.

### 4. Catalog resubscribe is manual and error-prone

When the publisher changes codec, preset, or source, the catalog updates and
every subscriber must: detect the change via `catalog_watcher.update()`, drop
old video/audio tracks, pick new renditions from the updated catalog, create
new decoder pipelines, and wire them to the UI. The `split.rs` `resubscribe`
method is 60 lines. `watch.rs` avoids this by not handling dynamic publisher
changes at all.

`MediaTracks` already subscribes to the initial catalog automatically. It does
not watch for catalog changes or resubscribe. Adding that capability would
eliminate the most error-prone boilerplate in subscriber examples.

### 5. Shared egui helpers are not shared

`fit_to_aspect`, `overlay_bar`, `FrameStats`, and `OVERLAY_BAR_H` live in
`split.rs` and are duplicated (or will be) in any egui-based example.
`moq-media-egui` is the natural home for these since it already provides
`VideoTrackView` and `format_bitrate`.

### 6. EndpointAddr serialization was a problem (now solved)

`LiveTicket` already implements `Display` and `FromStr` with a
`name@base32addr` format. The `split.rs` example does not use it because it
connects internally, but any two-process call example would benefit from it.
This is already solved.

## Recommendations

### Quick wins — move existing code

**A. Demo sources feature in moq-media.** Add a `demo-sources` feature that
exposes `TestPatternSource` (animated SMPTE + bouncing ball) and
`TestToneSource` (periodic beep) as public types. Keep `test-util` for
`NullAudioBackend`, `CapturingAudioBackend`, and other test infrastructure.
The demo sources need to be more visually useful than the current
`TestVideoSource` — they should include animation so that codec, latency, and
frame rate problems are immediately visible.

Estimated effort: small. Most of the code exists in `split.rs`; it needs
cleanup, docs, and a feature gate.

**B. Source discovery in rusty-capture.** Add `discover_sources() ->
Vec<DiscoveredSource>` to `rusty-capture` (or `moq-media::capture`). The
function enumerates screen backends and cameras, deduplicates, and returns a
display-friendly list. Each entry carries enough information to construct a
capturer. Examples drop from ~75 lines of discovery logic to one function
call.

Estimated effort: small. The logic exists verbatim in `split.rs`.

**C. Egui helpers in moq-media-egui.** Move `fit_to_aspect`, `overlay_bar`,
and `FrameStats` into `moq-media-egui`. These are small, stable, and already
duplicated. `FrameStats` could be renamed to something clearer like
`FpsCounter` or `FrameTimingStats`.

Estimated effort: trivial.

### Medium effort — API sugar

**D. Simplify source replacement on `VideoPublisher`/`AudioPublisher`.** The
current API requires constructing a new `VideoRenditions` and calling
`set_renditions` whenever anything changes. A `replace_source(source)` method
on `VideoPublisher` that keeps the current codec and presets would reduce the
republish path from six lines to one. The current `set(source, codec,
presets)` shorthand in `publish.rs` is already good for initial setup; this
targets the dynamic-change case.

Estimated effort: medium. The encoder pipeline teardown and restart logic
exists but is not surfaced as a single operation.

**E. Auto-resubscribe on `MediaTracks` (or a new `LiveMediaTracks`).** The
subscriber's catalog-change → resubscribe cycle is the largest single chunk
of boilerplate. Two options:

1. Add a `MediaTracks::watch()` async method that yields a new
   `MediaTracks` whenever the catalog changes, handling decoder pipeline
   teardown and recreation internally.
2. Add a `LiveMediaTracks` wrapper that holds a `MediaTracks` and a catalog
   watcher, exposing `video()` and `audio()` accessors that always return
   the current track. On catalog change, it resubscribes internally and
   the caller just sees new frames.

Option 2 is simpler for callers but hides decoder choice. Option 1 gives the
caller a chance to adjust decode config between resubscribes.

Either option eliminates the 60-line `resubscribe` method and the
`catalog_watcher.update()` polling in the UI loop.

Estimated effort: medium. The resubscribe logic is well-understood but needs
careful lifecycle management — dropping old decoder threads cleanly while new
ones start.

### What this gets us

With changes A through E, a split-screen example would look roughly like:

- ~15 lines: endpoint setup, `Live::new`, `Router` spawn
- ~10 lines: create `LocalBroadcast`, set camera + mic sources
- ~5 lines: `live.publish(name, &broadcast)`
- ~10 lines: connect to publisher, get `LiveMediaTracks`
- ~200 lines: egui app with controls, video rendering, overlay stats

That is approximately 240 lines, with zero copy-pasted infrastructure. The
egui-specific UI code remains in the example where it belongs.

### What to leave alone

The egui control widgets (combo boxes for source/codec/preset selection) are
genuinely example-specific. Different applications will want different UI
frameworks and different control layouts. Trying to abstract these into a
library creates a rigid widget that nobody actually wants. The right answer is
to make the underlying operations (change source, change codec, list
available renditions) so simple that the UI code is trivially short.

The `PublishCaptureController` already attempts this for the rooms use case
but is tightly coupled to camera/screen/audio toggles. It is better to keep
the low-level `VideoPublisher`/`AudioPublisher` API clean and composable
rather than building more opinionated controllers.

## Priority order

1. **A (demo sources)** and **C (egui helpers)** — immediate wins, no API
   design needed, unblocks cleaner examples today.
2. **B (source discovery)** — small and self-contained, reduces boilerplate
   in every capture-based example.
3. **E (auto-resubscribe)** — largest payoff for subscriber examples, needs
   design thought around decoder lifecycle.
4. **D (source replacement)** — nice but lower impact; the current API works,
   it is just verbose.
