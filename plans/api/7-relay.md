# Relay Support

How iroh-live integrates with moq-relay for relayed publishing, subscription, and
hybrid P2P+relay rooms.

This is designed to be **purely additive** to the API in `3-sketch.md` and
`4-impl.md`. No existing types need to change — relay adds new types and new
methods on existing types.

## Background

### moq-relay

`moq-relay` (from the moq workspace at `/moq/rs/moq-relay/`) is a content-agnostic
MoQ relay server. Key properties:

- **Path-based broadcast hierarchy**: Broadcasts live at paths like
  `"room/abc/alice/camera"`. Paths use `/` delimiters.
- **OriginProducer/OriginConsumer**: moq-lite's `with_root(prefix)` scopes
  publish/subscribe to a path prefix. This is the relay's scoping mechanism.
- **Subscribe-and-republish**: The relay subscribes to what clients publish and
  publishes what clients can subscribe to. Broadcasts flow through transparently.
- **JWT auth with path scoping**: Each connection gets a token with allowed
  publish/subscribe path prefixes.
- **Clustering**: Multiple relays mesh for global distribution.

### moq-native

`moq-native` (from `/moq/rs/moq-native/`) is the client library for connecting to
relays. It handles transport negotiation (QUIC, WebSocket fallback) and MoQ
handshake. Key API:

```rust
let client = Client::new(config)?
    .with_publish(origin_consumer)   // what we publish TO the relay
    .with_consume(origin_producer)   // what we subscribe FROM the relay
    .connect(url).await?;
```

### Current naming (P2P)

On P2P sessions, broadcast names are flat: `"camera"`, `"screen"`. Participant
identity comes from the session's `remote_id()` (one session = one participant).

### Relay naming

On a relay, all participants share one session. Broadcasts need a participant
prefix: `"{participant_id}/camera"`. The relay's path hierarchy handles this
naturally.

---

## Use Cases

### A) Publish to a relay

Publish a broadcast to a relay identified by endpoint address, under a prefix and
name. Like hang but over iroh.

### B) Subscribe to a room via relay

Subscribe to all publications under a prefix on a relay. This is the relay-backed
room: discover participants and their broadcasts by watching announces under the
room prefix.

### C) Hybrid P2P + relay room

Join a room using both gossip-based P2P connections AND a relay. Deduplicate when
a participant is reachable both directly and via relay. Let the user decide which
path to prefer (direct = lower latency, relay = more reliable NAT traversal).

---

## Proposed Types

### `Relay`

Connection to a moq-relay instance.

```rust
/// Connection to a moq-relay server.
///
/// Manages a single MoQ session to the relay. Broadcasts published through
/// this relay are visible to all subscribers on the same relay (or relay
/// cluster). Cheap to clone (Arc-based).
#[derive(Debug, Clone)]
pub struct Relay {
    inner: Arc<RelayInner>,
}

struct RelayInner {
    session: MoqSession,
    // Or: moq_native client handle, if we use moq-native for transport
}

impl Relay {

    /// Publishes a local broadcast to the relay under the given path.
    ///
    /// The path is the full broadcast path on the relay, e.g.
    /// `"rooms/abc123/alice/camera"`.
    pub fn publish(&self, path: impl ToString, broadcast: &LocalBroadcast) {
        self.inner.session.publish(path.to_string(), broadcast.producer().consume());
    }

    /// Subscribes to a broadcast on the relay by path.
    pub async fn subscribe(&self, path: &str) -> Result<RemoteBroadcast> {
        let mut session = self.inner.session.clone();
        let consumer = session.subscribe(path).await?;
        let sub = SubscribeBroadcast::new(path.to_string(), consumer).await?;
        Ok(RemoteBroadcast::new(sub))
    }

    /// Watches for all broadcast announces under a prefix.
    ///
    /// Yields `(relative_path, RemoteBroadcast)` for each new broadcast.
    /// The prefix is stripped from paths, so if prefix is `"rooms/abc"` and
    /// a broadcast is announced at `"rooms/abc/alice/camera"`, this yields
    /// `("alice/camera", broadcast)`.
    pub async fn watch_prefix(
        &self,
        prefix: &str,
    ) -> Result<RelayWatcher> {
        todo!()
    }

    /// Returns the underlying MoQ session for advanced usage.
    pub fn session(&self) -> &MoqSession {
        &self.inner.session
    }
}

/// Watches for broadcast announces under a relay prefix.
pub struct RelayWatcher {
    // Wraps OriginConsumer scoped with with_root(prefix)
}

impl RelayWatcher {
    /// Yields the next announced broadcast.
    pub async fn next(&mut self) -> Result<(String, RemoteBroadcast)> {
        todo!()
    }
}
```

### `Live::connect_relay`

Construction goes through `Live`, consistent with `connect_raw`:

