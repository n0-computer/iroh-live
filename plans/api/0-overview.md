# API Redesign Overview

## Goal

Redesign the public API across `moq-media` and `iroh-live` to be ergonomic,
idiomatic Rust, and usable across RTC (rooms, calls) and non-RTC (streaming,
studio links, relays, pipelines) use cases.

## Current State

Strong low-level building blocks (`moq-media` codecs/pipelines, `iroh-moq`
transport, `moq-lite`/`hang` broadcast primitives) but the public API is
transport-shaped. Users must reason about `MoqSession`,
`BroadcastProducer`/`BroadcastConsumer`, catalog entries, and actor split
handles before they can express simple intents.

Naming is also inconsistent: `PublishBroadcast` / `SubscribeBroadcast` are
verbose, `watch()` / `listen()` are asymmetric and non-obvious, `WatchTrack`
doesn't match `AudioTrack` in naming convention.

## Direction: Rename + Improve In-Place, Re-Export Up

The original plan called for three wrapper layers. In practice that creates
duplication: `LocalBroadcast` wrapping `PublishBroadcast`, `RemoteBroadcast`
wrapping `SubscribeBroadcast`, option types duplicated across crates.

Instead: **rename and improve moq-media's API directly**, then **re-export from
iroh-live** and add only what requires signaling (rooms, calls, participants).

The three logical layers still exist, but as crate boundaries, not as wrapper
types within a single crate:

1. **moq-media** — broadcast/media layer. All publish/subscribe/codec types
   live here. Usable standalone for non-RTC use cases.
2. **iroh-live** — product layer. Rooms, calls, participants. Re-exports
   moq-media types and adds signaling/identity.
3. **iroh-moq** — transport layer. Raw MoQ protocol. Exposed via
   `iroh_live::transport` escape hatch.

### Naming Changes

| Current (moq-media) | New | Rationale |
|---|---|---|
| `PublishBroadcast` | `LocalBroadcast` | Symmetric with `RemoteBroadcast`. "Local" = ours. |
| `SubscribeBroadcast` | `RemoteBroadcast` | Symmetric with `LocalBroadcast`. "Remote" = theirs. |
| `WatchTrack` | `VideoTrack` | Matches `AudioTrack`. Describes what it is, not how you got it. |
| `AvRemoteTrack` | `MediaTracks` | Simple struct `{ video, audio }`. |
| `watch()` | `video()` / `video_with()` | On `RemoteBroadcast`. Parallel with `audio()`. |
| `listen()` | `audio()` / `audio_with()` | Consistent with `video()`. |
| `watch_local()` | `preview()` | Self-documenting. |
| `CatalogWrapper` | `CatalogSnapshot` | Describes what it is. |

### moq-media (broadcast + media layer)

The primary API for all media operations. Usable standalone without iroh-live
for non-RTC use cases.

- `LocalBroadcast` — publish media. Sub-handles: `video()` → `VideoPublisher`, `audio()` → `AudioPublisher`
- `RemoteBroadcast` — subscribe to remote media. Stores `MoqSession`, constructor takes `(session, name)`.
- `VideoTrack` / `AudioTrack` — decoded media from a remote broadcast
- `VideoTarget` — declarative quality selection: `max_pixels`, `max_bitrate`
- `LocalBroadcast::relay(remote)` — zero-transcode relay (forward packets without decode)
- `VideoTrack` implements `VideoSource` — enables transcode relay (decode → re-encode)
- All `&self` (interior mutability). All `Clone` (Arc-based). Drop-based cleanup.
- Domain-specific error types: `PublishError`, `SubscribeError`
- Status/catalog via `n0_watcher::Direct<T>` — `.get()` for snapshot, `.updated().await` for changes

### iroh-live (product layer)

Adds signaling, identity, and room semantics. Re-exports moq-media types.
Only wraps where genuinely new concepts are at play (participants, calls).

- `Live` — entry point (builder-based construction), manages transport + gossip
- `Call` — one-to-one call helper (sugar over MoQ primitives)
- `Room` — multi-party with participant model, `events() → RoomEventStream`
- `LocalParticipant` → `.broadcast(TrackName)` returns `&LocalBroadcast`
- `RemoteParticipant` → `.broadcast(TrackName)` returns `Option<&RemoteBroadcast>`
- `TrackName` — enum `{ Camera, Screen, Other(String) }` for room track disambiguation
- Tickets: `RoomTicket`
- IDs: `ParticipantId`, `RoomId` (newtype wrappers, `Copy`, `Hash`, `Eq`)
- Structured errors: `CallError`, `RoomError`
- No prelude module

### iroh-moq (transport, mostly unchanged)

