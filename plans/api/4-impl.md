# Implementation Plan

Step-by-step plan for implementing the three-layer API redesign. Each phase produces
a working codebase — no phase leaves things broken.

See `0-overview.md` for the design, `3-sketch.md` for the target API shape,
`3a-glossary.md` for term definitions.

## Principles

- **Each phase compiles and passes tests.** No "we'll fix it in the next phase."
- **No backwards compatibility.** This is a monorepo. Port all examples and apps
  in the same phase that changes the API. Remove old types immediately — no
  deprecation period.
- **Keep moq-media unchanged where possible.** Exceptions: rename `WatchTrack` →
  `VideoTrack` (Phase 1), and change `SubscribeBroadcast::catalog_watcher` to
  `&self` if feasible (Phase 1).
- **Only wrap when adding state.** See justification table in `3-sketch.md`.

## Current Codebase Mapping

What exists today and what it becomes:

| Current type | Location | Becomes |
|-------------|----------|---------|
| `Live` | `iroh-live/src/live.rs` | Redesigned `Live` (builder, call, room, broadcast methods) |
| `LiveNode` | `iroh-live/src/node.rs` | **Removed.** `LiveBuilder::spawn()` absorbs its role. |
| `LiveTicket` | `iroh-live/src/ticket.rs` | Renamed to `BroadcastTicket` (same fields: endpoint + broadcast_name) |
| `Room` | `iroh-live/src/rooms.rs` | Redesigned `Room` with participant model |
| `RoomHandle` | `iroh-live/src/rooms.rs` | Internal to new `Room` (not public) |
| `RoomEvent` | `iroh-live/src/rooms.rs` | Redesigned: `BroadcastPublished`/`BroadcastUnpublished` replace raw events |
| `RoomPublisherSync` | `iroh-live/src/rooms/publisher.rs` | **Removed.** Replaced by `LocalParticipant::broadcast(kind)` + slot API. |
| `StatsSmoother` | `iroh-live/src/util.rs` | Kept as-is |
| `Moq` | `iroh-moq/src/lib.rs` | Kept. Add `accept_session()` method. |
| `MoqProtocolHandler` | `iroh-moq/src/lib.rs` | Kept. Split accept into actor + incoming channel. |
| `MoqSession` | `iroh-moq/src/lib.rs` | Kept. Already `Clone`. Used internally by Call and Room. |
| `PublishBroadcast` | `moq-media/src/publish.rs` | Kept. Wrapped by `LocalBroadcast`. |
| `SubscribeBroadcast` | `moq-media/src/subscribe.rs` | Kept. Wrapped by `RemoteBroadcast`. |
| `WatchTrack` | `moq-media/src/subscribe.rs` | Renamed to `VideoTrack`. |
| `AudioTrack` | `moq-media/src/subscribe.rs` | Kept as-is. |
| `AvRemoteTrack` | `moq-media/src/subscribe.rs` | **Removed** in Phase 5. Use `VideoTrack` + `AudioTrack` separately. |

New types not in current codebase:

| New type | Location | Purpose |
|----------|----------|---------|
| `Call`, `IncomingCall`, `CallTicket` | `iroh-live/src/call.rs` | One-to-one call API |
| `LocalParticipant`, `RemoteParticipant` | `iroh-live/src/room.rs` | Participant model |
| `LocalBroadcast`, `RemoteBroadcast` | `iroh-live/src/broadcast.rs` | Broadcast wrappers |
| `LocalVideoSlot`, `LocalAudioSlot` | `iroh-live/src/broadcast.rs` | Slot API |
| `BroadcastKind`, `ParticipantId`, `RoomId` | `iroh-live/src/types.rs` | Shared IDs |
| `LiveBuilder` | `iroh-live/src/live.rs` | Builder for Live |
| `CallState`, `DisconnectReason` | `iroh-live/src/types.rs` | State enums |
| `BroadcastTicket`, `BroadcastStatus` | `iroh-live/src/broadcast.rs` | Broadcast layer types |

## Phase Overview

| Phase | What | Depends on | Scope |
|-------|------|------------|-------|
| 1 | moq-media rename + shared types + broadcast wrappers + LiveBuilder | nothing | large |
| 2 | Incoming session support in iroh-moq | Phase 1 | small |
| 3 | Call API + call example | Phases 1, 2 | medium |
| 4 | Room participant model + room migration | Phases 1, 2 | large |
| 5 | Port all examples/apps, remove old API, polish | Phases 3, 4 | medium |

---

## Phase 1: Types + Broadcast Layer

**Goal**: Rename `WatchTrack`, introduce shared types, broadcast wrappers, transport
re-exports, and `LiveBuilder`. No behavior changes to existing code yet (additive only,
old API still works).

### 1.1 Rename `WatchTrack` → `VideoTrack` (moq-media)

In `moq-media/src/subscribe.rs`:
- Rename struct `WatchTrack` → `VideoTrack`
- Rename all internal references (construction sites, type aliases)
- Update `moq-media/src/lib.rs` re-exports
- Update all downstream usage in `iroh-live` and examples

Also in `moq-media/src/publish.rs`:
- `PublishBroadcast::watch_local()` returns `WatchTrack` → rename to `VideoTrack`

**Files touched**: `moq-media/src/subscribe.rs`, `moq-media/src/lib.rs`,
`moq-media/src/publish.rs`, `iroh-live/src/rooms/publisher.rs` (if it references
WatchTrack), all examples that use WatchTrack.

### 1.2 Consider: `SubscribeBroadcast::catalog_watcher` → `&self`

Currently `catalog_watcher(&mut self)` returns `n0_watcher::Direct<CatalogWrapper>`.
Internally, `SubscribeBroadcast` holds a `Watchable<CatalogWrapper>` and
`catalog_watcher` calls `.watch()` on it. `Watchable::watch()` takes `&self` in
n0_watcher, so this **can be changed to `&self`** with no semantic change.

