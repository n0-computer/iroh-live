# Implementation Plan

Step-by-step plan for the API redesign. Each phase compiles and passes tests.

See `0-overview.md` for direction, `3-sketch.md` for target API.

## Principles

- **Each phase compiles and passes tests.**
- **Rename first, then add.** Get naming right before adding new surface.
- **moq-media first.** All broadcast/media improvements land there before
  iroh-live touches anything.
- **Deprecate, don't delete.** Old names get `#[deprecated]` aliases during
  transition. Remove in a later semver-major.

## Phase Overview

| Phase | What | Crate | Scope |
|-------|------|-------|-------|
| 1 | Rename moq-media types | moq-media | medium |
| 2 | Improve Broadcast (slot API, `&self`) | moq-media | medium |
| 3 | Improve Subscription (VideoTarget, options, status) | moq-media | medium |
| 4 | Error types + domain errors | moq-media | small |
| 5 | Relay support | moq-media | small-medium |
| 6 | Incoming session support | iroh-moq | small |
| 7 | `Live::builder()` + Call API | iroh-live | medium |
| 8 | Room participant model + Stream events | iroh-live | medium-large |
| 9 | Polish, prelude, deprecation removal, examples | all | small |

---

## Phase 1: Rename moq-media Types

**Goal**: Get naming right. All old names become deprecated aliases.

### 1.1 Core renames

| Old | New | File |
|-----|-----|------|
| `PublishBroadcast` | `Broadcast` | `publish.rs` |
| `SubscribeBroadcast` | `Subscription` | `subscribe.rs` |
| `WatchTrack` | `VideoTrack` | `subscribe.rs` |
| `CatalogWrapper` | `CatalogSnapshot` | `subscribe.rs` |
| `AvRemoteTrack` | `MediaTracks` | `subscribe.rs` |

### 1.2 Method renames on Subscription

| Old | New |
|-----|-----|
| `watch()` | `video()` |
| `watch_with()` | `video_with()` (temporary, replaced in Phase 3) |
| `watch_rendition()` | `video_rendition()` |
| `listen()` | `audio()` |
| `listen_with()` | `audio_with()` (temporary, replaced in Phase 3) |
| `listen_rendition()` | `audio_rendition()` |
| `watch_and_listen()` | `media()` |
| `watch_local()` on Broadcast | `preview()` |

### 1.3 Deprecated aliases

```rust
#[deprecated(since = "0.x", note = "renamed to `Broadcast`")]
pub type PublishBroadcast = Broadcast;

#[deprecated(since = "0.x", note = "renamed to `Subscription`")]
pub type SubscribeBroadcast = Subscription;

#[deprecated(since = "0.x", note = "renamed to `VideoTrack`")]
pub type WatchTrack = VideoTrack;
// etc.
```

### 1.4 Re-exports at crate root

```rust
// moq-media/src/lib.rs
pub use publish::Broadcast;
pub use subscribe::{AudioTrack, Subscription, VideoTrack};
```

### 1.5 Update all internal callers

- Examples, tests, iroh-live, moq-media internals all switch to new names.
- iroh-live re-exports: `pub use moq_media::{Broadcast, Subscription, VideoTrack, AudioTrack};`

### Done when

- All tests pass with new names.
- Old names still compile (deprecated).
- `cargo clippy` clean (allow deprecation warnings in tests temporarily).

---

## Phase 2: Improve Broadcast

**Goal**: Add slot sub-handles, switch to `&self`, make publishing ergonomic.

### 2.1 Interior mutability

Change `Broadcast` (formerly `PublishBroadcast`) internal structure:

```rust
struct BroadcastInner {
    video: Mutex<Option<VideoRenditions>>,
    audio: Mutex<Option<AudioRenditions>>,
    catalog: CatalogProducer,
    producer: BroadcastProducer,
    shutdown: CancellationToken,
}
```

Existing `set_video(&mut self, ...)` and `set_audio(&mut self, ...)` become
internal methods that lock the mutex. Public API uses `&self`.

### 2.2 VideoPublisher / AudioPublisher

New types sharing the `Arc<BroadcastInner>`:

