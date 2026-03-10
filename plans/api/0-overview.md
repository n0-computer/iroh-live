# API Redesign Overview

## Goal

Redesign the `iroh-live` public API to be ergonomic, idiomatic Rust, and usable across both RTC (rooms, calls) and non-RTC (streaming, studio links, pipelines) use cases.

## Current State

The existing stack has strong low-level building blocks (`moq-media` codecs/pipelines, `iroh-moq` transport, `moq-lite`/`hang` broadcast primitives) but the public API is transport-shaped rather than product-shaped. Users must reason about `MoqSession`, `BroadcastProducer`/`BroadcastConsumer`, catalog entries, and actor split handles before they can express simple intents like "join a room" or "subscribe to a stream".

## Chosen Direction: Three-Layer API (Alternative D)

> codex: This is the right choice. A room/call-only API would underserve manual media users, and a broadcast-only API would keep product ergonomics too low. The three-layer split matches the actual goals of the repo.

### Layer 1: Product API (`iroh_live`)

For app authors building calls, rooms, and interactive sessions.

- `Live` — entry point, builder-based
- `Call` / `IncomingCall` — one-to-one sessions with accept/reject
- `Room` / `RoomEvent` — multi-party with participant and publication model
- `LocalParticipant` / `RemoteParticipant` — first-class participant identity
- `LocalTrackPublication` / `RemoteTrackPublication` — publish/subscribe through publications
- `CallTicket` / `RoomTicket` — portable join handles

### Layer 2: Broadcast API (`iroh_live::broadcast`)

For advanced users who want direct media composition without room/call semantics. Also the primary API for non-RTC use cases (streaming, studio links, dashboards, pipelines).

- `LocalBroadcast` / `RemoteBroadcast` — cohesive broadcast objects
- `BroadcastTicket` — standalone broadcast join handle
- `VideoTarget` / `SubscribeVideoOptions` — declarative selection
- Catalog access, rendition selection, preview

> codex: This middle layer is where the toolkit can become genuinely distinctive. If `RemoteBroadcast` and `LocalBroadcast` are excellent, the project can be both an ergonomic app SDK and a serious media toolkit.

### Layer 3: Raw Transport + Media (`iroh_live::transport`, `moq-media`)

For systems work, custom pipelines, and experimentation.

- `MoqSession` — direct transport access
- `BroadcastProducer` / `BroadcastConsumer` — raw publish/subscribe
- Media pipelines, codecs, sources, sinks

## Key Design Principles

1. **Product-first top layer, systems-first lower layers.** Simple path stays in layer 1.
2. **Stable, cheaply-cloneable object model.** All handles are `Arc`-based + `Clone`.
3. **Drop-based cleanup.** Dropping a `Call` closes it, dropping a subscription unsubscribes.
4. **Command-query split.** Methods mutate (`&self` + interior mutability), queries return owned snapshots, events via `Stream`.
5. **Watchers for continuous state, streams for discrete events.** Using `n0_watcher::Watchable` (already in codebase).
6. **Broadcast layer is first-class.** Not just an implementation detail — it's the universal mid-level API for all use cases.
7. **Accepting inbound is first-class.** `IncomingCall` with type-safe accept/reject, auto-reject on drop.
8. **Manual processing remains first-class.** Custom sources, encoders, decoders, frame processing — behind explicit advanced APIs.

## Use Case Coverage

| Use case | Primary layer | Key types |
|---|---|---|
| Video/audio call | Product | `Call`, `LocalParticipant`, `RemoteTrackPublication` |
| Multi-party room | Product | `Room`, `RoomEvent`, `Participant` |
| Live streaming | Broadcast | `LocalBroadcast`, `RemoteBroadcast` |
| Audio studio link | Broadcast | `LocalBroadcast` (audio-only) |
| Camera dashboard | Broadcast | N × `RemoteBroadcast` |
| Recording pipeline | Broadcast + Raw | `RemoteBroadcast`, frame access |
| Transcoding relay | Broadcast + Raw | `RemoteBroadcast` + `LocalBroadcast` + codec API |

## Documents

- `0-overview.md` — this file
- `1-review.md` — detailed review of current API with inline comments
- `2-research.md` — survey of LiveKit, Hang, WebRTC, GStreamer, OBS APIs
- `3-sketch.md` — Rust code sketch of the proposed API (all todo!())
- `4-impl.md` — phased implementation plan with concrete steps
- `5-risks-and-future.md` — redesign risks, future work, and success criteria
- `6-examples.md` — rewritten examples against the proposed API, with async/sync boundaries

## Migration Strategy

1. Add broadcast object layer (`LocalBroadcast` / `RemoteBroadcast`) wrapping existing types
2. Add incoming call acceptance primitives (`Moq::incoming()`, `Live::accept_call()`)
3. Define participant/publication object model
4. Redesign room events around participants and tracks
5. Redesign local publish APIs around `LocalParticipant`
6. Hide transport-first types from default surface
7. Clean up naming

> codex: The ordering matters. We should not jump directly to `Call` and `Room` wrappers if the broadcast layer is still thin, or we will just bake today's rough edges into tomorrow's API.