```rust
impl Live {
    /// Connects to a moq-relay server.
    ///
    /// Returns a [`Relay`] handle for publishing and subscribing through
    /// the relay. The underlying MoQ session is managed by `Live`.
    pub async fn connect_relay(
        &self,
        remote: impl Into<EndpointAddr>,
    ) -> Result<Relay> {
        let session = self.connect_raw(remote).await?;
        Ok(Relay { inner: Arc::new(RelayInner { session }) })
    }
}
```

### `RoomOpts`

Builder-light options for `join_room_with_opts`. Extensible for future options
(e.g. max participants, room metadata, custom KV) without API churn.

```rust
/// Options for joining a room.
///
/// Use with [`Live::join_room_with_opts`]. Constructed via [`RoomOpts::new`]
/// and configured with builder methods.
#[derive(Debug, Clone, Default)]
pub struct RoomOpts {
    relay: Option<RoomRelay>,
    path_preference: Option<PathPreference>,
}

/// Relay configuration for a room.
#[derive(Debug, Clone)]
pub struct RoomRelay {
    /// The relay connection.
    pub relay: Relay,
    /// Path prefix for this room on the relay (e.g. `"rooms/{room_id}"`).
    pub prefix: String,
}

impl RoomOpts {
    /// Creates default room options (P2P only, no relay).
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds relay support. The room publishes and subscribes through the
    /// relay in addition to P2P gossip.
    pub fn relay(mut self, relay: Relay, prefix: impl Into<String>) -> Self {
        self.relay = Some(RoomRelay { relay, prefix: prefix.into() });
        self
    }

    /// Sets the path preference for hybrid P2P+relay rooms.
    /// Defaults to [`PathPreference::PreferDirect`].
    pub fn path_preference(mut self, pref: PathPreference) -> Self {
        self.path_preference = Some(pref);
        self
    }
}
```

### `Live::join_room_with_opts`

```rust
impl Live {
    /// Joins or creates a room with custom options.
    pub async fn join_room_with_opts(
        &self,
        ticket: impl Into<RoomTicket>,
        opts: RoomOpts,
    ) -> Result<Room> {
        todo!()
    }
}
```

Relay-only rooms (no gossip) use the same method:
```rust
let room = live.join_room_with_opts(
    RoomTicket::generate(),  // or a relay-only ticket
    RoomOpts::new()
        .relay(relay, "rooms/my-room")
        .path_preference(PathPreference::RelayOnly),
).await?;
```
No separate `join_room_relay_only` needed — `PathPreference::RelayOnly` expresses this.

---

## Use Case A: Publish to a Relay

```rust
use iroh_live::{Live, broadcast::LocalBroadcast};
use iroh_live::relay::Relay;

#[tokio::main]
async fn main() -> Result<()> {
    let live = Live::builder().from_env().spawn().await?;

    // Connect to the relay.
    let relay = live.connect_relay(relay_addr).await?;

    // Create and configure a broadcast.
    let broadcast = live.create_broadcast();
    broadcast.video().set_source(cam, VideoCodec::H264, [VideoPreset::P720])?;
    broadcast.audio().set_source(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    // Publish to the relay under a path.
    relay.publish("live/my-stream/camera", &broadcast);

    println!("publishing to relay");
    tokio::signal::ctrl_c().await?;
    Ok(())
}
```

Subscribing from the relay:

```rust
let relay = live.connect_relay(relay_addr).await?;
let broadcast = relay.subscribe("live/my-stream/camera").await?;
let video = broadcast.subscribe_video::<DefaultDecoders>()?;
```

### How it works

`Relay::publish()` calls `session.publish(path, consumer)` on the relay's MoQ
session. The relay announces this broadcast to all subscribers. `Relay::subscribe()`
calls `session.subscribe(path)` to get the broadcast from the relay.

This is the simplest use case — no rooms, no participants, just publish/subscribe
through a relay. Equivalent to hang's basic relay usage but over iroh transport.

---

## Use Case B: Subscribe to a Room via Relay

```rust
use iroh_live::{Live, RoomTicket, RoomEvent, BroadcastKind::*};
use iroh_live::relay::RoomOpts;

#[tokio::main]
async fn main() -> Result<()> {
    let live = Live::builder().from_env().spawn().await?;
    let relay = live.connect_relay(relay_addr).await?;

    let ticket: RoomTicket = std::env::args().nth(1).unwrap().parse()?;

    // Join room with relay support. The room uses both P2P (gossip) and
    // relay for discovery and media delivery.
    let room = live.join_room_with_opts(ticket, RoomOpts::new()
        .relay(relay.clone(), format!("rooms/{}", ticket.room_id()))
    ).await?;

    // Publish camera — the room publishes to both P2P peers and the relay.
    room.local_participant().broadcast(Camera).video().set_source(
        cam, VideoCodec::H264, [VideoPreset::P720],
    )?;

    // Event loop is the same as P2P-only rooms.
    loop {
        match room.recv().await? {
            RoomEvent::BroadcastPublished { participant, kind, broadcast } => {
                println!("{participant} published {kind} (source: {:?})",
                    broadcast.source());
                // broadcast.source() → Direct | Relayed | Both
            }
            _ => {}
        }
    }
}
```