```rust
impl Broadcast {
    pub fn video(&self) -> VideoPublisher { VideoPublisher { inner: self.inner.clone() } }
    pub fn audio(&self) -> AudioPublisher { AudioPublisher { inner: self.inner.clone() } }
}
```

`VideoPublisher` methods:
- `set(source, codec, presets)` — builds `VideoRenditions`, stores in mutex
- `set_with::<E>(source, presets)` — generic encoder factory
- `replace(source)` — swap source, keep codec/presets
- `clear()` — remove video
- `set_enabled(bool)` / `is_enabled()` — pause/resume
- `renditions()` — list current rendition names

Same pattern for `AudioPublisher` with `set_muted` instead of `set_enabled`.

### 2.3 Convenience methods

- `broadcast.has_video()` / `has_audio()` — check if slot is populated
- `broadcast.preview(config)` — renamed from `watch_local()`

### 2.4 Replace PublishCaptureController

`PublishCaptureController` is superseded by the slot sub-handles:
- Camera/mic setup moves to `broadcast.video().set()` / `broadcast.audio().set()`
- Source switching moves to `broadcast.video().replace()`
- Enable/disable moves to `broadcast.video().set_enabled()` / `broadcast.audio().set_muted()`
- `PublishCaptureController` becomes an internal implementation detail or is
  removed entirely. It can be replaced by a thin `CaptureManager` utility that
  coordinates device enumeration and source creation, but does not own the
  broadcast publishing logic.
- `PublishOpts` is replaced by direct method calls on the slot sub-handles.
- `RoomPublisherSync` is removed — the room actor calls the slot API directly.

### 2.5 Update callers

- iroh-live `Room::publish()` and `RoomPublisherSync` adapt
- Examples use new API internally

### Done when

- `&self` everywhere on `Broadcast`
- Slot API works: `broadcast.video().set(source, codec, presets)?`
- Preview works: `broadcast.preview(config)`
- `PublishCaptureController` no longer in the public API
- All existing tests pass

---

## Phase 3: Improve Subscription

**Goal**: Add `VideoTarget`, options builders, status tracking, dual-accessor
pattern, `frames()` stream.

### 3.1 VideoTarget type

New type in `subscribe.rs`:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct VideoTarget {
    pub max_pixels: Option<u32>,
    pub max_bitrate_kbps: Option<u32>,
    pub rendition: Option<String>,
}

impl From<Quality> for VideoTarget { ... }
```

### 3.2 Option types

New types in `subscribe.rs`, all `#[non_exhaustive]`:

- `VideoOptions` — `playback`, `target: VideoTarget`, `viewport` with builder methods
- `AudioOptions` — `quality`, `rendition` with builder methods

`VideoOptions::quality(Quality)` is sugar for `target(quality.into())`.

### 3.3 New subscription methods

Replace the generic-decoder methods with `DynamicVideoDecoder`-based defaults:

```rust
impl Subscription {
    pub fn video(&self) -> Result<VideoTrack, SubscribeError> { ... }
    pub fn video_with(&self, options: VideoOptions) -> Result<VideoTrack, SubscribeError> { ... }
    pub async fn audio(&self, backend: ...) -> Result<AudioTrack, SubscribeError> { ... }
    pub async fn audio_with(&self, options: AudioOptions, backend: ...) -> Result<AudioTrack, SubscribeError> { ... }
    pub async fn media(&self, backend: ...) -> Result<MediaTracks, SubscribeError> { ... }
}
```

The old generic-decoder methods (`video_rendition::<D>()`, `audio_rendition::<D>()`)
stay as advanced API.

### 3.4 Dual-accessor status tracking

Add `Watchable<BroadcastStatus>` to `SubscriptionInner`:
- Set to `Connecting` on creation
- Set to `Live` when first catalog arrives
- Set to `Ended` when broadcast closes

Expose dual-accessor:
- `fn status(&self) -> BroadcastStatus` — snapshot
- `fn status_watcher(&self) -> impl Watcher<Value = BroadcastStatus>` — subscribe

### 3.5 Dual-accessor catalog

- `fn catalog(&self) -> CatalogSnapshot` — snapshot
- `fn catalog_watcher(&self) -> impl Watcher<Value = CatalogSnapshot>` — subscribe

