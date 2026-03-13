# Implementation Plan

Step-by-step plan for implementing the three-layer API redesign. Each phase produces
a working codebase — no phase leaves things broken.

See `0-overview.md` for the design, `3-sketch.md` for the target API shape.

## Principles

- **Each phase compiles and passes tests.** No "we'll fix it in the next phase."
- **Additive first, remove later.** New types wrap old ones. Old types get deprecated.
- **Examples update with each phase.** At least one example should use the new API.
- **Keep moq-media unchanged as long as possible.** It's transport-agnostic and stable.

## Phase Overview

| Phase | What | Depends on | Estimated scope |
|-------|------|------------|-----------------|
| 1 | Shared types + broadcast wrappers | nothing | small |
| 2 | Incoming session support | phase 1 | medium |
| 3 | Call API | phases 1, 2 | medium |
| 4 | Room participant model | phases 1, 2 | medium-large |
| 5 | Polish, deprecation, examples | phases 3, 4 | small |

---

## Phase 1: Types + Broadcast Layer

**Goal**: Introduce the `types`, `broadcast`, and `transport` modules. These are thin
wrappers around existing types. No behavior changes.

### 1.1 Shared types (`iroh-live/src/types.rs`)

New file. Contains:

- `ParticipantId` — newtype over `EndpointId`
  - Derive: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize`
  - `Display` (short hex), `From<EndpointId>`
- `PublicationId` — struct `{ participant: ParticipantId, name: String }`
  - Derive: `Debug, Clone, PartialEq, Eq, Hash`
  - `Display` → `"participant:name"`
- `RoomId` — newtype over gossip `TopicId`
  - Derive: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize`
- `DisconnectReason` — enum `{ Normal, Error(String), Timeout }`
- `TrackKind` — enum `{ Video, Audio }`

Implementation: pure data types, no logic beyond Display/From impls.

### 1.2 Broadcast wrappers (`iroh-live/src/broadcast.rs`)

New file. Wraps existing moq-media types:

**`LocalBroadcast`**:
- Wraps `moq_media::publish::PublishBroadcast` in an `Arc`
- Methods delegate directly:
  - `new()` → `PublishBroadcast::new()`
  - `video()` / `audio()` return lightweight slot handles
  - `LocalVideoSlot::publish_camera()` / `publish_screen()` build renditions and call inner `set_video(Some(...))`
  - `LocalVideoSlot::replace_source()` preserves the logical video slot while swapping the underlying source
  - `LocalVideoSlot::clear()` → inner `set_video(None)`
  - `LocalAudioSlot::publish_microphone()` builds renditions and calls inner `set_audio(Some(...))`
  - `LocalAudioSlot::replace_source()` preserves the logical audio slot while swapping the underlying source
  - `LocalAudioSlot::clear()` → inner `set_audio(None)`
  - `preview(config)` → inner `watch_local(config)`
  - `producer()` → inner `producer()`
- Add `Clone` (Arc-based)

**Source replacement semantics**:
- camera A → camera B keeps the same logical video slot
- microphone A → microphone B keeps the same logical audio slot
- `clear()` removes the slot entirely and tears down active encoders
- mute/disable should not require full teardown
- the broadcast layer preserves slots; the product layer decides whether a semantic source change maps to a new publication identity

**`RemoteBroadcast`**:
- Wraps `moq_media::subscribe::SubscribeBroadcast` in an `Arc` (all methods take `&self`)
- Methods delegate:
  - `status()` → new Watchable tracking connect/live/ended
  - `catalog()` → inner `catalog_watcher()`
  - `catalog_snapshot()` → inner `catalog()`
  - `name()` → inner `broadcast_name()`
  - `has_video()` / `has_audio()` derived from catalog
  - `subscribe_video<D>()` → inner `watch::<D>()`
  - `subscribe_video_with<D>(opts)` → inner `watch_with::<D>(config, quality)` or `watch_rendition::<D>(config, name)`
  - `subscribe_audio<D>(backend)` → inner `listen::<D>(backend)`
  - `video_renditions()` → iterate catalog
  - `audio_renditions()` → iterate catalog
  - `selected_video()` / `selected_audio()` retain the last successful selection snapshot for UI and debugging
  - `closed()` → inner `closed()`
  - `as_inner()` → access inner