If changed: `RemoteBroadcast` can expose `catalog()` by calling
`self.inner.subscribe.catalog_watcher()` directly — no mirror task needed.

If not changed: `RemoteBroadcast` spawns a background task during construction that
mirrors catalog updates into its own `Watchable<CatalogWrapper>`, and exposes a
watcher on that. This works but adds complexity.

**Recommendation**: Change to `&self` in moq-media. It's a one-line signature change
with zero semantic impact. Verify no downstream code relies on `&mut self` exclusivity.

### 1.3 Shared types (`iroh-live/src/types.rs`)

New file. Pure data types with no dependencies beyond iroh:

```rust
// ParticipantId — newtype over EndpointId
//   Derive: Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize
//   Impls: Display (fmt_short), From<EndpointId>

// BroadcastKind — enum { Camera, Screen, Named(String) }
//   Derive: Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize
//   Impls: Display, From<&str>, From<String>, as_str()
//   Round-trip: "camera" → Camera, "screen" → Screen, anything else → Named

// RoomId — newtype over TopicId
//   Derive: Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize
//   Impls: Display (hex)

// DisconnectReason — enum { LocalClose, RemoteClose, TransportError(Arc<dyn Error>) }
//   Derive: Debug, Clone
//   No PartialEq (Arc<dyn Error> doesn't support it)

// CallState — enum { Connecting, Connected, Disconnected(DisconnectReason) }
//   Derive: Debug, Clone
//   Helper methods: is_connecting(), is_connected(), is_disconnected()
```

### 1.4 Broadcast wrappers (`iroh-live/src/broadcast.rs`)

#### `LocalBroadcast`

Wraps `PublishBroadcast`. Inner needs `Mutex` because `set_video()`/`set_audio()` take
`&mut self`:

```rust
struct LocalBroadcastInner {
    publish: Mutex<PublishBroadcast>,
}
```

**Slot implementation**: `LocalVideoSlot` and `LocalAudioSlot` both hold
`Arc<LocalBroadcastInner>` (same inner as their parent `LocalBroadcast`). They don't
own separate state — they're thin handles that lock the `Mutex` to modify the
`PublishBroadcast`.

**`set_source` and the encoder question**: The current `VideoRenditions` API supports
two construction paths:
- `VideoRenditions::new(source, codec, presets)` — dynamic codec selection (requires
  `any_video_codec` feature)
- `VideoRenditions::new_from_generic::<E>(source, presets)` — explicit encoder type

The slot `set_source` needs to create `VideoRenditions`. Options:

**(a) Codec parameter on slot**: `set_source(source, codec, presets)` — uses the dynamic
path. Requires `any_video_codec` feature (or we make it default).

**(b) Generic encoder type on slot**: `set_source::<E: VideoEncoderFactory>(source, presets)`
— explicit but verbose.

**(c) Take pre-built renditions**: `set_source(source, renditions: VideoRenditions)` —
caller builds renditions manually. Most flexible but least ergonomic.

**(d) Codec parameter on `LocalBroadcast`**: Set the default codec at broadcast level,
slots inherit it. `LocalBroadcast::new_with_codec(VideoCodec::H264)`.

**Decision**: Option (a) as the primary API, with escape hatch (c) for advanced use.
The slot signature:
```rust
pub fn set_source(
    &self,
    source: impl VideoSource,
    codec: VideoCodec,
    presets: impl IntoIterator<Item = VideoPreset>,
) -> Result<()>
```
This maps directly to `VideoRenditions::new()`. For the generic path, users can call
`LocalBroadcast::producer()` and use `PublishBroadcast` directly.

Similarly for audio:
```rust
pub fn set_source(
    &self,
    source: impl AudioSource,
    codec: AudioCodec,
    presets: impl IntoIterator<Item = AudioPreset>,
) -> Result<()>
```

**`preview()`**: Delegates to `PublishBroadcast::watch_local()` (which returns
`Option<VideoTrack>`). Needs lock but the lock is short (watch_local clones internal
state and returns).

**`publish(name)` on `LocalBroadcast`**: The sketch shows this method, but
`LocalBroadcast` doesn't know about networking. Two options:

**(a) LocalBroadcast holds a channel to Moq**: Pass during construction for Live-managed
broadcasts. `publish()` sends the producer through the channel.

**(b) Separate publish step on Live**: `live.publish_broadcast("name", &broadcast)`.
LocalBroadcast is purely local; publishing is a Live concern.

**Decision**: Option (b). `LocalBroadcast` is a pure media wrapper. Publishing happens
through:
- Standalone: `let ticket = live.publish_broadcast("name", &broadcast)?`
- Room: room actor calls `moq.publish()` with the producer
- Call: call code calls `session.publish()` with the producer

This means `LocalBroadcast` has no `publish()` method. Publishing happens through
`Live::publish_broadcast()` instead. The `producer()` escape hatch is the bridge.

#### `RemoteBroadcast`

Wraps `SubscribeBroadcast`. If we change `catalog_watcher` to `&self` (§1.2), the
inner is simpler:

```rust
struct RemoteBroadcastInner {
    subscribe: SubscribeBroadcast,
    status: Watchable<BroadcastStatus>,
    selected_video: Mutex<Option<SelectedVideo>>,
    selected_audio: Mutex<Option<SelectedAudio>>,
    _status_task: AbortOnDropHandle<()>,  // watches closed() → updates status
}
```

No catalog mirror task needed — `catalog()` calls `subscribe.catalog_watcher()`
directly.

**Status tracking**: Spawn one task during construction:
1. Start as `Connecting`
2. Watch `subscribe.catalog_watcher()` — first update → transition to `Live`
3. Watch `subscribe.closed()` — resolves → transition to `Ended`

**`subscribe_video_with`**: Delegates to `SubscribeBroadcast::watch_rendition()` or
`watch_with()` depending on options. Then updates `selected_video`. Returns `VideoTrack`.

