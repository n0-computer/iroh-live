# Rooms overhaul: feature-complete AV rooms with text chat

## Motivation

The current room system handles peer discovery via gossip and automatic
broadcast subscription, but it lacks text chat, structured presence
events, and the UI affordances that make rooms usable as a standalone
communication tool. This plan adds those pieces incrementally, working
within the existing hang catalog format which already defines `Chat`
and `User` metadata.

## Design decisions

**Chat messages as MoQ track groups.** Each chat message becomes one
group on the broadcast's chat track (declared in the hang catalog's
`chat.message` field). This reuses the existing transport with zero new
wire formats. Groups are naturally ordered, tolerate late joiners
(groups expire after 30s in moq-lite), and the catalog tells
subscribers where to find the track.

**Chat lives in moq-media, not iroh-live.** The chat track is a data
track on a broadcast, just like video and audio. It belongs at the
moq-media layer so that any transport (not just iroh) can carry it.
The iroh-live `Room` layer wires chat into the room's gossip
coordination without knowing the details.

**User metadata in the catalog.** hang's `User { id, name, avatar,
color }` struct is already part of the catalog. We populate it on the
publish side and expose it on the subscribe side. This gives rooms a
display name for each peer without a separate presence protocol.

**Room-level presence from gossip KV.** The existing `PeerState`
gossip message gains an optional display name. The room actor emits
`PeerJoined` / `PeerLeft` events derived from gossip KV
insertions/expirations. No new gossip protocol needed.

**Incremental commits.** Each step compiles and passes `check-all`.

## Implementation phases

### Phase 1: ChatTrack in moq-media

Add `moq-media/src/chat.rs` with:

- `ChatMessage`: timestamped text message with sender ID
- `ChatPublisher`: wraps a `TrackProducer`, writes JSON-serialized
  messages as single-frame groups
- `ChatSubscriber`: wraps a `TrackConsumer`, yields `ChatMessage`
  from an async stream
- Wire into `LocalBroadcast`: add `chat()` method returning a
  `ChatPublisher` handle, catalog gets `chat.message` track set
- Wire into `RemoteBroadcast`: add `chat()` method returning a
  `ChatSubscriber` when the catalog has a chat track

### Phase 2: User metadata

- `LocalBroadcast::set_user(User)` updates the catalog's user field
- `RemoteBroadcast::user()` reads it from the catalog snapshot
- Expose in `CatalogSnapshot` (already derefs to `Catalog`, so
  `snapshot.user` works)

### Phase 3: Room presence events

- Extend `PeerState` with optional `display_name: Option<String>`
- Add `RoomEvent::PeerJoined { remote, display_name }` and
  `RoomEvent::PeerLeft { remote }`
- Track known peers in the room actor, emit join on first sight, leave
  on gossip KV expiry or disconnect

### Phase 4: Room chat integration

- `RoomHandle::send_chat(text)` sends through the local broadcast's
  chat publisher
- `RoomEvent::ChatMessage { remote, message }` emitted when a
  subscribed peer's chat track delivers a message
- Room actor spawns chat subscriber tasks for each subscribed broadcast

### Phase 5: CLI room chat UI

- Add a bottom chat panel to the egui room view
- Text input field, message history display
- Show peer display names from user metadata
- Show join/leave events inline in chat

## Scope boundaries

- No message persistence or history beyond moq-lite's 30s group cache
- No typing indicators in this iteration (the catalog supports it,
  but it adds complexity for little value right now)
- No end-to-end encryption for chat (same trust model as AV)
- No rich text or file attachments

## Dependencies

- `serde_json` for chat message serialization (already transitive via
  hang)
- `chrono` or `std::time` for message timestamps (prefer
  `std::time::SystemTime` to avoid new deps)
