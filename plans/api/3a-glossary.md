# Glossary

Precise definitions of all terms in the API redesign and their layer assignments.
Terms are grouped by concept, then listed alphabetically within each group.

See `0-overview.md` for the three-layer architecture and `3-sketch.md` for code.

## Layers

| Layer | Module | Purpose |
|-------|--------|---------|
| **Layer 1 — Product** | `iroh_live` (crate root) | Calls, rooms, participants. For app authors. |
| **Layer 2 — Broadcast** | `iroh_live::broadcast` | Direct media publish/subscribe without room or call semantics. For streaming, pipelines, advanced composition. |
| **Layer 3 — Transport + Media** | `iroh_live::transport`, `moq_media` | Raw MoQ sessions, codec pipelines, frame-level access. For systems work. |
| **Shared** | `iroh_live::types` | IDs, enums, and options used across all layers. |

---

## Core Concepts

### Broadcast

The fundamental unit of media publication in MoQ. A broadcast is a named, independently subscribable stream of media with its own **catalog**. One broadcast may contain video renditions, audio renditions, or both. A single participant may publish multiple broadcasts (e.g. one for camera+mic, another for screen-share).

**Wire representation**: A string name (e.g. `"camera"`, `"screen"`, `"concert"`) scoped to a MoQ session. Subscribers address a broadcast as `(endpoint_addr, broadcast_name)`.

### Catalog

A metadata document published alongside a broadcast that describes its available **renditions** (video and audio tracks with their codecs, resolutions, bitrates). Updated dynamically — renditions can be added or removed over the lifetime of a broadcast. Defined in `hang::catalog::Catalog`.

**Layer**: Layer 3 (hang/moq-lite), exposed through Layer 2 via `RemoteBroadcast::catalog()`.

### Rendition

One quality variant of a media track within a broadcast's catalog. A video broadcast might have renditions at 180p, 480p, and 720p. An audio broadcast might have a single HQ rendition. Subscribers choose which rendition to receive (or let the system auto-select based on bandwidth).

**Wire representation**: A named entry in the catalog's `BTreeMap<String, VideoConfig>` or `BTreeMap<String, AudioConfig>`.

### Track

A single media stream within a broadcast — one rendition of one media kind. At the transport level, each track is a sequence of **groups** (GOPs for video), each containing **frames**. A subscriber receives one track at a time per media kind.

**Layer**: Layer 3 (moq-lite `Track`, `TrackProducer`, `TrackConsumer`).

### Slot

A logical publishing position within a `LocalBroadcast` for one media kind (video or audio). A slot can have its source set, replaced, cleared, enabled/disabled, or muted/unmuted without tearing down the broadcast. Conceptually equivalent to WebRTC's `RTCRtpSender` — the slot persists, only the source changes.

**Layer**: Layer 2 (`LocalVideoSlot`, `LocalAudioSlot`).

### Publication

In this API, "publication" means "broadcast". A participant publishes broadcasts, not individual tracks. Track-level details (renditions, codecs) are internal to the broadcast and discoverable through its catalog. This differs from WebRTC/LiveKit where "publication" refers to a single track.

---

## Shared Types (Layer: Shared)

### `BroadcastKind`

Type-safe identifier for the semantic role of a broadcast within a participant's set of broadcasts.

- `Camera` — camera video + microphone audio (wire name: `"camera"`)
- `Screen` — screen-share video + optional system audio (wire name: `"screen"`)
- `Named(String)` — custom broadcast for advanced workflows

Implements `From<&str>` and `From<String>` for ergonomic construction. `"camera"` and `"screen"` round-trip to the typed variants.

### `DisconnectReason`

Why a call or room connection ended: `LocalClose`, `RemoteClose`, or `TransportError(Arc<dyn Error>)`.

### `ParticipantId`

Newtype over `iroh::EndpointId` identifying a participant. Two participants with the same underlying endpoint compare equal. Used as map keys for participant lookups.

### `RoomId`

Newtype over `iroh_gossip::TopicId` identifying a room by its gossip topic.

---

## Layer 1 — Product Types

### `Live`

Central entry point. Holds the iroh `Endpoint`, gossip handle, and protocol handlers. All operations start here: `call()`, `accept_call()`, `join_room()`, `create_broadcast()`, `subscribe_broadcast()`. Arc-based, cheap to clone.

**Construction**: `Live::builder().from_env().spawn().await?` or `Live::new(endpoint, gossip)`.

### `LiveBuilder`

Builder for `Live`. Configures endpoint, gossip, and protocol handlers with sensible defaults. `from_env()` reads relay URLs and secrets from environment. `spawn()` creates the instance.

### `Call`

Active one-to-one call session. Provides local and remote broadcasts keyed by `BroadcastKind`. Each side can independently publish camera, screen, or custom broadcasts. Arc-based, cheap to clone. Dropping closes the call.