**`subscribe_audio_with`**: Delegates to `SubscribeBroadcast::listen_rendition()` or
`listen_with()`. Updates `selected_audio`. Returns `AudioTrack`.

**`has_video()`/`has_audio()`**: Check `subscribe.catalog().video.is_empty()` /
`subscribe.catalog().audio.is_empty()`. These read the current catalog snapshot — no
lock needed since `catalog()` returns owned `CatalogWrapper`.

#### `BroadcastTicket`

Rename of existing `LiveTicket`. Same fields: `endpoint: EndpointAddr` +
`broadcast_name: String`. Keep the existing `serialize()`/`deserialize()` logic but
rename the type and update the format string prefix if it has one.

#### Option types

Pure data, no implementation complexity:
- `SubscribeVideoOptions` — quality, rendition, viewport, decode_config (builder)
- `SubscribeAudioOptions` — quality, rendition (builder)
- `SelectedVideo` — rendition name + options snapshot
- `SelectedAudio` — rendition name + options snapshot
- `BroadcastStatus` — enum { Connecting, Live, Ended }

### 1.5 Transport re-exports (`iroh-live/src/transport.rs`)

```rust
pub use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};

pub mod raw {
    pub use moq_lite::{BroadcastConsumer, BroadcastProducer, Track, TrackProducer};
}
```

### 1.6 LiveBuilder (`iroh-live/src/live.rs`)

Absorbs `LiveNode`'s role. The current `LiveNode::spawn_from_env()` does:
1. Read `IROH_SECRET` env var (or generate key)
2. Build & bind `Endpoint` with ALPN
3. Create `Gossip`
4. Create `Live::new(endpoint)`
5. Create `Router`, register gossip + moq protocol handlers
6. Spawn router
7. Return `LiveNode { router, live, gossip }`

`LiveBuilder::spawn()` does the same but returns a `Live` that owns everything:

```rust
struct LiveInner {
    endpoint: Endpoint,
    moq: Moq,
    gossip: Gossip,
    _router: Router,
}
```

**`Live::new(endpoint, gossip)`**: Manual construction for users who manage their own
endpoint/gossip. Creates `Moq` internally but doesn't spawn a router — caller is
responsible for registering the protocol handler.

### 1.7 Wire up in lib.rs

- Add new modules: `pub mod broadcast`, `pub mod transport`, `mod types`
- Add re-exports for new types
- Remove old exports (`LiveNode`, `LiveTicket`, `Live::connect`, `Live::connect_and_subscribe`,
  `Live::watch_and_listen`, `Live::publish`, `pub moq` field) in this phase

### 1.8 Tests

- `BroadcastKind` round-trip: `From<&str>` ↔ `as_str()` for "camera", "screen", custom
- `ParticipantId` Display/Eq
- `LocalBroadcast`: create, set video source via slot, verify `has_video()`
- `RemoteBroadcast`: create from `SubscribeBroadcast`, verify status transitions
  (Connecting → Live on first catalog → Ended on close)
- `BroadcastTicket`: Display/FromStr round-trip
- `LiveBuilder::spawn()`: verify it produces a working `Live`
- Integration: `LocalBroadcast` → publish via `live.publish_broadcast()` → connect →
  `RemoteBroadcast` → subscribe_video → get frames

### Done when

- `cargo check --workspace --all-features --tests --examples` passes
- `cargo test --workspace` passes
- `cargo clippy --locked --workspace --all-targets --all-features` clean
- All examples ported to new types

---

## Phase 2: Incoming Session Support

**Goal**: Add the ability to receive and distinguish incoming MoQ sessions.
Required for the Call API (Phase 3). Small, focused change in iroh-moq.

### 2.1 Transport-level incoming stream (`iroh-moq`)

**Current behavior**: `MoqProtocolHandler::accept()` receives connections, does the MoQ
handshake, and sends `ActorMessage::HandleSession { session }` to the actor. The actor
auto-publishes all local broadcasts to every new session.

**Change**: After the MoQ handshake, *also* send the session to an incoming channel:

```rust
// New field in Moq's actor:
incoming_tx: mpsc::Sender<IncomingSession>,

// In MoqProtocolHandler::handle_connection():
let session = MoqSession::session_accept(wt_session).await?;
// Send to actor (existing — for broadcast distribution)
self.tx.send(ActorMessage::HandleSession { session: session.clone() }).await?;
// Also send to incoming channel (new — for Call API)
let _ = self.incoming_tx.send(IncomingSession { session: session.clone() });
```

Since `MoqSession` is `Clone`, this is straightforward.

```rust
// New public type
pub struct IncomingSession {
    session: MoqSession,
}

impl IncomingSession {
    pub fn remote_id(&self) -> EndpointId {
        self.session.conn().remote_id()
    }
    pub fn session(&self) -> &MoqSession { &self.session }
    pub fn into_session(self) -> MoqSession { self.session }
}

// New method on Moq
impl Moq {
    pub async fn accept_session(&self) -> Result<IncomingSession> { ... }
}
```

**Note**: The actor continues to handle all sessions (publishing local broadcasts to
them). The incoming channel is an *additional* notification that lets product code
(Call, accept_call) act on specific sessions. This is not either/or — both paths fire.

### 2.2 Wire into Live

```rust
impl Live {
    /// Receives the next incoming MoQ session. Used internally by accept_call().
    pub(crate) async fn accept_session(&self) -> Result<IncomingSession> {
        self.inner.moq.accept_session().await
    }
}
```

Keep `pub(crate)` — it's a building block for `accept_call()`, not a public API.

### 2.3 Tests

- Integration: peer A connects to peer B → B receives via `moq.accept_session()`
- Verify `remote_id()` matches A's endpoint ID
- Verify the session is usable (subscribe/publish work after accept)
- Verify existing room behavior still works (rooms use gossip-initiated outbound
  connections, not the incoming channel)
- Verify the actor still auto-publishes to the new session (both paths fire)

### Done when

- `Moq::accept_session()` works
- Existing tests pass unchanged
- No regressions