**Option types** (same file):
- `BroadcastTicket` — struct `{ remote: EndpointAddr, broadcast_name: String }`, Display/FromStr
- `BroadcastStatus` — enum `{ Connecting, Live, Ended }`
- `SubscribeVideoOptions` — struct with `quality`, `rendition`, `viewport`, `decode_config`. Builder methods.
- `SubscribeAudioOptions` — struct with `quality`, `rendition`. Builder methods.
- `SelectedVideo` / `SelectedAudio` — snapshots of the currently selected rendition and options

**Key decision**: `SubscribeBroadcast` methods already take `&self` (catalog, watch,
listen, etc.), so `RemoteBroadcast` can use `Arc<SubscribeBroadcast>` directly — no
Mutex needed. `PublishBroadcast::set_video/set_audio` take `&mut self`, so
`LocalBroadcast` needs `Arc<Mutex<PublishBroadcast>>` (or refactor to `&self` later).

### 1.3 Transport re-exports (`iroh-live/src/transport.rs`)

New file. Re-exports:
```rust
pub use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};
pub mod raw {
    pub use moq_lite::{BroadcastConsumer, BroadcastProducer, Track, TrackProducer};
}
```

### 1.4 Wire up in lib.rs

- Add `pub mod broadcast;`
- Add `pub mod transport;`
- Add `pub mod types;`
- Add re-exports: `pub use types::{ParticipantId, PublicationId, RoomId, TrackKind, DisconnectReason};`
- Keep all existing exports (no breaking changes)

### 1.5 Add broadcast example

New example `iroh-live/examples/broadcast.rs`:
- Publisher creates `LocalBroadcast`, sets video, publishes via `Live`
- Subscriber creates `RemoteBroadcast`, subscribes to video, prints frame count
- Uses new types throughout

### 1.6 Tests

- Unit tests for type Display/FromStr/From impls
- Integration test: `LocalBroadcast` → transport → `RemoteBroadcast` → subscribe video
- Verify `Clone` works (Arc semantics)

### Done when

- `cargo check --workspace --all-features` passes
- `cargo test --workspace` passes
- `cargo clippy --workspace --all-features` clean
- New example runs

---

## Phase 2: Incoming Session Support

**Goal**: Add a public stream of incoming MoQ sessions at the transport layer, then
wrap it at the Live layer.

### 2.1 Transport-level incoming stream (`iroh-moq`)

Changes to `iroh-moq/src/lib.rs`:

**New type `IncomingSession`**:
```rust
pub struct IncomingSession {
    remote_id: EndpointId,
    session: MoqSession, // already accepted at transport level
}

impl IncomingSession {
    pub fn remote_id(&self) -> EndpointId { ... }
    pub fn into_session(self) -> MoqSession { ... }
}
```

**Modify actor to surface incoming sessions**:
- Add a new channel `incoming_tx: mpsc::Sender<IncomingSession>`
- In `MoqProtocolHandler::handle_connection`, after `MoqSession::session_accept()`,
  send to both the actor (for internal tracking) and the incoming channel
- New method `Moq::incoming() -> mpsc::Receiver<IncomingSession>`

**Design note**: The current protocol handler auto-accepts at the QUIC level and then
does MoQ session negotiation. We keep that — the "incoming" stream surfaces *after*
MoQ handshake completes. True pre-accept inspection (before MoQ handshake) can come
later if needed.

> codex: This is a sensible first cut, but we should document the semantic limitation clearly: "accept/reject" here means application-level acceptance after MoQ setup, not transport-level refusal before setup cost is paid.

### 2.2 Wire into Live

Add to `Live`:
```rust
pub fn incoming_sessions(&self) -> impl Stream<Item = IncomingSession> { ... }
```

This is the raw escape hatch. The product-level `incoming_calls()` comes in Phase 3.

### 2.3 Tests

- Integration test: peer A connects to peer B, B receives IncomingSession
- Verify remote_id matches
- Verify session is usable (subscribe/publish work)

### Done when

- `Moq::incoming()` works
- `Live::incoming_sessions()` works
- Existing behavior unchanged (auto-accept still works for rooms)

---

## Phase 3: Call API

**Goal**: Add `Call`, `IncomingCall`, `CallTicket` on top of Phase 2.

### 3.1 Call types (`iroh-live/src/call.rs`)

New file:

**`CallTicket`**:
- Struct `{ remote: EndpointAddr }`
- `Display`/`FromStr` (base32 encoded)
- `From<EndpointAddr>`