**Key methods**:
- `local(kind)` — get/create a local broadcast for publishing
- `remote(kind)` — get a remote broadcast if announced
- `wait_remote(kind)` — async, wait for a specific remote broadcast
- `recv_remote()` — async, yield the next remote broadcast as it appears
- `close()` — end the call gracefully
- `state()` — watcher for `CallState` (Connecting/Connected/Disconnected)

### `CallTicket`

Portable handle for initiating a call. Contains the remote `EndpointAddr` and a `call_id` (broadcast name for MoQ subscription, generated randomly by default). Serializable, shareable via QR/URL.

### `CallState`

Enum: `Connecting`, `Connected`, `Disconnected(DisconnectReason)`.

### `IncomingCall`

An unanswered incoming call. Must be explicitly `accept()`ed or `reject()`ed. Dropping without accepting auto-rejects. Consumes self on accept/reject (cannot be reused).

### `Room`

Active multi-party room session. Provides `local_participant()` for publishing and `recv()` for events. Maintains a live participant map accessible via `remote_participants()` or the reactive `participants()` watcher. Arc-based, cheap to clone. Dropping the last clone leaves the room.

### `RoomEvent`

Discrete events from a room:
- `ParticipantJoined(RemoteParticipant)` — new peer
- `ParticipantLeft { participant, reason }` — peer departed
- `BroadcastPublished { participant, kind, broadcast }` — new broadcast available for subscription
- `BroadcastUnpublished { participant, kind }` — broadcast removed

Track-level changes (video added to existing broadcast) are **not** room events — observe those via `RemoteBroadcast::catalog()`.

### `RoomTicket`

Portable handle for joining/creating a room. Contains `RoomId` and bootstrap peer list. `RoomTicket::generate()` creates a new room.

### `LocalParticipant`

The local peer within a room. Provides `broadcast(kind)` to create/access local broadcasts for publishing. Broadcasts are created lazily on first access.

### `RemoteParticipant`

A remote peer in a room or call. Provides:
- `broadcast(kind)` — get a remote broadcast by kind
- `broadcasts()` — all broadcasts
- `wait_broadcast(kind)` — async, wait for a broadcast to appear

Arc-based, cheap to clone.

---

## Layer 2 — Broadcast Types

### `LocalBroadcast`

Outbound broadcast for publishing media. Wraps `moq_media::publish::PublishBroadcast` with:
- `Arc + Mutex` for `&self` API (inner needs `&mut self`)
- Slot abstraction via `video()` / `audio()` returning `LocalVideoSlot` / `LocalAudioSlot`
- `publish(name)` for network attachment → `BroadcastTicket`

Also: `preview()` for local decode, `has_video()` / `has_audio()`, `producer()` escape hatch.

### `RemoteBroadcast`

Inbound broadcast for consuming media. Wraps `moq_media::subscribe::SubscribeBroadcast` with:
- `BroadcastStatus` watchable (Connecting → Live → Ended lifecycle tracking)
- `SelectedVideo` / `SelectedAudio` snapshots (remembers last subscription choice)
- `has_video()` / `has_audio()` convenience derived from catalog

Subscribe via `subscribe_video::<D>()` → `VideoTrack` and `subscribe_audio::<D>(backend)` → `AudioTrack`. Catalog access via `catalog()` watcher or `catalog_snapshot()`. Raw access via `as_inner()`.

### `BroadcastTicket`

Portable handle for subscribing to a remote broadcast. Contains `EndpointAddr` + `broadcast_name`. Serializable, shareable.

### `BroadcastStatus`

Enum: `Connecting`, `Live`, `Ended`. Tracks the lifecycle of a `RemoteBroadcast`.

### `LocalVideoSlot`

Video-side control for a `LocalBroadcast`. One method for all source types: `set_source(source, presets)`. Also `clear()`, `set_enabled(bool)`, `is_enabled()`. The broadcast's `BroadcastKind` carries the semantic meaning (camera vs screen), not the slot.

### `LocalAudioSlot`

Audio-side control for a `LocalBroadcast`. One method: `set_source(source, presets)`. Also `clear()`, `set_muted(bool)`, `is_muted()`.

### `SubscribeVideoOptions`

Options for video subscription: `quality`, `rendition` (specific name), `viewport` (decode scaling), `decode_config`. Builder pattern.

### `SubscribeAudioOptions`

Options for audio subscription: `quality`, `rendition`. Builder pattern.

### `SelectedVideo` / `SelectedAudio`

Snapshots of the currently active subscription choice (rendition name + options). For UI display and debugging.

---

## Layer 3 — Transport + Media Types

These types are defined in `moq-media`, `iroh-moq`, `moq-lite`, and `hang`. They are not wrapped by the API redesign — used directly.

### `VideoTrack` (`moq_media::subscribe::VideoTrack`)