### 3.6 Catalog helpers

- `has_video()` / `has_audio()` — derived from catalog
- `video_renditions()` / `audio_renditions()` — sorted lists

### 3.7 CatalogSnapshot improvements

Rename `CatalogWrapper` to `CatalogSnapshot`. Add:
- `select_video(quality)` and `select_audio(quality)`
- `select_video_target(target)` for `VideoTarget`-based selection

### 3.8 VideoTrack frames() stream

Add `fn frames(&mut self) -> impl Stream<Item = VideoFrame>` to `VideoTrack`
for StreamExt composability. Wraps the internal `mpsc::Receiver`.

### Done when

- `sub.video()` works with automatic decoder selection
- `VideoTarget` selection works: `sub.video_with(VideoOptions::default().target(VideoTarget::default().max_pixels(640*480)))?`
- Quality shorthand works: `sub.video_with(VideoOptions::default().quality(Quality::Low))?`
- Dual-accessor fires correctly for status and catalog
- `video.frames()` returns a composable stream
- All existing tests pass

---

## Phase 4: Error Types

**Goal**: Domain-specific errors on moq-media public API.

### 4.1 Create error module

```rust
// moq-media/src/error.rs
pub enum PublishError { NoVideoSource, NoAudioSource, CodecUnavailable(String), EncoderFailed(anyhow::Error) }
pub enum SubscribeError { NotFound, NoCatalog, RenditionNotFound(String), DecoderFailed(anyhow::Error), Ended }
```

### 4.2 Update public method signatures

Change all public `Result<T>` (anyhow) to `Result<T, PublishError>` or
`Result<T, SubscribeError>` as appropriate. Internal code continues to use
`anyhow` and converts at the public boundary with `impl From<anyhow::Error>`.

### 4.3 iroh-live error types

```rust
pub enum CallError { ConnectionFailed(anyhow::Error), Rejected, Ended(DisconnectReason) }
pub enum RoomError { JoinFailed(anyhow::Error), GossipFailed(anyhow::Error), Closed }
```

### Done when

- All public methods return domain errors
- Internal code still uses anyhow
- Error types are `Debug + Display + Error + Send + Sync`

---

## Phase 5: Relay Support

**Goal**: First-class zero-transcode relay and `VideoTrack` as `VideoSource`.

### 5.1 `VideoTrack: VideoSource`

Implement `VideoSource` trait on `VideoTrack`:
- `name()` → rendition name
- `format()` → derive from last received frame or decoder output
- `start()` / `stop()` → no-op (already running)
- `pop_frame()` → delegates to `current_frame()`

This enables transcode relay: `sub.video()` → feed into `Broadcast` as source.

### 5.2 `Broadcast::relay()`

```rust
impl Broadcast {
    pub fn relay(subscription: &Subscription) -> Result<Self, PublishError> { ... }
}
```

Implementation:
1. Create a new `Broadcast`
2. Copy the catalog from the subscription
3. For each track in the catalog, spawn a forwarding task:
   - Get `TrackConsumer` from subscription's `BroadcastConsumer`
   - Create `TrackProducer` on the new broadcast's `BroadcastProducer`
   - Use `forward_packets()` (already exists in `pipeline.rs`) to forward
4. Watch for catalog changes and adapt (new tracks → new forwarding tasks)

### 5.3 Tests

- Zero-transcode relay: publish → subscribe → relay → subscribe → verify frames match
- Transcode relay: publish 1080p → subscribe → `VideoTrack` as source → re-encode 360p
- Relay catalog propagation: add rendition on source → appears on relay

### Done when

- `Broadcast::relay(&sub)?` works end-to-end
- `VideoTrack` can be passed to `broadcast.video().set_with::<E>(track, presets)`
- Tests pass

---

## Phase 6: Incoming Session Support

**Goal**: Surface incoming MoQ sessions for accept/reject at transport level.

### 6.1 IncomingSession type (iroh-moq)

```rust
pub struct IncomingSession {
    remote_id: EndpointId,
    session: MoqSession,
}

impl IncomingSession {
    pub fn remote_id(&self) -> EndpointId { ... }
    pub fn into_session(self) -> MoqSession { ... }
}
```