---

## Phase 3: Call API

**Goal**: Add `Call`, `IncomingCall`, `CallTicket`. One-to-one media sessions.

### 3.1 Call signaling design

MoQ has no "call" concept. A MoQ session is a bidirectional media pipe. We layer call
semantics on top using naming conventions.

**Design**: A call is a MoQ session where both sides publish broadcasts with a shared
`call_id` prefix:
- For `call_id = "abc123"`:
  - Camera → `"abc123/camera"`
  - Screen → `"abc123/screen"`
  - Custom `"foo"` → `"abc123/foo"`

Both sides subscribe to the other's broadcasts by watching announces with the call_id
prefix.

### 3.2 Key implementation insight: `OriginConsumer` announce watching

The MoQ transport already has exactly the announce-watching API we need:

```rust
// In moq-lite OriginConsumer:
async fn announced(&mut self) -> Option<(PathOwned, Option<BroadcastConsumer>)>
fn consume_only(&mut self, prefixes: &[&str])  // filter to prefix
fn with_root(&mut self, prefix: &str)          // strip prefix from paths
fn consume_broadcast(&self, path: &str) -> Option<BroadcastConsumer>  // lookup
```

**For Call**: Clone the `MoqSession`, access its `OriginConsumer` field (need to
check visibility — currently `subscribe` field is pub in MoqSession struct, but it's
`OriginConsumer`). Use `with_root("{call_id}/")` to create a scoped consumer that
yields announces like `"camera"`, `"screen"` with the prefix stripped.

Add `MoqSession::announced()` to iroh-moq (wraps `self.subscribe.announced()`):
```rust
/// Yields the next broadcast announced by the remote peer.
///
/// Returns `(name, Some(consumer))` for new broadcasts, `(name, None)` for
/// unannounced broadcasts. Returns `Err` when the session closes.
pub async fn announced(&mut self) -> Result<(String, Option<BroadcastConsumer>)> {
    loop {
        tokio::select! {
            res = self.subscribe.announced() => {
                let (path, consumer) = res.ok_or_else(|| ...)?;
                return Ok((path.to_string(), consumer));
            }
            reason = self.wt_session.closed() => {
                return Err(reason.into());
            }
        }
    }
}
```

This gives Call everything it needs. The Call's background task:
1. Clones the `MoqSession`
2. Loops on `session.announced()`
3. Filters for `"{call_id}/"` prefix (or we add prefix filtering later)
4. Creates `SubscribeBroadcast` → `RemoteBroadcast` for each match
5. Maps the suffix to `BroadcastKind`
6. Inserts into remotes map and sends on channel

### 3.3 Call types (`iroh-live/src/call.rs`)

**`CallTicket`**:
```rust
pub struct CallTicket {
    pub(crate) remote: EndpointAddr,
    pub(crate) call_id: String,
}
```
- `From<EndpointAddr>` generates random call_id (UUID or short random)
- `Display`/`FromStr` for sharing

**`Call`**:
```rust
struct CallInner {
    session: MoqSession,
    call_id: String,
    state: Watchable<CallState>,
    locals: RwLock<HashMap<BroadcastKind, LocalBroadcast>>,
    remotes: RwLock<HashMap<BroadcastKind, RemoteBroadcast>>,
    remote_tx: mpsc::Sender<(BroadcastKind, RemoteBroadcast)>,
    remote_rx: Mutex<mpsc::Receiver<(BroadcastKind, RemoteBroadcast)>>,
    _announce_task: AbortOnDropHandle<()>,
    _state_task: AbortOnDropHandle<()>,
}
```

**`local(kind)` — lazy creation + auto-publish**:
```rust
pub fn local(&self, kind: impl Into<BroadcastKind>) -> LocalBroadcast {
    let kind = kind.into();
    // Check if already exists
    if let Some(b) = self.inner.locals.read().get(&kind) {
        return b.clone();
    }
    // Create new
    let broadcast = LocalBroadcast::new();
    let name = format!("{}/{}", self.inner.call_id, kind.as_str());
    // Publish the producer to the session
    self.inner.session.publish(name, broadcast.producer());
    // Cache
    self.inner.locals.write().insert(kind, broadcast.clone());
    broadcast
}
```

Note: `session.publish()` takes `&self` (not `&mut self`). No mutability issue.

**`remote(kind)` / `wait_remote(kind)` / `recv_remote()`**:

`wait_remote` and `recv_remote` use separate mechanisms to avoid channel contention:

```rust
struct CallInner {
    remotes: RwLock<HashMap<BroadcastKind, RemoteBroadcast>>,
    recv_remote_rx: Mutex<mpsc::Receiver<(BroadcastKind, RemoteBroadcast)>>,
    recv_remote_tx: mpsc::Sender<(BroadcastKind, RemoteBroadcast)>,
    remote_notify: Notify,
}

// Announce task on each new broadcast:
//   1. Insert into remotes map
//   2. Send on recv_remote_tx (for recv_remote consumers)
//   3. Notify remote_notify (for wait_remote waiters)
```

```rust
pub fn remote(&self, kind: impl Into<BroadcastKind>) -> Option<RemoteBroadcast> {
    self.inner.remotes.read().get(&kind.into()).cloned()
}

pub async fn wait_remote(&self, kind: impl Into<BroadcastKind>) -> Result<RemoteBroadcast> {
    let kind = kind.into();
    loop {
        if let Some(b) = self.inner.remotes.read().get(&kind).cloned() {
            return Ok(b);
        }
        tokio::select! {
            _ = self.inner.remote_notify.notified() => continue,
            _ = self.closed() => return Err(anyhow!("call ended")),
        }
    }
}

pub async fn recv_remote(&self) -> Result<(BroadcastKind, RemoteBroadcast)> {
    let mut rx = self.inner.recv_remote_rx.lock().await;
    rx.recv().await.ok_or_else(|| anyhow!("call ended"))
}
```

