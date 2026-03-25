# API Ergonomics Plan

Concrete proposals for improving iroh-live/moq-media/rusty-* APIs to reduce
boilerplate in the CLI and any future applications. No backwards compatibility
constraints.

## 1. Unified device selection on capturers

**Problem:** The CLI reimplements device selection logic (50 lines in
`source.rs`) that every application will need. Camera and screen capturers
have `with_backend()` but no "find by ID or name" method.

**Change:** Add `CameraCapturer::open()` and `ScreenCapturer::open()` that
accept optional backend and optional ID, with helpful error messages listing
available devices on failure.

```rust
// rusty-capture/src/lib.rs
impl CameraCapturer {
    /// Opens a camera matching the given backend and/or ID.
    ///
    /// When only `backend` is given, selects the first camera on that backend.
    /// When only `id` is given, searches all cameras by ID or name.
    /// When both are given, requires an exact match.
    /// When neither is given, uses the system default.
    pub fn open(
        backend: Option<CaptureBackend>,
        id: Option<&str>,
        config: &CameraConfig,
    ) -> anyhow::Result<Self> { ... }
}

impl ScreenCapturer {
    /// Opens a screen matching the given backend and/or ID. Same semantics.
    pub fn open(
        backend: Option<CaptureBackend>,
        id: Option<&str>,
        config: &ScreenConfig,
    ) -> anyhow::Result<Self> { ... }
}
```

Error messages should list available devices:
```
camera 'usb-3' not found. Available: Integrated Camera (pipewire, /dev/video0), USB Webcam (v4l2, /dev/video2)
```

**CLI impact:** `source.rs` drops from 148 → ~80 lines. The `open_camera()`
and `open_screen()` functions become one-liners delegating to library methods.

## 2. Convenience methods on VideoPublisher/AudioPublisher

**Problem:** Setting up a video source requires three steps every time:
```rust
let camera = CameraCapturer::new()?;
broadcast.video().set(VideoInput::new(camera, codec, presets.to_vec()))?;
```

This pattern repeats in source.rs, ui.rs (DeviceSelectors), and call.rs.

**Change:** Add source-aware setters on the publisher handles.

```rust
// moq-media/src/publish.rs
impl<'a> VideoPublisher<'a> {
    /// Sets video from any VideoSource.
    pub fn set_source(
        &self,
        source: impl VideoSource + 'static,
        codec: VideoCodec,
        presets: impl Into<Vec<VideoPreset>>,
    ) -> Result<()> {
        self.set(VideoInput::new(source, codec, presets))
    }
}
```

This is a small change but eliminates the `VideoInput::new()` wrapper from
every call site. The `set()` method stays for advanced use (pre-encoded
tracks).

**CLI impact:** Every `VideoInput::new(source, codec, presets.to_vec())`
becomes `broadcast.video().set_source(source, codec, &presets)`.

## 3. AudioBackend blocking helpers

**Problem:** In egui update callbacks (synchronous), every audio operation
requires `tokio::runtime::Handle::current().block_on(...)`. This pattern
appears 5 times in ui.rs, source.rs, and room.rs.

**Change:** Add `_blocking` variants for the common async methods.

```rust
// moq-media/src/audio_backend.rs
impl AudioBackend {
    pub fn default_input_blocking(&self) -> Result<InputStream> {
        tokio::runtime::Handle::current().block_on(self.default_input())
    }
    pub fn switch_input_blocking(&self, device: Option<DeviceId>) -> Result<()> {
        tokio::runtime::Handle::current().block_on(self.switch_input(device))
    }
    pub fn switch_output_blocking(&self, device: Option<DeviceId>) -> Result<()> {
        tokio::runtime::Handle::current().block_on(self.switch_output(device))
    }
}
```

**CLI impact:** Eliminates all manual `Handle::current().block_on()` calls.

## 4. Codec/preset parsing with helpful errors

**Problem:** `VideoCodec` and `VideoPreset` implement `FromStr` via strum,
but the error messages are generic ("Matching variant not found"). The CLI
wraps every parse call with custom error messages.

**Change:** Add `parse_or_list()` methods that include available values in
the error.

```rust
// rusty-codecs/src/codec.rs
impl VideoCodec {
    /// Parses a codec name, or returns an error listing available codecs.
    pub fn parse_or_list(s: &str) -> anyhow::Result<Self> {
        s.parse().map_err(|_| {
            let names: Vec<_> = Self::available().iter().map(|c| c.to_string()).collect();
            anyhow::anyhow!("unknown codec '{s}'. Available: {}", names.join(", "))
        })
    }

    /// Parses a codec name, or returns the best available if None.
    pub fn parse_or_best(s: Option<&str>) -> anyhow::Result<Self> {
        match s {
            Some(name) => Self::parse_or_list(name),
            None => Self::best_available()
                .ok_or_else(|| anyhow::anyhow!("no video codec compiled in")),
        }
    }
}
```

Same for `VideoPreset`, `AudioPreset`, `AudioCodec`.

**CLI impact:** `args.rs` codec/preset parsing shrinks from 30 lines to 3.

## 5. Default presets constant

**Problem:** `[VideoPreset::P180, VideoPreset::P360, VideoPreset::P720, VideoPreset::P1080]`
is repeated in args.rs and in examples. `VideoPreset::all()` exists but
returns a fixed-size array, which is fine.

**Change:** No code change needed — `VideoPreset::all()` already works.
Just use it in the CLI instead of the manual list. (Already missed this.)