**`CallState`**:
- Enum `{ Connecting, Connected, Disconnected(DisconnectReason) }`

**`Call`**:
- Inner: `Arc<CallInner>` containing:
  - `MoqSession`
  - `Watchable<CallState>`
  - `LocalBroadcast` (for outgoing media)
  - `RemoteBroadcast` (for incoming media, created lazily on first subscription)
- Methods:
  - `remote_participant() -> ParticipantId`
  - `local() -> &LocalBroadcast`
  - `remote() -> &RemoteBroadcast`
  - `state() -> impl Watcher<Value = CallState>`
  - `hangup()`

**Construction** (outbound):
- `Live::call(ticket)` → `MoqSession::connect()` → wrap in `Call`
- `Call` publishes the local broadcast on the session
- `Call` subscribes to the first announced broadcast from remote

**`IncomingCall`**:
- Wraps `IncomingSession` from Phase 2
- `remote_participant() -> ParticipantId`
- `accept(self) -> Result<Call>` — consumes self, creates Call
- `reject(self, reason)` — consumes self, closes connection
- `Drop` → auto-reject if not consumed

### 3.2 Wire into Live

- `Live::call(impl Into<CallTicket>) -> Result<Call>`
- `Live::accept_call() -> Result<IncomingCall>` — socket-style, awaits next incoming call
  - Internally reads from an mpsc channel fed by the incoming session handler
  - Returns `Err` when Live is shut down

### 3.3 Update example

New or updated example `iroh-live/examples/call.rs`:
- Peer A calls peer B
- Peer B accepts
- Both publish camera + mic
- Both subscribe to remote video + audio
- Uses new Call API throughout

### 3.4 Tests

- Outbound call connects and returns Call
- Inbound call surfaces as IncomingCall
- Accept → Call works, media flows
- Reject → connection closes
- Drop IncomingCall → auto-reject
- CallState transitions: Connecting → Connected → Disconnected

### Done when

- Call example works end-to-end
- All tests pass
- IncomingCall auto-reject on drop works

---

## Phase 4: Room Participant Model

**Goal**: Redesign `Room` to expose participants, publications, and structured events.

> codex: This phase is where API quality will be won or lost. If participant and publication identities remain stringly or ephemeral here, the top layer will still feel improvised even if the names improve.
The current room actor stays but gets a new public face.

### 4.1 Participant types (in `iroh-live/src/room.rs`)

**`LocalParticipant`**:
- Inner: `Arc<LocalParticipantInner>` containing:
  - `ParticipantId`
  - `LocalBroadcast`
  - `Vec<LocalTrackPublication>` (tracked)
- Methods:
  - `id() -> ParticipantId`
  - `broadcast() -> &LocalBroadcast` (escape hatch)
  - `publish_video(source, encoder, presets) -> Result<LocalTrackPublication>`
  - `publish_audio(source, presets) -> Result<LocalTrackPublication>`
  - `publications() -> Vec<LocalTrackPublication>`

**`LocalTrackPublication`**:
- Inner: `Arc<_>` with `PublicationId`, `TrackKind`, mute state
- Methods: `id()`, `kind()`, `set_muted()`, `is_muted()`, `unpublish(self)`

**`RemoteParticipant`**:
- Inner: `Arc<RemoteParticipantInner>` containing:
  - `ParticipantId`
  - `RemoteBroadcast`
  - `Vec<RemoteTrackPublication>` (derived from catalog)
- Methods:
  - `id() -> ParticipantId`
  - `broadcast() -> &RemoteBroadcast` (escape hatch)
  - `publications() -> Vec<RemoteTrackPublication>`
  - `publication(name) -> Option<RemoteTrackPublication>`

**`RemoteTrackPublication`**:
- Inner: `Arc<_>` with `PublicationId`, `TrackKind`, reference to `RemoteBroadcast`
- Methods:
  - `id()`, `kind()`, `participant()`
  - `subscribe_video() -> Result<VideoTrack>`
  - `subscribe_video_with(opts) -> Result<VideoTrack>`
  - `subscribe_audio(backend) -> Result<AudioTrack>`
  - `subscribe_audio_with(opts, backend) -> Result<AudioTrack>`

### 4.2 Redesign RoomEvent