Active video subscription yielding decoded frames. Runs a decoder thread internally. Key methods:
- `current_frame(&mut self) -> Option<DecodedVideoFrame>` — latest decoded frame (non-blocking, for render loops)
- `next_frame(&mut self) -> Option<DecodedVideoFrame>` — async, wait for next frame
- `set_viewport(w, h)` — set decode output dimensions
- `rendition() -> &str` — active rendition name

**Previously named `VideoTrack`**. Renamed for consistency with `AudioTrack`.

### `AudioTrack` (`moq_media::subscribe::AudioTrack`)

Active audio subscription playing through an audio backend. Runs a decoder and playback pipeline internally. Key methods:
- `stopped() -> impl Future<Output = ()>` — resolves when audio ends
- `rendition() -> &str` — active rendition name
- `handle() -> &dyn AudioSinkHandle` — playback control

### `MoqSession` (`iroh_moq::MoqSession`)

A MoQ transport session over a QUIC connection. Provides raw `subscribe(name)` and `publish(name, producer)`. Used internally by `Call`, `Room`, and `RemoteBroadcast`.

### `BroadcastProducer` / `BroadcastConsumer` (`moq_lite`)

Raw publish/subscribe handles for a broadcast at the transport level. A `BroadcastProducer` writes groups and frames. A `BroadcastConsumer` reads them. Accessible via `LocalBroadcast::producer()` and `RemoteBroadcast::as_inner()`.

### `PublishBroadcast` (`moq_media::publish::PublishBroadcast`)

Higher-level publish handle that manages encoding pipelines, renditions, and catalog generation. Wrapped by `LocalBroadcast`.

### `SubscribeBroadcast` (`moq_media::subscribe::SubscribeBroadcast`)

Higher-level subscribe handle that manages catalog watching, rendition selection, and decoder spawning. Wrapped by `RemoteBroadcast`.

### `Catalog` (`hang::catalog::Catalog`)

Metadata describing a broadcast's available renditions. Contains `BTreeMap<String, VideoConfig>` and `BTreeMap<String, AudioConfig>`. Updated dynamically.

---

## Relationship Diagram

```
Live
 ├── Call
 │    ├── local(Camera)  → LocalBroadcast → LocalVideoSlot / LocalAudioSlot
 │    ├── local(Screen)  → LocalBroadcast → LocalVideoSlot / LocalAudioSlot
 │    ├── remote(Camera) → RemoteBroadcast → subscribe_video() → VideoTrack
 │    └── remote(Screen) → RemoteBroadcast → subscribe_audio() → AudioTrack
 │
 ├── Room
 │    ├── local_participant()
 │    │    └── broadcast(kind) → LocalBroadcast → slots
 │    └── recv() → RoomEvent::BroadcastPublished { broadcast }
 │         └── broadcast → RemoteBroadcast → subscribe_video/audio
 │
 ├── create_broadcast() → LocalBroadcast (standalone, no room/call)
 └── subscribe_broadcast(ticket) → RemoteBroadcast (standalone)
```

---

## Name Mapping: Wire ↔ API

| Wire concept | MoQ protocol | API type |
|-------------|-------------|----------|
| Endpoint address | `EndpointAddr` | `CallTicket.remote`, `BroadcastTicket.remote` |
| Broadcast name | String (e.g. `"camera"`) | `BroadcastKind` |
| Catalog | JSON metadata track | `Catalog` (hang), exposed via `RemoteBroadcast::catalog()` |
| Track | Named stream within broadcast | `VideoTrack` / `AudioTrack` (subscribed), `Track` / `TrackProducer` (raw) |
| Group | GOP / chunk of frames | Internal to transport, not exposed at Layers 1-2 |
| Frame | Single encoded unit | `DecodedVideoFrame` (after decode), raw bytes at Layer 3 |

---

## Name Mapping: LiveKit ↔ iroh-live

For users coming from LiveKit's API:

| LiveKit | iroh-live | Notes |
|---------|-----------|-------|
| `Room` | `Room` | Same concept |
| `LocalParticipant` | `LocalParticipant` | Same concept |
| `RemoteParticipant` | `RemoteParticipant` | Same concept |
| `TrackPublication` | `RemoteBroadcast` | In iroh-live, broadcasts are the publication unit (one broadcast = N tracks/renditions) |
| `Track` | `VideoTrack` / `AudioTrack` | Active subscription handles |
| `RoomEvent` | `RoomEvent` | Similar events, but broadcast-level not track-level |
| `LocalTrackPublication` | `LocalBroadcast` + slot API | Publishing is through broadcast slots, not track publications |
| `connect()` | `Live::builder().spawn()` | Builder-based setup |
| `room.localParticipant.publishTrack()` | `local_participant().broadcast(Camera).video().set_source()` | Source-on-slot, not track publish |
