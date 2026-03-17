# Implementation Plan

Step-by-step plan for the API redesign. Each phase compiles and passes tests.

See `0-overview.md` for direction, `iroh-live/examples/api_sketch.rs` for target API.

## Status Checklist

- [x] Phase 1: Rename types (moq-media)
- [x] Phase 2: Slot API — VideoPublisher/AudioPublisher, `&self` (moq-media)
- [x] Phase 3: Subscription options + status watchers (moq-media) — partial, see notes
- [x] Phase 4: Domain error types (moq-media)
- [ ] Phase 5: Relay support (moq-media)
- [x] Phase 6: Incoming session support (iroh-moq)
- [x] Phase 7: Live builder + Call + TrackName (iroh-live) — partial, see notes
- [ ] Phase 8: Room participant model + events (iroh-live)
- [x] Phase 9: Wire api_sketch.rs to real imports

### Phase 3 notes

- [x] 3.2 VideoTarget + options (VideoTarget, VideoOptions, AudioOptions, Quality)
- [x] 3.3 Status watchers — catalog_watcher(), catalog(), has_video(), has_audio(), video_renditions(), audio_renditions()
- [x] 3.4 VideoTrack improvements — current_frame(), next_frame(), set_viewport()
- [ ] 3.1 Constructor change — RemoteBroadcast takes BroadcastConsumer, not MoqSession (layering: moq-media can't depend on iroh-moq)
- [ ] 3.3 BroadcastStatus watcher — status() not yet implemented
- [ ] 3.4 VideoTrack::frames() Stream — not yet implemented

### Phase 7 notes

- [x] 7.1 LiveBuilder — builder, spawn, spawn_with_router, register_protocols
- [x] 7.2 Call — dial, accept, local(), remote(), close(), session(), closed()
- [x] 7.3 TrackName — Camera, Screen, Other(String)
- [ ] 7.4 Relay convenience — connect_relay, publish_to_relay, subscribe_from_relay

### Phase 2 notes

- [x] VideoPublisher — set(), replace(), clear(), set_enabled() (stub), renditions(), set_renditions()
- [x] AudioPublisher — set(), clear(), set_muted() (stub), set_renditions()
- [ ] set_enabled() — stub, needs encoder pipeline pause/resume
- [ ] set_muted() — stub, needs audio pipeline mute support

## Principles

- **Each phase compiles and passes tests.**
- **No backwards compat.** Old names are deleted, not deprecated. Update all callers.
- **moq-media first.** All broadcast/media improvements land there before iroh-live.
- **Commit at each green phase.**

## Phase Overview

| Phase | What | Crate | Scope |
|-------|------|-------|-------|
| 1 | Rename types | moq-media | medium |
| 2 | Slot API (VideoPublisher/AudioPublisher, `&self`) | moq-media | medium |
| 3 | Subscription options + status watchers | moq-media | medium |
| 4 | Domain error types | moq-media | small |
| 5 | Relay support | moq-media | small-medium |
| 6 | Incoming session support | iroh-moq | small |
| 7 | Live builder + Call + TrackName | iroh-live | medium |
| 8 | Room participant model + events | iroh-live | medium-large |
| 9 | Wire api_sketch.rs to real imports | all | medium |

---

## Phase 1: Rename moq-media Types

**Goal**: Get naming right. Delete old names, update all callers.

### Renames

| Old | New | File |
|-----|-----|------|
| `PublishBroadcast` | `LocalBroadcast` | `publish.rs` |
| `SubscribeBroadcast` | `RemoteBroadcast` | `subscribe.rs` |
| `CatalogWrapper` | `CatalogSnapshot` | `subscribe.rs` |
| `AvRemoteTrack` | `MediaTracks` | `subscribe.rs` |

`VideoTrack` and `AudioTrack` keep their names (already correct).

### Method renames on RemoteBroadcast

| Old | New |
|-----|-----|
| `watch()` | `video()` |
| `watch_with()` | `video_with()` |
| `watch_rendition()` | `video_rendition()` |
| `listen()` | `audio()` |
| `listen_with()` | `audio_with()` |
| `listen_rendition()` | `audio_rendition()` |
| `watch_and_listen()` | `media()` |
| `watch_local()` on LocalBroadcast | `preview()` |

### Update all callers

- moq-media internals, tests, examples
- iroh-live (rooms, live, node)
- moq-media-egui, moq-media-dioxus
- viewer examples

### Done when

- `cargo check --workspace --all-features --tests --examples` clean
- `cargo clippy --locked --workspace --all-targets --all-features` clean
- Old names completely gone

---

## Phase 2: Slot API (VideoPublisher / AudioPublisher)

**Goal**: `&self` on LocalBroadcast, sub-handles for video/audio configuration.

### 2.1 Interior mutability

Change `LocalBroadcast` internal structure to `Arc<Inner>` with mutex-protected
video/audio state. All public methods take `&self`.

### 2.2 VideoPublisher / AudioPublisher

```rust
impl LocalBroadcast {
    pub fn video(&self) -> VideoPublisher { ... }
    pub fn audio(&self) -> AudioPublisher { ... }
}
```

VideoPublisher methods: `set()`, `replace()`, `clear()`, `set_enabled()`, `renditions()`
AudioPublisher methods: `set()`, `replace()`, `clear()`, `set_muted()`

### 2.3 Convenience

- `broadcast.has_video()` / `has_audio()`
- `broadcast.preview(config)` — renamed from `watch_local()`
- `broadcast.producer()` — escape hatch to moq-lite
- `broadcast.consume()` — shorthand for `producer().consume()`

### 2.4 Remove PublishCaptureController

Superseded by slot sub-handles. Remove from public API entirely.

---

## Phase 3: RemoteBroadcast Options + Status

**Goal**: `RemoteBroadcast::new(session, name)`, VideoTarget, options, status watchers.

### 3.1 Constructor change

`RemoteBroadcast::new(session: MoqSession, name: impl Into<String>)` — calls
`session.subscribe(name)` internally. Stores session clone, accessible via `.session()`.

### 3.2 VideoTarget + options

- `VideoTarget { max_pixels, max_bitrate_kbps, rendition }` with builder methods
- `Quality` enum with `From<Quality> for VideoTarget`
- `VideoOptions { playback, target, viewport }` with builder methods
- `AudioOptions { rendition }` with builder methods

### 3.3 Status watchers (n0_watcher::Direct<T>)

- `fn status(&self) -> Direct<BroadcastStatus>` — Connecting / Live / Ended
- `fn catalog(&self) -> Direct<CatalogSnapshot>` — renditions added/removed
- Convenience: `has_video()`, `has_audio()`, `video_renditions()`, `audio_renditions()`

### 3.4 VideoTrack improvements

- `current_frame()` — latest frame snapshot (non-blocking)
- `next_frame()` — async wait for next frame
- `frames()` — `Stream` of all frames in order
- `set_viewport(w, h)` — hint for rendition auto-switching

---

## Phase 4: Domain Error Types

- `PublishError { NoVideoSource, NoAudioSource, CodecUnavailable, EncoderFailed }`
- `SubscribeError { NotFound, NoCatalog, RenditionNotFound, DecoderFailed, Ended }`
- All public methods return domain errors. Internal code uses anyhow + converts.
- `Debug + Display + Error + Send + Sync` on all error types.

---

## Phase 5: Relay Support

### 5.1 `VideoTrack: VideoSource`

Implement `VideoSource` on `VideoTrack` so decoded video can feed into a
`LocalBroadcast` for transcode relay.

### 5.2 `LocalBroadcast::relay(remote)`

Zero-transcode relay: copy catalog, forward all track data verbatim.
Watch for catalog changes and adapt forwarding tasks.

---

## Phase 6: Incoming Sessions (iroh-moq)

- `IncomingSession { remote_id(), accept() -> MoqSession, reject() }`
- `IncomingSessionStream` with `next() -> Option<IncomingSession>`
- `Moq::incoming_sessions() -> IncomingSessionStream`

---

## Phase 7: Live Builder + Call + TrackName (iroh-live)

### 7.1 LiveBuilder

```rust
Live::builder(endpoint)
    .enable_gossip()
    .spawn()           // no router
    .spawn_with_router() // creates router
```

`Live::register_protocols(router)` for compose-your-own-router pattern.

### 7.2 Call

Sugar over MoQ primitives:
- `Call::dial(live, remote)` — connect + publish "call" + subscribe "call"
- `Call::accept(session)` — accept + publish "call" + subscribe "call"
- `.local()` → `&LocalBroadcast`, `.remote()` → `Option<&RemoteBroadcast>`

### 7.3 TrackName

```rust
pub enum TrackName { Camera, Screen, Other(String) }
```

Used by room participants to disambiguate broadcasts. Room prepends namespace.

### 7.4 Relay convenience

- `Live::connect_relay(config)` → `MoqSession`
- `Live::publish_to_relay(config, name, broadcast)` → `MoqSession`
- `Live::subscribe_from_relay(config, name)` → `RemoteBroadcast`

---

## Phase 8: Room Participant Model

### 8.1 Participant types

- `LocalParticipant { id(), broadcast(TrackName) -> &LocalBroadcast }`
- `RemoteParticipant { id(), broadcast(TrackName) -> Option<&RemoteBroadcast> }`

### 8.2 RoomEvent

```rust
pub enum RoomEvent {
    ParticipantJoined(RemoteParticipant),
    ParticipantLeft { participant: ParticipantId, reason: DisconnectReason },
    TrackPublished { participant: ParticipantId, track: TrackName, kind: TrackKind, rendition: String },
    TrackUnpublished { participant: ParticipantId, track: TrackName, kind: TrackKind, rendition: String },
}
```

### 8.3 Room API

- `room.local()` → `&LocalParticipant`
- `room.remote_participants()` → `Direct<Vec<RemoteParticipant>>`
- `room.events()` → `RoomEventStream`
- `room.ticket()` → `RoomTicket`

---

## Phase 9: Wire api_sketch.rs

Replace stub modules in `api_sketch.rs` with real `use` imports from the crates.
Remove placeholder types. Verify all ~18 examples compile against the real API.

---

## What NOT to Change

- `iroh-moq` protocol behavior (only add incoming stream)
- `moq-lite` / `hang` types
- Codec implementations (rusty-codecs)
- Pipeline threading model (OS threads for decoders)
- `web-transport-iroh`