### 6.2 Stream in Moq actor

- Add `incoming_tx: mpsc::Sender<IncomingSession>` to actor
- In `MoqProtocolHandler::handle_connection`, after session accept, send to channel
- New method: `Moq::incoming() -> impl Stream<Item = IncomingSession>` (wraps receiver)

### 6.3 Tests

- Peer A connects to peer B, B receives IncomingSession
- Session is usable for publish/subscribe

### Done when

- `moq.incoming()` returns a stream of incoming sessions
- Existing auto-accept behavior unchanged for rooms

---

## Phase 7: `Live::builder()` + Call API

**Goal**: Builder-based entry point and one-to-one calls with accept/reject.

### 7.1 LiveBuilder

```rust
pub struct LiveBuilder { ... }
impl LiveBuilder {
    pub fn secret_key(self, key: SecretKey) -> Self { ... }
    pub fn relay_url(self, url: Url) -> Self { ... }
    pub fn gossip(self, enabled: bool) -> Self { ... }
    pub async fn spawn(self) -> Result<Live> { ... }
}
impl Live {
    pub fn builder() -> LiveBuilder { ... }
}
```

`Live::new()` remains for advanced use (existing components).

### 7.2 Call types (iroh-live/src/call.rs)

- `CallTicket` — `{ remote: EndpointAddr }`, `Display`/`FromStr`, `From<EndpointAddr>`
- `CallState` — `{ Connecting, Connected, Disconnected(DisconnectReason) }`
- `CallError` — `{ ConnectionFailed, Rejected, Ended }`
- `Call` — Arc-based, holds `Broadcast` + `Option<Subscription>` + dual-accessor state
- `IncomingCall` — Option-wrapped inner, auto-reject on drop

### 7.3 Wire into Live

- `Live::call(ticket)` → connect session → create `Call` with local `Broadcast`
- `Live::incoming_calls()` → receive `IncomingSession` → wrap as `impl Stream<Item = IncomingCall>`
- `IncomingCall::accept()` → create `Call`

### 7.4 Example

`examples/call.rs`: peer A calls peer B, both publish camera + mic, both
subscribe to remote video + audio.

### Done when

- Call example works end-to-end
- IncomingCall auto-reject on drop works
- `Live::builder().spawn().await?` works

---

## Phase 8: Room Participant Model + Stream Events

**Goal**: Redesign Room around participants, emit events via `impl Stream`.

### 8.1 Participant types

- `LocalParticipant` — `{ id: ParticipantId, broadcast: Broadcast }`
- `RemoteParticipant` — `{ id: ParticipantId, subscription: Subscription }`

These are thin. They add identity, not media logic.

### 8.2 Redesign RoomEvent

```rust
pub enum RoomEvent {
    ParticipantJoined(RemoteParticipant),
    ParticipantLeft { participant: ParticipantId, reason: DisconnectReason },
    TrackPublished { participant: ParticipantId, kind: TrackKind, rendition: String },
    TrackUnpublished { participant: ParticipantId, kind: TrackKind, rendition: String },
}
```

Events carry full handles where appropriate (`RemoteParticipant` in
`ParticipantJoined`), not just IDs. Users can immediately call
`p.subscription().video()` without a follow-up lookup.

### 8.3 Room actor changes

- Maintain `HashMap<ParticipantId, RemoteParticipant>`
- On `RemoteConnected` → create `RemoteParticipant` with `Subscription`, emit `ParticipantJoined`
- Watch each participant's catalog → diff → emit `TrackPublished` / `TrackUnpublished`
- On disconnect → emit `ParticipantLeft`

### 8.4 Room API with Stream + dual-accessor

```rust
impl Room {
    pub fn local(&self) -> &LocalParticipant { ... }
    pub fn remote_participants(&self) -> Vec<RemoteParticipant> { ... }  // owned snapshot
    pub fn remote_participant(&self, id: &ParticipantId) -> Option<RemoteParticipant> { ... }
    pub fn remote_participants_watcher(&self) -> impl Watcher<Value = Vec<RemoteParticipant>> { ... }
    pub fn events(&self) -> impl Stream<Item = RoomEvent> + '_ { ... }
    // ...
}
```