- Add `IncomingSession` stream for accept/reject
- `MoqSession`, `BroadcastProducer`/`BroadcastConsumer` stay as-is

## Design Principles

1. **No wrapper duplication.** moq-media types are used directly. iroh-live
   only adds types for genuinely new concepts (participants, rooms, calls).
2. **`&self` everywhere.** Interior mutability. No `&mut self` on public types.
3. **Drop-based cleanup.** Drop a `Call` → closes. Drop a `Subscription` → unsubscribes.
4. **Relay is first-class.** Zero-transcode relay via `Broadcast::relay()`.
   Transcode relay via `VideoTrack` as `VideoSource`.
5. **Consistent naming.** Publish: `Broadcast`, `VideoPublisher`, `AudioPublisher`.
   Subscribe: `Subscription`, `VideoTrack`, `AudioTrack`.
6. **moq-media is standalone.** Non-RTC users never touch iroh-live.
7. **Watcher for continuous state, Stream for discrete events.** Connection
   quality, active rendition → `Watcher`. Participant joined, track published →
   `impl Stream`. Both are idiomatic async Rust.
8. **Declarative intent, not mechanism.** `VideoTarget { max_pixels: 1280*720 }`
   instead of manually selecting rendition strings.
9. **Self-contained events.** Events carry handles (e.g., `RemoteParticipant`),
   not just IDs that require follow-up lookups.
10. **Queries return owned snapshots.** `remote_participants()` returns `Vec<RemoteParticipant>`
    (cheap Arc clone), not borrowed references. Safe in async code.

## Reactivity Patterns

Two complementary patterns, used consistently across all stateful types:

| Pattern | Used for | Example |
|---|---|---|
| `Direct<T>` (n0_watcher) | Continuous state — always has a current value | `sub.status()`, `sub.catalog()`, `room.remote_participants()` |
| Event stream | Discrete events — sequence of one-time occurrences | `room.events()`, `moq.incoming_sessions()` |

**Watcher convention:** stateful types return `Direct<T>`. Call `.get()` for a
snapshot, `.updated().await` to wait for the next change. No dual-accessor
pattern — a single method returns the watcher, and `.get()` gives the snapshot.

## Use Case Coverage

| Use case | Crate | Key types |
|---|---|---|
| Video/audio call | iroh-live | `Call`, `LocalBroadcast`, `RemoteBroadcast` |
| Multi-party room | iroh-live | `Room`, `RoomEvent`, `TrackName` |
| Live streaming | moq-media | `LocalBroadcast`, `RemoteBroadcast` |
| Zero-transcode relay | moq-media | `RemoteBroadcast` → `LocalBroadcast::relay()` |
| Transcoding relay | moq-media | `RemoteBroadcast::video()` → `LocalBroadcast` (re-encode) |
| Camera dashboard | moq-media | N × `RemoteBroadcast` with `VideoTarget::max_pixels()` |
| Recording pipeline | moq-media | `RemoteBroadcast` → `VideoTrack::frames()` stream |
| Audio studio link | moq-media | `LocalBroadcast` (audio only), `RemoteBroadcast` (audio only) |
| Custom codec pipeline | moq-media | `VideoEncoderPipeline`, `VideoDecoderPipeline` |

## Documents

- `0-overview.md` — this file
- `1-review.md` — detailed review of current API with inline comments
- `2-research.md` — survey of LiveKit, Hang, WebRTC, GStreamer, OBS APIs
- `3-sketch.md` — Rust code sketch of the proposed API
- `4-impl.md` — phased implementation plan
- `5-examples.md` — example code against the proposed API
- `6-relay.md` — relay server integration (moq-relay, SFU rooms, CDN)

## Canonical API Sketch

The full compiling API sketch is in `iroh-live/examples/api_sketch.rs`. It
defines all types as stubs with `todo!()` bodies and includes ~18 usage examples.
This is the single source of truth for the target API surface.

## Migration Strategy

1. Rename moq-media types (`LocalBroadcast`, `RemoteBroadcast`, `VideoTrack`, `CatalogSnapshot`)
2. Add slot sub-handles (`VideoPublisher`, `AudioPublisher`) with `&self`
3. Add `VideoTarget`, `VideoOptions`, `AudioOptions`, status watchers on `RemoteBroadcast`
4. Add domain error types (`PublishError`, `SubscribeError`)
5. Add relay support (`LocalBroadcast::relay()`, `VideoTrack: VideoSource`)
6. Add `IncomingSession`/`IncomingSessionStream` to iroh-moq
7. Add `Live::builder()`, `Call`, `TrackName` to iroh-live
8. Redesign `Room` around participants with `TrackName`-keyed broadcasts
9. Wire api_sketch.rs to use real crate imports, remove stubs