`recv_remote()` is single-consumer (`Mutex<Receiver>`). `wait_remote()` uses
`Notify` + map re-check — safe for multiple concurrent waiters, cancellation-safe
(re-checks map on each wake).

### 3.4 `IncomingCall`

```rust
pub struct IncomingCall {
    session: MoqSession,
    call_id: String,
    remote_id: ParticipantId,
    first_announce: Option<(String, BroadcastConsumer)>,  // preserved for Call::new()
}
```

**How the callee discovers the call_id**: The caller connects and announces broadcasts
prefixed with `"{call_id}/"`. `Live::accept_call()` waits for the first announce from
the session to extract the prefix. The first announce's consumer is preserved in
`IncomingCall` and passed to `Call::new()` so the announce task doesn't miss it:

```rust
impl Live {
    pub async fn accept_call(&self) -> Result<IncomingCall> {
        let incoming = self.accept_session().await?;
        let mut session = incoming.into_session();
        // Wait for first announce to discover call_id
        let (name, consumer) = session.announced().await?;
        let call_id = name.split('/').next()
            .ok_or_else(|| anyhow!("invalid call: no prefix in broadcast name"))?
            .to_string();
        let remote_id = ParticipantId::from(session.conn().remote_id());
        Ok(IncomingCall {
            session,
            call_id,
            remote_id,
            first_announce: consumer.map(|c| (name, c)),
        })
    }
}
```

**`accept()` → `Call`**:
```rust
impl IncomingCall {
    pub fn accept(self) -> Call {
        Call::new(self.session, self.call_id, self.first_announce)
    }
    pub fn reject(self) {
        self.session.close(/* reject code */);
    }
    pub fn remote_id(&self) -> ParticipantId { self.remote_id }
}
// Drop without accept → close session (auto-reject)
// Uses Option<IncomingCallInner> internally — accept/reject take it, drop checks Some.
```

`IncomingCall` uses `Option<IncomingCallInner>` internally. `accept()` takes it via
`.take()`, `reject()` takes it, `Drop` checks if it's still `Some` (auto-rejects).
This is already reflected in the sketch.

### 3.5 Announce task for Call

When a `Call` is created (either outbound or inbound-accepted), spawn the announce
watching task:

```rust
async fn announce_watch_task(
    mut session: MoqSession,  // cloned
    call_id: String,
    remotes: Arc<RwLock<HashMap<BroadcastKind, RemoteBroadcast>>>,
    recv_remote_tx: mpsc::Sender<(BroadcastKind, RemoteBroadcast)>,
    remote_notify: Arc<Notify>,
    state: Watchable<CallState>,
) {
    // Set state to Connected once we're running
    state.set(CallState::Connected);

    loop {
        match session.announced().await {
            Ok((name, Some(consumer))) => {
                // Check if this is our call's broadcast
                let suffix = match name.strip_prefix(&format!("{}/", call_id)) {
                    Some(s) => s,
                    None => continue,  // not our call
                };
                let kind = BroadcastKind::from(suffix);
                // Create SubscribeBroadcast → RemoteBroadcast
                let sub = SubscribeBroadcast::new(name.clone(), consumer).await;
                let sub = match sub {
                    Ok(s) => s,
                    Err(e) => { warn!("failed to subscribe: {e}"); continue; }
                };
                let broadcast = RemoteBroadcast::new(sub);
                remotes.write().insert(kind.clone(), broadcast.clone());
                let _ = recv_remote_tx.send((kind.clone(), broadcast)).await;
                remote_notify.notify_waiters();
            }
            Ok((name, None)) => {
                // Broadcast unannounced — remove from map
                if let Some(suffix) = name.strip_prefix(&format!("{}/", call_id)) {
                    let kind = BroadcastKind::from(suffix);
                    remotes.write().remove(&kind);
                }
            }
            Err(_) => {
                // Session closed
                state.set(CallState::Disconnected(DisconnectReason::RemoteClose));
                break;
            }
        }
    }
}
```

**Announce stream ownership**: The Moq actor does NOT read incoming announces on
sessions — it only publishes outbound. So the Call's announce task safely owns the
announce stream for its session. Room sessions are separate (created via gossip-initiated
outbound connections), so there's no conflict. Each Call gets exclusive ownership of
its session's `announced()` stream.

### 3.6 State tracking task

Separate task that watches the QUIC connection for closure:

```rust
async fn state_watch_task(session: MoqSession, state: Watchable<CallState>) {
    let reason = session.conn().closed().await;
    // Only update if not already disconnected (announce task may have set it)
    if !state.get().is_disconnected() {
        state.set(CallState::Disconnected(
            DisconnectReason::TransportError(Arc::new(reason))
        ));
    }
}
```

Actually, the announce task already detects session closure (when `announced()` returns
Err). So we might not need a separate state task — the announce task handles it.

**Decision**: No separate state task. Announce task sets Disconnected on session close.

### 3.7 Wire into Live

```rust
impl Live {
    pub async fn call(&self, ticket: impl Into<CallTicket>) -> Result<Call> {
        let ticket = ticket.into();
        let session = self.inner.moq.connect(ticket.remote).await?;
        Ok(Call::new(session, ticket.call_id))
    }

    pub async fn accept_call(&self) -> Result<IncomingCall> {
        // See §3.4 above
    }
}
```

### 3.8 Port/create call examples

- `examples/call.rs` — simple two-peer call using Camera
- Show both initiator and acceptor sides
- May rewrite existing `publish.rs` / `watch.rs` to use Call API if they fit

### 3.9 Tests

- Outbound: `live_a.call(ticket_b)` → Call connects
- Inbound: `live_b.accept_call()` → IncomingCall surfaces, remote_id correct
- Accept → both sides can local(Camera).video().set_source() + recv_remote()
- Reject → session closes immediately
- Drop IncomingCall without accept → auto-reject (session closes)
- `recv_remote()` yields broadcasts as they appear
- `wait_remote(Camera)` resolves when camera broadcast announced
- `wait_remote(Camera)` returns immediately if already announced
- `CallState` transitions: Connecting → Connected → Disconnected
- Multi-broadcast: caller publishes Camera + Screen, callee sees both via recv_remote
- `call.close()` → state transitions to Disconnected(LocalClose), remote sees Err

