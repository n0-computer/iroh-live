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

| Current (moq-media) | Proposed | Rationale |
|---|---|---|
| `PublishBroadcast` | `Broadcast` | It's your broadcast. The verb is in the action, not the type name. |
| `SubscribeBroadcast` | `Subscription` | It's your subscription to someone's broadcast. |
| `WatchTrack` | `VideoTrack` | Matches `AudioTrack`. Describes what it is, not how you got it. |
| `AvRemoteTrack` | `MediaTracks` | Simple struct `{ video, audio }`. |
| `watch()` | `video()` / `video_with()` | On `Subscription`. Parallel with `audio()`. |
| `listen()` | `audio()` / `audio_with()` | Consistent with `video()`. |
| `watch_local()` | `preview()` | Self-documenting. |
| `watch_rendition()` | `video_rendition()` | Consistent prefix. |
| `listen_rendition()` | `audio_rendition()` | Consistent prefix. |
| `CatalogWrapper` | `CatalogSnapshot` | Describes what it is. |

### moq-media (broadcast + media layer)

The primary API for all media operations. Usable standalone without iroh-live
for non-RTC use cases.

- `Broadcast` — publish media. Sub-handles: `video()` → `VideoPublisher`, `audio()` → `AudioPublisher`
- `Subscription` — subscribe to remote media. Methods: `video()`, `audio()`, catalog access
- `VideoTrack` / `AudioTrack` — decoded media from a subscription
- `VideoTarget` — declarative quality selection: `max_pixels`, `max_bitrate`
- `Broadcast::relay(subscription)` — zero-transcode relay (forward packets without decode)
- `VideoTrack` implements `VideoSource` — enables transcode relay (decode → re-encode)
- All `&self` (interior mutability). All `Clone` (Arc-based). Drop-based cleanup.
- Domain-specific error types: `PublishError`, `SubscribeError`, `CodecError`

### iroh-live (product layer)

Adds signaling, identity, and room semantics. Re-exports moq-media types.
Only wraps where genuinely new concepts are at play (participants, calls).

- `Live` — entry point (builder-based construction), manages transport + gossip
- `Call` / `IncomingCall` — one-to-one sessions
- `Room` — multi-party with participant model, `events() → impl Stream<Item = RoomEvent>`
- `LocalParticipant` → `.broadcast()` returns `&Broadcast`
- `RemoteParticipant` → `.subscription()` returns `&Subscription`
- Tickets: `CallTicket`, `RoomTicket`
- IDs: `ParticipantId`, `RoomId` (newtype wrappers, `Copy`, `Hash`, `Eq`)
- Structured errors: `CallError`, `RoomError`
- Prelude module for ergonomic imports

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
| `Watcher` (n0_watcher) | Continuous state — always has a current value | `call.state()`, `sub.status()`, `sub.catalog_watcher()` |
| `impl Stream` (futures) | Discrete events — sequence of one-time occurrences | `room.events()`, `live.incoming_calls()` |

**Dual-accessor convention:** stateful types expose both a snapshot method and a
watcher method. Example: `sub.catalog()` → `CatalogSnapshot` (current value),
`sub.catalog_watcher()` → `impl Watcher<Value = CatalogSnapshot>` (subscribe to
changes).

## Use Case Coverage

| Use case | Crate | Key types |
|---|---|---|
| Video/audio call | iroh-live | `Call`, `LocalParticipant`, `RemoteParticipant` |
| Multi-party room | iroh-live | `Room`, `RoomEvent` |
| Live streaming | moq-media | `Broadcast`, `Subscription` |
| Zero-transcode relay | moq-media | `Subscription` → `Broadcast::relay()` |
| Transcoding relay | moq-media | `Subscription::video()` → `Broadcast` (re-encode) |
| Camera dashboard | moq-media | N × `Subscription` with `VideoTarget::max_pixels()` |
| Recording pipeline | moq-media | `Subscription` → `VideoTrack::frames()` stream |
| Audio studio link | moq-media | `Broadcast` (audio only), `Subscription` (audio only) |
| Custom codec pipeline | moq-media | `VideoEncoderPipeline`, `VideoDecoderPipeline` |

## Documents

- `0-overview.md` — this file
- `1-review.md` — detailed review of current API with inline comments
- `2-research.md` — survey of LiveKit, Hang, WebRTC, GStreamer, OBS APIs
- `3-sketch.md` — Rust code sketch of the proposed API
- `4-impl.md` — phased implementation plan
- `5-examples.md` — example code against the proposed API
- `6-relay.md` — relay server integration (moq-relay, SFU rooms, CDN)

## Migration Strategy

1. Rename + improve moq-media types (`Broadcast`, `Subscription`, `VideoTrack`)
2. Add slot sub-handles (`VideoPublisher`, `AudioPublisher`)
3. Add `VideoTarget`, `VideoOptions`, `AudioOptions` to subscription
4. Add relay support (`Broadcast::relay()`, `VideoTrack: VideoSource`)
5. Add `IncomingSession` to iroh-moq
6. Add `Live::builder()`, `Call` / `IncomingCall` to iroh-live
7. Redesign `Room` around participants with `impl Stream<Item = RoomEvent>`
8. Add domain error types, prelude, polish