### How it works

When `RoomRelay` is provided:

1. **Publishing**: When `local_participant().broadcast(Camera)` publishes, the room
   actor does two things:
   - P2P: `session.publish("camera", consumer)` on each direct peer session
   - Relay: `relay.session.publish("{prefix}/{my_id}/camera", consumer)` on the
     relay session

2. **Subscribing**: The room actor watches for broadcasts from two sources:
   - P2P: gossip KV announces → `connect_and_subscribe()` per peer
   - Relay: `relay.watch_prefix("{prefix}")` → yields
     `("{participant_id}/camera", broadcast)` as participants publish

3. **Participant discovery**: On the relay, participant identity is extracted from
   the path prefix (`"{participant_id}/camera"` → ParticipantId + BroadcastKind).
   On P2P, it comes from the session's remote_id.

### Naming on the relay

For a room with prefix `"rooms/abc123"`:

```
rooms/abc123/{participant_id_short}/camera     → Camera broadcast
rooms/abc123/{participant_id_short}/screen     → Screen broadcast
rooms/abc123/{participant_id_short}/{custom}   → Named broadcast
```

The `watch_prefix("rooms/abc123")` call strips the prefix, yielding:
```
"{participant_id_short}/camera"
"{participant_id_short}/screen"
```

The room actor parses these into `(ParticipantId, BroadcastKind)`.

### Relay-only room (no P2P)

For use cases where P2P is not desired (firewalled environments, large rooms):

```rust
let room = live.join_room_with_opts(RoomTicket::generate(), RoomOpts::new()
    .relay(relay.clone(), "rooms/my-room")
    .path_preference(PathPreference::RelayOnly)
).await?;
```

No gossip, no direct connections. All media flows through the relay.

---

## Use Case C: Hybrid P2P + Relay with Deduplication

```rust
let relay = live.connect_relay(relay_addr).await?;

let room = live.join_room_with_opts(ticket, RoomOpts::new()
    .relay(relay, format!("rooms/{}", ticket.room_id()))
    .path_preference(PathPreference::PreferDirect)
).await?;

loop {
    match room.recv().await? {
        RoomEvent::BroadcastPublished { participant, kind, broadcast } => {
            // The broadcast is deduplicated. Even if the same participant's
            // camera is available both via P2P and relay, you get ONE event
            // with ONE RemoteBroadcast.
            let video = broadcast.subscribe_video::<DefaultDecoders>()?;

            // Query how this broadcast is being delivered:
            match broadcast.delivery() {
                Delivery::Direct => println!("via P2P"),
                Delivery::Relayed => println!("via relay"),
            }
        }
        RoomEvent::PathChanged { participant, kind, delivery } => {
            // Fired when a broadcast switches between direct and relayed.
            // The VideoTrack/AudioTrack continue seamlessly — no re-subscribe
            // needed.
            println!("{participant}/{kind} now via {delivery:?}");
        }
        _ => {}
    }
}
```

### How deduplication works

The room maintains a participant map keyed by `ParticipantId`. When a broadcast
arrives from both P2P and relay for the same `(ParticipantId, BroadcastKind)`:

1. **Same participant, same broadcast kind** → deduplicate.
2. **Path preference** decides which source to use for media delivery.
3. The `RemoteBroadcast` returned to the user wraps the active source. If the
   preferred source becomes unavailable, it falls back to the other.
4. `PathChanged` event fires when the active delivery path switches.

### Implementation sketch

```rust
struct DeduplicatedBroadcast {
    direct: Option<SubscribeBroadcast>,    // from P2P session
    relayed: Option<SubscribeBroadcast>,   // from relay session
    preference: PathPreference,
    active: Watchable<Delivery>,
}

impl DeduplicatedBroadcast {
    fn active_inner(&self) -> &SubscribeBroadcast {
        match self.preference {
            PreferDirect => self.direct.as_ref().or(self.relayed.as_ref()).unwrap(),
            PreferRelay => self.relayed.as_ref().or(self.direct.as_ref()).unwrap(),
            DirectOnly => self.direct.as_ref().unwrap(),
            RelayOnly => self.relayed.as_ref().unwrap(),
        }
    }
}
```

`RemoteBroadcast` wraps `DeduplicatedBroadcast` instead of a single
`SubscribeBroadcast`. Subscribe calls delegate to the active inner source.