### Done when

- Call example works end-to-end
- All tests pass
- IncomingCall auto-reject on drop works

---

## Phase 4: Room Participant Model

**Goal**: Redesign `Room` to expose participants, `BroadcastKind`-based broadcasts,
and structured events. Port all room examples.

### 4.1 Current Room actor behavior (reference)

The actor in `rooms.rs`:
1. Subscribes to a gossip topic via `iroh_smol_kv`
2. Each peer publishes its broadcast list to KV key `"s"` (PeerState JSON)
3. When a remote peer's KV entry arrives:
   - Emit `RoomEvent::RemoteAnnounced { remote, broadcasts }`
   - For each new broadcast: spawn `live.connect_and_subscribe(remote, name)`
4. When connect+subscribe completes:
   - Emit `RoomEvent::BroadcastSubscribed { session, broadcast }`
5. When local `room.publish(name, producer)` is called:
   - Publish directly on room sessions (see §4.4a — session-scoped, not global)
   - Update KV with new PeerState

Remove `RemoteConnected` event (defined but never emitted).

### 4.2 Strategy: wrap existing actor

Rewriting the room actor is high-risk. Instead, wrap it:
1. Create the old `Room` actor internally
2. `split()` it into `(RoomEvents, RoomHandle)`
3. Spawn an event-forwarder task that translates old events → new events
4. Expose new `Room` struct with participant model

This reuses all the gossip/KV/subscription logic unchanged.

### 4.3 New Room struct

```rust
pub struct Room {
    inner: Arc<RoomInner>,
}

struct RoomInner {
    id: RoomId,
    local: LocalParticipant,
    participants: RwLock<HashMap<ParticipantId, RemoteParticipant>>,
    event_tx: mpsc::Sender<RoomEvent>,
    event_rx: Mutex<mpsc::Receiver<RoomEvent>>,  // Mutex for &self recv()
    _room_handle: RoomHandle,  // kept alive for publishing
    _forwarder: AbortOnDropHandle<()>,
}
```

**`Room::recv(&self)`**: Locks the Mutex on event_rx and awaits. Single consumer per
call (Mutex ensures this). Multiple concurrent callers will serialize — only one gets
each event. This matches socket-style usage.

### 4.4 Participant types

**`LocalParticipant`**:
```rust
struct LocalParticipantInner {
    id: ParticipantId,
    broadcasts: RwLock<HashMap<BroadcastKind, LocalBroadcast>>,
    room_handle: RoomHandle,
}
```

**`broadcast(kind)` implementation**:
```rust
pub fn broadcast(&self, kind: impl Into<BroadcastKind>) -> LocalBroadcast {
    let kind = kind.into();
    if let Some(b) = self.inner.broadcasts.read().get(&kind) {
        return b.clone();
    }
    let broadcast = LocalBroadcast::new();
    let name = kind.as_str().to_string();
    // Publish the producer to the room (which forwards to all sessions + KV)
    let handle = self.inner.room_handle.clone();
    let producer = broadcast.producer();
    tokio::spawn(async move {
        if let Err(e) = handle.publish(name, producer).await {
            warn!("failed to publish to room: {e}");
        }
    });
    self.inner.broadcasts.write().insert(kind, broadcast.clone());
    broadcast
}
```

`room_handle.publish()` is async but `broadcast(kind)` is sync. Solved by spawning a
fire-and-forget task for the async publish. If the actor is alive it succeeds nearly
instantly. If dead, the broadcast still works locally (useful for preview). Log errors.

**Publishing order**: `broadcast(Camera)` publishes the producer immediately with an
empty catalog. Subscribers see `has_video() = false` until `set_source()` updates the
catalog. This is the simplest approach — deferring publish to first `set_source()` would
require the slot to know about the room (more coupling). Empty catalog is valid.

**`RemoteParticipant`**:
```rust
struct RemoteParticipantInner {
    id: ParticipantId,
    broadcasts: RwLock<HashMap<BroadcastKind, RemoteBroadcast>>,
    broadcast_notify: Notify,
}
```

### 4.4a Room actor: session-scoped publishing

The current room actor calls `self.live.publish(name, producer)` which goes through
the global Moq actor (leaks to all sessions). Change to session-scoped:

```rust
// In Actor:
local_producers: HashMap<String, BroadcastProducer>,  // NEW: track local producers
sessions: HashMap<EndpointId, MoqSession>,             // NEW: track room sessions

async fn handle_api_message(&mut self, msg: ApiMessage) {
    match msg {
        ApiMessage::Publish { name, producer } => {
            // Publish to all existing room sessions directly
            for session in self.sessions.values() {
                session.publish(name.clone(), producer.consume());
            }
            self.local_producers.insert(name.clone(), producer);
            self.active_publish.insert(name.clone());
            self.update_kv().await;
        }
    }
}

// When a connecting future completes with a new session:
fn register_session(&mut self, remote: EndpointId, session: MoqSession) {
    // Publish all local broadcasts to the new session
    for (name, producer) in &self.local_producers {
        session.publish(name.clone(), producer.consume());
    }
    self.sessions.insert(remote, session);
}
```

This is ~20 lines of change to the existing actor. No wrapping needed — we modify
`handle_api_message` directly. The `self.live.publish()` call is removed.

See question 15 in Open Questions for full rationale (publish scoping).

### 4.5 Event forwarder

Translates internal actor events to the public `RoomEvent` types:

