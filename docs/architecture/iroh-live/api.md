# iroh-live API Design

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | iroh-live |

## Entry point

`Live` is the entry point for all iroh-live functionality. It wraps an iroh `Endpoint`, manages the MoQ transport layer (`Moq`), and optionally manages gossip for room membership.

Construction uses a builder pattern:

```rust
let live = Live::builder(endpoint)
    .enable_gossip()
    .spawn_with_router();
```

`LiveBuilder::enable_gossip()` creates a `Gossip` instance internally and auto-mounts it on the router. For applications that manage their own `Gossip`, `LiveBuilder::gossip()` accepts an externally-created instance.

`spawn()` creates the transport without a router (caller mounts protocols manually). `spawn_with_router()` creates a `Router`, mounts all protocols, and stores it on `Live` for lifetime management.

## Three abstraction levels

**`Live`** provides the lowest-level publish/subscribe operations. `live.publish(name, broadcast)` announces a `LocalBroadcast` for P2P discovery. `live.subscribe(addr, name)` connects to a remote endpoint and returns a `MoqSession` plus a `RemoteBroadcast`. These are the building blocks for custom session management.

**`Call`** wraps `Live` for 1:1 sessions. It handles the handshake of publishing local media and subscribing to the remote peer's media in a single operation. Dropping a `Call` closes the session, stops encoder pipelines, and disconnects.

**`Room`** provides multi-party sessions with gossip-based membership coordination. Participants publish their broadcasts and discover each other via `iroh-gossip`. `RoomTicket` encodes the topic ID and known peer addresses for sharing via links or QR codes.

## Reactivity model

Continuous state uses `n0_watcher::Direct<T>`. The `Direct` type always has a current value (accessed via `.get()`) and supports waiting for changes (`.updated().await`). Connection quality, active rendition, and catalog contents are examples of continuous state.

Discrete events (participant joins, track publications, session arrivals) are exposed as `impl Stream`. Callers `poll_next` or use `while let Some(event) = stream.next().await` patterns.

## Re-exports

moq-media types are re-exported under `iroh_live::media`, so applications that use iroh-live do not need a direct moq-media dependency for common types like `LocalBroadcast`, `RemoteBroadcast`, `VideoTrack`, and `AudioTrack`.

## Naming conventions

Types follow a symmetric naming pattern:

- `LocalBroadcast` (publish side) / `RemoteBroadcast` (subscribe side)
- `VideoPublisher` / `VideoTrack`
- `AudioPublisher` / `AudioTrack`
- `MediaTracks` combines a `RemoteBroadcast` with optional `VideoTrack` and `AudioTrack` for convenience

The "local/remote" prefix distinguishes the publishing and subscribing sides of a broadcast, while "publisher/track" distinguishes the slot handle (for feeding media in) from the decoded output handle (for consuming media).

## Transport access

`Live::transport()` returns a reference to the underlying `Moq` instance, which provides `connect()` for outbound connections and access to the `MoqProtocolHandler` for inbound connection acceptance. `MoqSession::conn()` exposes the raw iroh `Connection` for path stats and low-level diagnostics.