**CLI impact:** Replace the manual list with `VideoPreset::all().to_vec()`.

## 6. Prelude module

**Problem:** CLI files import from 6-8 nested paths under `iroh_live::media::*`.

**Change:** Add `moq_media::prelude` re-exporting the commonly used types.

```rust
// moq-media/src/prelude.rs
pub use crate::{
    AudioBackend, AudioDevice,
    capture::{CameraCapturer, ScreenCapturer, CameraInfo, MonitorInfo, CaptureBackend},
    codec::{AudioCodec, VideoCodec, DefaultDecoders, DynamicVideoDecoder},
    format::{AudioPreset, VideoPreset, DecoderBackend, PlaybackConfig, DecodeConfig},
    publish::{LocalBroadcast, VideoInput},
    subscribe::{RemoteBroadcast, VideoTrack, AudioTrack, MediaTracks},
    test_sources::{TestPatternSource, TestToneSource},
    traits::VideoSource,
};
```

And in `iroh_live`: `pub use moq_media::prelude as media_prelude;`

**CLI impact:** Most files drop from 8 use statements to 1-2.

## 7. File publish preview

**Problem:** Capture preview works via `LocalBroadcast::preview()` which
returns a `VideoTrack` tapped directly from the encoder's source. File
publish uses `BroadcastProducer` directly, which has no preview capability.
`LocalBroadcast::preview()` explicitly returns `None` for pre-encoded inputs.

**How it works today:**
- `BroadcastProducer::consume()` creates independent consumers
- `RemoteBroadcast::new(name, consumer)` creates a subscription from a consumer
- Multiple consumers work in parallel (proven in tests)

**Change:** Add a function to create a preview from any producer.

```rust
// moq-media/src/publish.rs (or a new module)
impl LocalBroadcast {
    /// Creates a preview subscription that decodes the published media
    /// back for local display. Works for both live capture and file import.
    ///
    /// For live capture, prefer the existing [`preview()`] method which
    /// taps the raw source frames without re-encoding + decoding.
    /// This method goes through the full encode→mux→demux→decode path,
    /// which is useful for file imports and for verifying the published
    /// output matches expectations.
    pub async fn subscribe_preview<D: Decoders>(
        &self,
        audio_ctx: &AudioBackend,
        playback_config: PlaybackConfig,
    ) -> Result<MediaTracks> {
        let consumer = self.consume();
        let broadcast = RemoteBroadcast::new("preview", consumer).await?;
        broadcast.media::<D>(audio_ctx, playback_config).await
    }
}
```

For raw `BroadcastProducer` (used in file import before we have a
`LocalBroadcast`):

```rust
// Free function or on a helper struct
pub async fn preview_from_producer<D: Decoders>(
    producer: &BroadcastProducer,
    audio_ctx: &AudioBackend,
    playback_config: PlaybackConfig,
) -> Result<MediaTracks> {
    let consumer = producer.consume();
    let broadcast = RemoteBroadcast::new("preview", consumer).await?;
    broadcast.media::<D>(audio_ctx, playback_config).await
}
```

**CLI impact:** File preview window becomes possible. The publish command's
`run_file()` can create a preview the same way `run_capture()` does, just
using the producer-based path instead of the source-based one.

**Caveat:** The preview consumer must be created and subscribed *after* the
import starts writing the catalog (the `init_from()` step). The sequence is:
1. Create `BroadcastProducer`
2. Start `Import::init_from()` (writes catalog)
3. After init, call `preview_from_producer()` (reads catalog)
4. Continue with `Import::read_from()` while preview renders

This requires splitting the import flow, which the current `run_import()`
already does (it has separate `init_from` and `read_from` phases).

## 8. Stats recording as part of subscription

**Problem:** Every subscriber calls `spawn_stats_recorder(session.conn(), broadcast.stats().net.clone(), broadcast.shutdown_token())` manually. This is 4 lines repeated in play.rs, call.rs, room.rs, and ui.rs.

**Change:** Make stats recording opt-in at subscription time.

```rust
// iroh-live/src/live.rs
impl Live {
    /// Subscribes and automatically starts recording connection stats
    /// into the broadcast's stat counters.
    pub async fn subscribe_with_stats(
        &self,
        endpoint: EndpointId,
        name: &str,
    ) -> Result<(MoqSession, RemoteBroadcast)> {
        let (session, broadcast) = self.subscribe(endpoint, name).await?;
        crate::util::spawn_stats_recorder(
            session.conn(),
            broadcast.stats().net.clone(),
            broadcast.shutdown_token(),
        );
        Ok((session, broadcast))
    }
}
```

**CLI impact:** Every subscriber drops 4 lines.

## Implementation priority

| # | Change | Effort | Impact | Priority |
|---|--------|--------|--------|----------|
| 1 | `CameraCapturer::open()` / `ScreenCapturer::open()` | Small | High | P1 |
| 2 | `VideoPublisher::set_source()` | Tiny | Medium | P1 |
| 3 | `AudioBackend::*_blocking()` | Tiny | Medium | P1 |
| 4 | Codec/preset `parse_or_list()` | Small | Medium | P1 |
| 7 | File preview (`preview_from_producer`) | Medium | High | P1 |
| 8 | `subscribe_with_stats()` | Tiny | Medium | P2 |
| 6 | Prelude module | Tiny | Low | P2 |
| 5 | Use `VideoPreset::all()` | None | Low | P2 |

P1 items should be done together as they compound: items 1-4 together would
cut the CLI's source.rs + args.rs by ~40%, and item 7 enables file preview.