New `RoomEvent` enum:
```rust
pub enum RoomEvent {
    ParticipantJoined(RemoteParticipant),
    ParticipantLeft { participant: ParticipantId, reason: DisconnectReason },
    TrackPublished { participant: ParticipantId, publication: RemoteTrackPublication },
    TrackUnpublished { participant: ParticipantId, publication_id: PublicationId },
}
```

### 4.3 Modify Room actor

The existing room actor (`iroh-live/src/rooms.rs`) needs to:

1. **Track participants** — maintain `HashMap<ParticipantId, RemoteParticipant>`
2. **Derive publications from catalogs** — when a catalog update arrives, diff against
   known publications and emit `TrackPublished`/`TrackUnpublished` events
3. **Map old events to new events**:
   - `RemoteAnnounced` → no direct event (internal bookkeeping)
   - `RemoteConnected` → create `RemoteParticipant` (wrapping session), emit `ParticipantJoined`
   - `BroadcastSubscribed` → create `RemoteBroadcast`, attach to participant,
     derive publications from catalog, emit `TrackPublished` for each

4. **Catalog diffing**: When a catalog update arrives (via `catalog_watcher()`), diff
   old vs new to emit `TrackPublished`/`TrackUnpublished`:
   ```rust
   fn diff_catalog(old: &Catalog, new: &Catalog) -> (Vec<TrackPublished>, Vec<TrackUnpublished>) {
       // Compare video/audio BTreeMap keys. New keys → TrackPublished. Missing keys → TrackUnpublished.
   }
   ```
   The actor spawns a catalog watch task per RemoteBroadcast, forwarding diffs as events.

5. **Expose new Room API** while keeping old one available (deprecated)

### 4.4 New Room struct

```rust
pub struct Room {
    inner: Arc<RoomInner>,
}

struct RoomInner {
    id: RoomId,
    local: LocalParticipant,
    participants: RwLock<HashMap<ParticipantId, RemoteParticipant>>,
    event_tx: broadcast::Sender<RoomEvent>,
    _actor: AbortOnDropHandle<()>,
}
```

Methods:
- `id() -> RoomId`
- `ticket() -> RoomTicket` (include self as bootstrap)
- `local_participant() -> &LocalParticipant`
- `recv() -> Result<RoomEvent>` — socket-style, awaits next event
- `remote_participants() -> HashMap<ParticipantId, RemoteParticipant>`
- `remote_participant(id) -> Option<RemoteParticipant>`
- `leave()`

### 4.5 Migration

- New Room wraps old room actor internally
- Old `RoomEvent` types (`RemoteAnnounced`, etc.) become internal
- Old `Room::recv()` / `Room::split()` deprecated with doc pointer to new API
- `RoomTicket` stays (same concept, maybe extend fields)

### 4.6 Update room example

Update `iroh-live/examples/rooms.rs` to use new API:
- `live.join_room(ticket)` instead of `Room::new(endpoint, gossip, live, ticket)`
- Event loop uses new `RoomEvent` variants
- Subscribe through `RemoteTrackPublication` instead of `BroadcastSubscribed`

### 4.7 Tests

- ParticipantJoined fires when peer connects
- ParticipantLeft fires on disconnect
- TrackPublished fires when catalog arrives
- Publications are accessible on RemoteParticipant
- Subscribe through publication works
- LocalParticipant publish_video/audio creates publications

### Done when

- Room example works with new API
- All room-related tests pass
- Old API still compiles (deprecated)

---

## Phase 5: Polish

**Goal**: Clean up, deprecate, document.

### 5.1 Deprecations

Mark with `#[deprecated(since = "0.x", note = "use ... instead")]`:
- `Live::connect()` → "use `Live::call()` or `Live::connect_raw()`"
- `Live::connect_and_subscribe()` → "use `Live::call()` or `Live::subscribe_broadcast()`"
- `Live::watch_and_listen()` → "use `Call::remote()` or `RemoteBroadcast::subscribe_*()`"
- `Live::publish()` → "use `LocalBroadcast::producer()` or `LocalParticipant::publish_*()`"
- `pub moq` field on `Live` → make private, add `Live::transport() -> &Moq`
- Old `RoomEvent` variants
- `Room::recv()` / `Room::split()`
- `RoomPublisherSync`

### 5.2 Prelude