```rust
async fn forward_events(
    mut old_rx: mpsc::Receiver<InternalRoomEvent>,
    new_tx: mpsc::Sender<RoomEvent>,
    participants: Arc<RwLock<HashMap<ParticipantId, RemoteParticipant>>>,
) {
    while let Some(event) = old_rx.recv().await {
        match event {
            InternalRoomEvent::RemoteAnnounced { remote, broadcasts } => {
                let pid = ParticipantId::from(remote);
                let participant = participants.write()
                    .entry(pid)
                    .or_insert_with(|| RemoteParticipant::new(pid))
                    .clone();
                let _ = new_tx.send(RoomEvent::ParticipantJoined(participant)).await;
            }
            InternalRoomEvent::BroadcastSubscribed { session, broadcast } => {
                let pid = ParticipantId::from(session.conn().remote_id());
                if let Some(participant) = participants.read().get(&pid) {
                    let name = broadcast.broadcast_name().to_string();
                    let kind = BroadcastKind::from(name.as_str());
                    let remote = RemoteBroadcast::new(broadcast);
                    participant.add_broadcast(kind.clone(), remote.clone());
                    // Spawn a closure watcher for BroadcastUnpublished
                    let close_tx = new_tx.clone();
                    let close_pid = pid;
                    let close_kind = kind.clone();
                    let close_participants = participants.clone();
                    let close_remote = remote.clone();
                    tokio::spawn(async move {
                        close_remote.closed().await;
                        if let Some(p) = close_participants.read().get(&close_pid) {
                            p.remove_broadcast(&close_kind);
                        }
                        let _ = close_tx.send(RoomEvent::BroadcastUnpublished {
                            participant: close_pid,
                            kind: close_kind.clone(),
                        }).await;
                        // If all broadcasts gone, emit ParticipantLeft
                        let should_remove = close_participants.read()
                            .get(&close_pid)
                            .map(|p| p.broadcasts().is_empty())
                            .unwrap_or(false);
                        if should_remove {
                            close_participants.write().remove(&close_pid);
                            let _ = close_tx.send(RoomEvent::ParticipantLeft {
                                participant: close_pid,
                                reason: DisconnectReason::RemoteClose,
                            }).await;
                        }
                    });
                    let _ = new_tx.send(RoomEvent::BroadcastPublished {
                        participant: pid,
                        kind,
                        broadcast: remote,
                    }).await;
                }
            }
        }
    }
}
```

Each `RemoteBroadcast` gets a closure watcher that emits `BroadcastUnpublished` when
the broadcast ends. When all of a participant's broadcasts close, `ParticipantLeft`
is inferred.

### 4.6 Wire into Live

```rust
impl Live {
    pub async fn join_room(&self, ticket: impl Into<RoomTicket>) -> Result<Room> {
        let ticket = ticket.into();
        // Create old room actor internally
        let old_room = OldRoom::new(/* endpoint, gossip, ticket */).await?;
        let (old_events, room_handle) = old_room.split();
        // Create new Room wrapping old actor
        Room::new(ticket.room_id, old_events, room_handle, self.endpoint().id())
    }
}
```

### 4.7 Port room examples

- `examples/rooms.rs` → use new Room/RoomEvent/LocalParticipant/RemoteParticipant API
- `examples/room-publish-file.rs` → use LocalParticipant::broadcast() + slot API

Example migration pattern:
```rust
// OLD:
let (mut events, handle) = room.split();
handle.publish("camera", producer).await?;
while let Ok(event) = events.recv().await {
    match event {
        RoomEvent::BroadcastSubscribed { broadcast, .. } => { ... }
    }
}

// NEW:
room.local_participant().broadcast(Camera).video().set_source(cam, codec, presets)?;
while let Ok(event) = room.recv().await {
    match event {
        RoomEvent::BroadcastPublished { participant, kind, broadcast } => {
            let video = broadcast.subscribe_video::<D>()?;
            // render video...
        }
        RoomEvent::ParticipantLeft { participant, .. } => { ... }
        _ => {}
    }
}
```

### 4.8 Tests

- `ParticipantJoined` fires when peer joins via gossip
- `BroadcastPublished` fires when remote's broadcast is subscribed
- `BroadcastUnpublished` fires when remote's broadcast closes
- `ParticipantLeft` fires on disconnect (if we have this signal)
- `RemoteParticipant::broadcast(Camera)` returns correct broadcast
- `RemoteParticipant::wait_broadcast(Screen)` resolves when screen announced
- `LocalParticipant::broadcast(Camera)` creates and caches
- Multiple participants in same room
- Participant with Camera + Screen broadcasts
- Room ticket round-trip

### Done when

- All room examples use new API
- All tests pass
- No references to old RoomEvent variants in user-facing code

---

## Phase 5: Remove Old API + Polish

**Goal**: Delete all old types, clean up, add docs.

### 5.1 Remove old types

Delete entirely (no deprecation):

| Remove | Replacement |
|--------|------------|
| `LiveNode` struct + `node.rs` | `Live::builder().spawn()` |
| `LiveTicket` struct + `ticket.rs` | `BroadcastTicket` |
| `RoomPublisherSync` + `rooms/publisher.rs` | `LocalParticipant::broadcast()` + slot API |
| `Live::connect()` | `Live::call()` or `Live::connect_raw()` |
| `Live::connect_and_subscribe()` | `Live::subscribe_broadcast()` |
| `Live::watch_and_listen()` | `RemoteBroadcast::subscribe_video/audio()` |
| `Live::publish()` | `Live::publish_broadcast()` |
| `pub moq` field on `Live` | Private. `Live::transport() -> &Moq` if needed. |
| Old `RoomEvent` variants | New participant-based events |
| `Room::split()` | Not needed (`&self` recv) |
| `RoomHandle` (public) | Internal only |
| `AvRemoteTrack` | Use `VideoTrack` + `AudioTrack` separately |
| `RoomEvents` type alias | Removed (internal mpsc) |
| Old `Room` struct (pub fields) | New `Room` (Arc-based, Clone) |

### 5.2 Update lib.rs exports