**Seamless switching**: When the active source changes (e.g. direct connection
drops, relay still works), the `RemoteBroadcast` switches internally. Existing
`VideoTrack`/`AudioTrack` handles become stale — the user needs to re-subscribe
(the `PathChanged` event signals this). Future work could make switching truly
seamless by staging the new decoder before swapping.

---

## New Types Summary

| Type | Module | Purpose |
|------|--------|---------|
| `Relay` | `iroh_live::relay` | Connection to a moq-relay |
| `RelayWatcher` | `iroh_live::relay` | Watch announces under a prefix |
| `RoomOpts` | `iroh_live::relay` | Options for room creation (relay, preferences) |
| `RoomRelay` | `iroh_live::relay` | Relay config within RoomOpts |
| `PathPreference` | `iroh_live::relay` | Direct vs relay preference |
| `Delivery` | `iroh_live::relay` | How a broadcast is being delivered |
| `PathChanged` | `iroh_live::room` | New RoomEvent variant |

## Changes to Existing Types

These are **additive only** — no existing signatures change:

| Type | Addition |
|------|----------|
| `Live` | `connect_relay(addr)`, `join_room_with_opts(ticket, RoomOpts)` |
| `RemoteBroadcast` | `delivery() -> Delivery` |
| `RoomEvent` | New variant `PathChanged { participant, kind, delivery }` |

## Compatibility with Current Design

The current API sketch (`3-sketch.md`) and impl plan (`4-impl.md`) are
relay-compatible as-is because:

1. **Session-scoped publishing** (see §4.4a of impl plan) means room
   broadcasts don't leak. Relay publishes go through the relay session only.

2. **Flat naming on P2P** (`"camera"`, `"screen"`) is fine — the relay adds the
   participant prefix only on the relay path. The Room abstraction maps between them.

3. **`RemoteBroadcast` wraps `SubscribeBroadcast`** — the deduplication layer sits
   between them. `subscribe_video()`/`subscribe_audio()` delegate to the active
   inner source.

4. **`BroadcastKind` mapping** works for both: `"camera"` (P2P) and
   `"{pid}/camera"` (relay, after prefix stripping) both map to `Camera`.

5. **`RoomEvent::BroadcastPublished`** works for both sources — the room actor
   emits it regardless of whether the broadcast came from P2P or relay.

No changes needed to `3-sketch.md`, `4-impl.md`, or `6-examples.md` for relay
to be additive.

## Naming Convention Reference

### P2P session (one session = one participant)

```
Broadcast name    BroadcastKind     Participant
"camera"          Camera            session.remote_id()
"screen"          Screen            session.remote_id()
"slides"          Named("slides")   session.remote_id()
```

### Relay session (shared, all participants)

```
Relay path                              Parsed as
"{prefix}/{pid}/camera"                 (ParticipantId(pid), Camera)
"{prefix}/{pid}/screen"                 (ParticipantId(pid), Screen)
"{prefix}/{pid}/slides"                 (ParticipantId(pid), Named("slides"))
```

After `watch_prefix("{prefix}")` strips the prefix:
```
Announced path        Parsed as
"{pid}/camera"        (ParticipantId(pid), Camera)
"{pid}/screen"        (ParticipantId(pid), Screen)
```

### Call over relay (if needed)

```
"{call_id}/{pid}/camera"    → Call participant's camera
"{call_id}/{pid}/screen"    → Call participant's screen
```

Same pattern — call_id is the relay prefix. Not a priority but falls out
naturally.

---

## Open Questions

1. **ParticipantId encoding in paths**: `ParticipantId` wraps `EndpointId` which
   is a public key. Full hex is 64 chars — too long for paths. Use `fmt_short()`
   (first 10 chars)? Or a hash? Short IDs risk collision in large rooms.

2. **Relay authentication**: moq-relay uses JWT tokens with path-based scoping.
   Who generates these tokens? Should `Live` or `Room` handle token acquisition,
   or is that the user's responsibility?

3. **Relay discovery**: How does the user find the relay address? Hardcoded,
   DNS-based, or embedded in `RoomTicket`?
   Proposal: add optional `relay: Option<EndpointAddr>` to `RoomTicket`.

4. **Re-subscribe on path switch**: When delivery switches from direct to relayed,
   existing `VideoTrack`/`AudioTrack` handles become stale. Should we auto-reconnect
   or require the user to re-subscribe on `PathChanged`?

5. **Relay-only rooms without gossip**: `join_room_relay_only` doesn't use gossip.
   How does participant discovery work? Pure announce-watching on the relay prefix.
   No gossip KV needed. But this means no out-of-band state (like participant
   metadata) unless we encode it in the catalog.

6. **Bandwidth deduplication**: If both direct and relayed paths are active, are we
   receiving double the data? With `PreferDirect`, we should only subscribe via the
   preferred path and keep the other as a fallback (subscribe on failover only).