### 8.5 Update room example

```rust
let room = live.join_room(ticket).await?;
room.local().broadcast().video().set(camera, codec, presets)?;

use futures::StreamExt;
let mut events = room.events();
while let Some(event) = events.next().await {
    match event {
        RoomEvent::ParticipantJoined(p) => {
            let video = p.subscription().video()?;
            let audio = p.subscription().audio(backend).await?;
            // render video, play audio
        }
        RoomEvent::ParticipantLeft { participant, .. } => {
            // cleanup
        }
        _ => {}
    }
}
```

### Done when

- Room example works with new API
- Events flow as `impl Stream`
- Participant queries return owned handles
- Old API still compiles (deprecated)

---

## Phase 9: Polish

### 9.1 Deprecation removal

Remove old type aliases and method names (semver-major).

### 9.2 Prelude module

```rust
pub mod prelude {
    pub use crate::{Live, Room, RoomEvent, Call, IncomingCall};
    pub use crate::room::{LocalParticipant, RemoteParticipant};
    pub use crate::types::ParticipantId;
    pub use crate::{CallTicket, RoomTicket};
    pub use crate::{Broadcast, Subscription, VideoTrack, AudioTrack, VideoTarget, Quality};
    pub use moq_media::AudioBackend;
}
```

### 9.3 Documentation pass

- Module-level `//!` docs
- Type-level docs following RFC 1574
- Examples on key types

### 9.4 Example suite

- `examples/call.rs` — one-to-one call
- `examples/room.rs` — multi-party room
- `examples/stream.rs` — one-to-many streaming (moq-media only, no iroh-live)
- `examples/relay.rs` — zero-transcode relay
- `examples/transcode.rs` — transcode relay
- `examples/dashboard.rs` — multi-source viewer with `VideoTarget`

---

## Implementation Notes

### Interior mutability strategy

| Type | Strategy | Reason |
|------|----------|--------|
| `Broadcast` | `Arc<BroadcastInner>` with `Mutex<Option<VideoRenditions>>` | `set_video`/`set_audio` need mutation |
| `Subscription` | `Arc<SubscriptionInner>` with `Watchable<T>` | Dual-accessor pattern for status + catalog |
| `Room` | Actor + `RwLock<HashMap>` for participants | Concurrent reads, owned snapshot returns |
| `Call` | `Watchable<CallState>` + Arc refs | Mostly reads |

### Thread safety

All public types `Send + Sync`. Verified with:
```rust
fn assert_send_sync<T: Send + Sync>() {}
assert_send_sync::<Broadcast>();
assert_send_sync::<Subscription>();
// etc.
```

### What NOT to change

- `iroh-moq` protocol behavior (only add incoming stream)
- `moq-lite` / `hang` types
- Codec implementations
- Pipeline threading model
- `web-transport-iroh`

### Open questions

1. **Should `Broadcast::relay()` track catalog changes?** Yes — watch the
   catalog and spawn new forwarding tasks when renditions are added.

2. **Should `VideoTrack::pop_frame()` block or return None?** Return `None`
   when no frame is ready (non-blocking), matching the existing `VideoSource`
   contract. The encoder thread polls in a loop.

3. **Should `Call` auto-subscribe?** Yes — the subscription is lazy
   (no decoding until you call `video()` or `audio()`), so there's no cost.

4. **Room auto-subscribe policy.** Current rooms auto-subscribe to all
   participants. Keep this but make it configurable via `RoomOptions` in a
   future iteration.

5. **Should `PublishCaptureController` be preserved?** No — the slot sub-handles
   (`VideoPublisher`/`AudioPublisher`) replace its functionality. Device
   enumeration/creation can move to a small `CaptureManager` utility that
   doesn't own broadcast state.

6. **Should `frames()` on VideoTrack consume `&mut self` or `&self`?** `&mut self`
   — it wraps `mpsc::Receiver` which requires `&mut` for `recv()`. Users who need
   concurrent access should use the snapshot `current_frame()` pattern instead.