Final public surface:
```rust
// Product API (crate root)
pub use call::{Call, CallTicket, CallState, IncomingCall};
pub use live::{Live, LiveBuilder};
pub use room::{Room, RoomEvent, RoomTicket, LocalParticipant, RemoteParticipant};
pub use types::{BroadcastKind, DisconnectReason, ParticipantId, RoomId};

// Broadcast layer
pub mod broadcast;  // LocalBroadcast, RemoteBroadcast, BroadcastTicket,
                    // LocalVideoSlot, LocalAudioSlot,
                    // SubscribeVideoOptions, SubscribeAudioOptions,
                    // SelectedVideo, SelectedAudio, BroadcastStatus

// Transport layer
pub mod transport;  // MoqSession, Moq, MoqProtocolHandler, raw::*

// Media access
pub use moq_media as media;
```

### 5.3 Error types

Introduce structured errors:
- `CallError` — connection failed, rejected, timed out
- `RoomError` — join failed, gossip failure
- `BroadcastError` — publish failed, subscribe failed, rendition not found

Wrap lower-level errors from iroh-moq and moq-media.

### 5.4 Documentation

- Module-level docs (`//!`) for each module
- Type-level docs for all public types
- Method docs following RFC 1574 style (third-person declarative)
- Doc examples for key types (Live, Call, Room, LocalBroadcast, RemoteBroadcast)

### 5.5 Final test pass

- `cargo check --workspace --all-features --tests --examples`
- `cargo test --workspace --all-features`
- `cargo clippy --locked --workspace --all-targets --all-features`
- `cargo fmt --check`
- `cargo doc --workspace` — verify no broken links
- Manually run each example

### Done when

- No old types in public API
- All examples compile and work
- No clippy warnings
- Clean docs

---

## Implementation Notes

### Interior mutability strategy

| Type | Strategy | Reason |
|------|----------|--------|
| `LocalBroadcast` | `Arc<Mutex<PublishBroadcast>>` | `set_video`/`set_audio` take `&mut self` |
| `RemoteBroadcast` | `Arc` + `Watchable`s + `Mutex<Option<Selected*>>` | Status and selection are extra state |
| `Room` | Actor + `RwLock<participants>` + `Mutex<mpsc::Receiver>` | Concurrent reads, single consumer |
| `Call` | `Watchable<CallState>` + `RwLock<remotes>` + `Mutex<Receiver>` | State + concurrent map access |
| `LocalParticipant` | `Arc<RwLock<HashMap<Kind, LocalBroadcast>>>` | Lazy broadcast creation |
| `RemoteParticipant` | `Arc<RwLock<HashMap<Kind, RemoteBroadcast>>>` + `Notify` | Broadcasts arrive over time |

### Return by value (settled)

All methods returning `LocalBroadcast`, `RemoteBroadcast`, `RemoteParticipant` etc.
return owned (Arc-cloned) values, not references. Arc-clone is a pointer-width atomic
increment — cheaper than managing lock guard lifetimes.

### MoqSession mutability (resolved)

`MoqSession` is `Clone`. Cloning shares the underlying channels. The `subscribe()`
and `announced()` methods take `&mut self` because `OriginConsumer::announced()`
advances a message queue.

For Call: clone the session, give the clone to the announce task. The announce task
calls `announced()` in a loop. The main Call object uses the original session for
`publish()` (which takes `&self`). No conflict since `announced()` and `publish()`
operate on different internal channels (`OriginConsumer` vs `OriginProducer`).

**⚠️ Careful**: Two clones of MoqSession calling `announced()` concurrently WILL
race on the same mpsc channel — each announce will be delivered to exactly one of them.
This is fine for Call (only one announce task per session) but would be a problem if
we tried to share.

### What NOT to change (beyond Phase 1 renames)

- `moq-media` codec pipelines — keep as-is
- `iroh-moq` protocol wire format — no changes
- `web-transport-iroh` — no changes
- `hang` / `moq-lite` — no changes
- `rusty-codecs` encoder/decoder impls — no changes

---

## Open Questions

### Protocol-level

1. **Call signaling robustness**: The design uses broadcast name prefixes
   (`"{call_id}/camera"`) for call signaling. What happens if someone connects and
   publishes broadcasts without following the naming convention? The announce task
   ignores names without the expected prefix. The call simply sees no remote
   broadcasts. Acceptable for now — we could add connection validation later.

2. **Call ID collision**: If two callers simultaneously call the same callee with
   different call_ids, `accept_call()` picks up whichever announce arrives first.
   The second session stays connected but nobody consumes its broadcasts. Should we
   add a timeout for unclaimed sessions?

3. **Room peer departure**: The current actor doesn't explicitly emit a "peer left"
   event. The forwarder infers it from all broadcasts closing (see §4.5). This may
   need a gossip-level change or timeout heuristic for more reliable detection.

### API design

4. **`set_source` codec parameter**: Requires the `any_video_codec` feature in
   moq-media for runtime codec dispatch. Should we gate the simple slot API behind
   a feature flag and provide only `set_source::<E>(source, presets)` otherwise?

5. **`recv_remote()` / `Room::recv()` single-consumer**: Both use `Mutex<Receiver>`.
   Only one task can call these at a time. Acceptable for the loop-based pattern
   shown in examples. Multi-consumer (via `tokio::broadcast`) could be added later
   if needed for UI components that subscribe independently.

6. **Screen share lifecycle**: Screen shares are transient. Should `LocalBroadcast`
   support `unpublish()` to remove it from the room/call entirely? Currently
   `clear()` removes the source but the broadcast still exists (announced/in KV).

7. **Publishing order for rooms**: `broadcast(Camera)` publishes immediately with
   empty catalog. Remote peers see `has_video() = false` until `set_source()`.
   Document this behavior explicitly?

### Implementation

8. **Room actor rewrite vs wrap**: Phase 4 wraps the existing actor. Lower risk but
   adds a translation layer. `ParticipantLeft`/`BroadcastUnpublished` detection is
   indirect (watching broadcast closures). Plan to rewrite the actor in a future
   phase?