Add `iroh-live/src/prelude.rs`:
```rust
pub use crate::{Live, Room, RoomEvent, Call, IncomingCall};
pub use crate::room::{LocalParticipant, RemoteParticipant};
pub use crate::room::{LocalTrackPublication, RemoteTrackPublication};
pub use crate::broadcast::{LocalBroadcast, RemoteBroadcast, BroadcastTicket};
pub use crate::call::CallTicket;
pub use crate::room::RoomTicket;
pub use crate::types::{ParticipantId, TrackKind};
```

### 5.3 Error types

Introduce structured errors for the product layer:
- `CallError` — connection failed, rejected, already connected
- `RoomError` — join failed, already in room, gossip failure
- `PublishError` — no source, codec failure, already published
- `SubscribeError` — rendition not found, decode failure, not available

These wrap the lower-level errors from iroh-moq and moq-media.

### 5.4 Documentation pass

- Module-level docs (`//!`) for each new module
- Type-level docs for all public types
- Method docs following RFC 1574 style
- Add doc examples for key types (Room, Call, LocalBroadcast, RemoteBroadcast)

### 5.5 Example suite

Update all examples to use new API:
- `examples/call.rs` — one-to-one call (new)
- `examples/broadcast.rs` — one-to-many streaming (new)
- `examples/rooms.rs` — multi-party room (updated)
- `examples/publish.rs` — keep as low-level example using transport layer
- `examples/watch.rs` — keep as low-level example

### Done when

- All examples compile and work
- No clippy warnings
- `cargo doc --workspace` builds cleanly
- Deprecated items have clear migration pointers

---

## Implementation Notes

### Interior mutability strategy

All Arc-based handles need interior mutability for state. Choices:

| Type | Strategy | Reason |
|------|----------|--------|
| `LocalBroadcast` | `Arc<Mutex<PublishBroadcast>>` | `set_video`/`set_audio` take `&mut self` |
| `RemoteBroadcast` | `Arc<SubscribeBroadcast>` | All methods already take `&self` |
| `Room` | Actor + `RwLock<HashMap>` for participants | Concurrent reads common |
| `Call` | `Watchable<CallState>` + Arc refs | Mostly reads + state transitions |
| `LocalParticipant` | `Arc<Mutex<Vec<LocalTrackPublication>>>` | Rare writes |
| `RemoteParticipant` | `Arc<RwLock<Vec<RemoteTrackPublication>>>` | Reads from catalog updates |

Future optimization: refactor `PublishBroadcast::set_video/set_audio` to use `&self`
internally (state is already behind internal Arc/Mutex). This would let
`LocalBroadcast` drop its Mutex.

### Thread safety

All public types must be `Send + Sync`. This is automatic with Arc + Mutex/RwLock.
Verify with:
```rust
fn assert_send_sync<T: Send + Sync>() {}
assert_send_sync::<Room>();
assert_send_sync::<Call>();
// etc.
```

### Backward compatibility

During the transition:
- Old `Room::new()` still works but is deprecated
- Old `Live::connect()` still works but is deprecated
- Old `RoomEvent` variants still exist but are deprecated
- New and old APIs can coexist in the same binary

After 1-2 releases, remove deprecated items in a semver-major bump.

### What NOT to change

- `moq-media` internals — keep as-is, it's stable
- `iroh-moq` protocol behavior — only add incoming stream
- `web-transport-iroh` — no changes
- `hang` / `moq-lite` — no changes
- Codec implementations — no changes
- Pipeline architecture — no changes

### Open questions

1. **Should `Call` auto-subscribe to remote broadcasts?** Currently the plan says yes
   (subscribe to first announced broadcast). But should it be opt-in? Consider
   audio-only calls or custom subscription logic.

2. **Should `RemoteBroadcast` cache subscriptions?** If you call `subscribe_video()`
   twice, should it return the same `VideoTrack` or create a new one? Current plan:
   create new one each time (caller manages lifetime).

3. **Should `Room` auto-subscribe to all participants?** Current room behavior is
   auto-subscribe. New API could make this configurable via `RoomOptions`.

4. **`PublicationId` stability**: is `(participant, track_name)` stable enough across
   reconnects? If a participant disconnects and reconnects, their publications get new
   session state but same logical identity.

5. **Should `LocalBroadcast` be created via `Live::create_broadcast()` or
   `LocalBroadcast::new()`?** The sketch shows both. Preference: `new()` for standalone,
   `Live` method for transport-attached.
