# Plan: rooms, gossip, and dynamic relay transport

Status: design + implementation. Supersedes the previous research-
only plan. Tracks both the core "rooms with optional relay" feature
and the broader "dynamic direct/relay switching" capability.

## Goal

Make every publish and every subscribe capable of using multiple
transports simultaneously, and able to switch between them at
runtime without the caller tearing down or re-opening anything.
Rooms and one-off "just watch" ticket subscribes use the same
primitive.

A publish can live on several sessions at once (direct peer
sessions, a relay H3 session, a future peer-relay session). The
publisher decides which sessions a given broadcast is announced on
and can change that set at any time. A subscribe can monitor
several announcement sources and route the actual decode through
whichever source currently wins. On degradation or disappearance,
decode is redirected to another source without the decoder being
rebuilt.

This also future-proofs for "every peer is optionally a relay" by
modelling each source as a first-class TransportSource rather than
hard-coding "direct" and "relay" as two special cases.

## Non-goals for this session

- Relay-side changes to moq-relay or on-wire MoQ protocol.
- A full peer-as-relay implementation (the model admits it; the
 implementation defers).
- Replacing gossip or the smol-kv layer.
- Changing the catalog on-wire format beyond adding optional fields
 to the room's gossip KV message.

## Background

See `worklogs/2026-04-24-rooms-gossip-relays.md` for the research
summary. Three facts drive the design:

1. MoQ is natively multi-origin. A publisher can ANNOUNCE the same
 broadcast path on several sessions; a subscriber sees multiple
 `(path, consumer)` pairs via `OriginConsumer::announced()` and
 picks one.
2. `moq_lite::BroadcastProducer::consume()` returns independent
 `BroadcastConsumer` handles, each of which can be handed to a
 different session. Publish to N sessions is already possible at
 the moq-lite level; nothing above it exposes this cleanly.
3. `frame_channel::new_sender()` already enables seamless pipeline
 swap for rendition switching. The same primitive applied one
 level up lets us swap the underlying `BroadcastConsumer` without
 tearing down the decoder.

## Concepts

### TransportSource

A single place where a broadcast is available or where a publish
can be directed. Concrete variants:

- `Direct { peer: EndpointId }` - a direct peer session. The caller
 can dial this peer and subscribe, or accept an incoming dial and
 publish on the resulting session.
- `Relay { target: RelayTarget }` - an H3 session to a moq-relay.
 The caller holds or opens the session, then publishes or
 subscribes on it.
- Future: `PeerRelay { peer: EndpointId, path: String }` - a peer
 that agreed to relay a specific namespace. Same wire shape as
 relay; the model already supports it as a first-class variant
 rather than a hack.

`TransportSource` is pure data: identity and how to reach it. It
carries no session state.

### SourceSet

Ordered list of candidate `TransportSource`s for a single broadcast
together with a policy that decides which one is currently active.
Held in a `Watchable<SourceSet>` so the publisher and the
subscriber can react to changes reactively. The ordering encodes
preference (index 0 is preferred); the policy evaluates health and
can override the ordering transiently.

### Broadcaster

Publisher-side handle that attaches one `BroadcastProducer` to many
sessions at once. Internally holds a map from `SourceId` to live
`BroadcastConsumer` clones and publishes them on the right sessions
as sources come and go. When a source is removed the consumer is
dropped, which ends the announce on that session.

### MultiSourceSubscription

Subscriber-side handle that holds several candidate sessions and
one output `RemoteBroadcast`. Tracks which source is "active" (the
one whose `BroadcastConsumer` is feeding the decoder). On source
change, it routes the new source's consumer into the same decode
pipeline via `FrameReceiver::new_sender()`.

Externally the caller sees one `Subscription`-shaped handle. The
switch is invisible above the moq-media layer.

### SelectionPolicy

A trait that evaluates candidate sources and picks one. Default
policy: prefer sources by declared order; hold on degraded sources
for a downgrade hysteresis window before falling to the next; when
a preferred source reappears, wait an upgrade hysteresis window
before switching back. Thresholds are tunable.

Implementations:
- `PreferOrdered` - pick the highest-priority source that is
 currently healthy. Default. Ordered list from the `SourceSet` is
 the preference.
- `Fixed(SourceId)` - pin to one specific source. Used for tests
 and for callers who do not want dynamic behaviour.

Future implementations can add loss-rate-based selection, latency
tiers, or load-aware selection; the trait is stable once shipped.

## Architecture

### Publisher side (Broadcaster)

```text
LocalBroadcast --producer--> Broadcaster
 |
 SourceSet (Watchable)
 |
 +----------------+---------------+
 v v v
 session_direct session_relay session_peer_relay
 (MoqSession) (MoqSession) (MoqSession)
```

Broadcaster owns a single `BroadcastProducer` and owns its consume
fan-out. For each source in the current set it calls
`session.publish(name, producer.consume())`. When a source is
removed it drops that specific consume handle, which ends the
announce on that session but leaves the producer intact for the
remaining ones.

This generalises what `Moq::publish` does today ("publish on every
session I have") to "publish on the subset of sessions this
broadcast has been configured for."

### Subscriber side (MultiSourceSubscription)

```text
SourceSet (Watchable) --+
 |
 v
 SelectionPolicy --picks--> active_source
 |
 v
 session_active.subscribe(name) --> BroadcastConsumer
 |
 +----------+----------+
 v v
 CatalogWatcher VideoTrack / AudioTrack
 (reads active's (new_sender swap on active change)
 catalog)
```

When the active source changes, the subscription:

1. Takes the new source's session (opening it if it is not already
 open; sessions are owned by the subscription).
2. Subscribes the broadcast name on that session, obtaining a new
 `BroadcastConsumer`.
3. Takes a fresh sender on each output `frame_channel` from the
 existing receivers via `FrameReceiver::new_sender()`.
4. Spawns new decode pipelines fed by the new consumer, writing
 into the same slots. Drops the old pipelines; their decoders
 stop when their `OrderedConsumer` closes.

Seamless switch at the decoded-frame level. The caller sees an
uninterrupted stream of frames. The switch boundary is the next
frame the new pipeline produces; for video that is the next
keyframe on the new source. Audio keeps flowing from the old source
until the new audio pipeline delivers its first frame.

### Announcement layer

For rooms: extend the gossip KV `PeerState` with an optional relay
hint. Each peer can advertise zero or one relay location that other
peers can use to reach their broadcasts.

```rust
struct PeerState {
 broadcasts: Vec<String>,
 display_name: Option<String>,
 /// Relay where this peer also publishes their broadcasts.
 /// When set, other peers MAY subscribe via this relay instead
 /// of (or in addition to) the direct path.
 relay: Option<RelayHint>,
}

struct RelayHint {
 endpoint: EndpointId,
 path: String,
 /// Optional JWT. When the room ticket was minted with a baked
 /// JWT, this is absent (peers resolve the JWT from the ticket).
 jwt: Option<String>,
}
```

`skip_serializing_if` stays off per the existing postcard rule. New
fields go at the end of the struct.

For one-off single-watch: `LiveTicket` grows an optional list of
alternative sources.

```rust
pub struct LiveTicket {
 pub endpoint: EndpointAddr, // canonical direct source
 pub broadcast_name: String,
 pub relays: Vec<RelayOffer>, // additional sources
}

pub struct RelayOffer {
 pub endpoint: EndpointId,
 pub path: String,
 pub jwt: Option<String>,
}
```

Subscribers construct a `SourceSet` from the ticket: the direct
endpoint goes first; each relay offer follows. The default
`PreferOrdered` policy does the right thing out of the box.

### Room extension

Today's `Room` becomes a caller of the new primitives rather than
the owner of bespoke subscribe code. Concretely:

1. The gossip KV delivers `PeerState` with relay hints.
2. For each `(remote, broadcast_name)` the room builds a `SourceSet`
 with `Direct { peer: remote }` plus `Relay { target }` for the
 peer's relay hint when present.
3. Room hands the `SourceSet` to `Live::subscribe_multi` and treats
 the returned `Subscription` the same as before.
4. On relay hint changes, the room updates the `SourceSet`
 in place (via its `Watchable`). The subscription re-evaluates
 and switches if policy says to.
5. For publishing, the room constructs a `Broadcaster` seeded with
 the direct accept-loop source; when the room was built with a
 relay target, the relay is also seeded. Callers can also
 dynamically `enable_relay()` / `disable_relay()` on a
 `RoomHandle`.

### Single-watch extension

`Live::subscribe_multi(SourceSet, broadcast_name)` is the entry
point. `Live::subscribe(remote, name)` and
`Live::subscribe_from_relay(target, name)` become thin wrappers
that build a singleton `SourceSet` and delegate. New callers that
want dynamic behaviour pass a richer `SourceSet` or call
`Live::subscribe_from_ticket(ticket)` which understands the ticket's
embedded relay offers.

## Commit strategy

Each step must build and pass tests in isolation.

1. **refactor(moq-media): swappable `BroadcastConsumer` for
 RemoteBroadcast**. Add an internal primitive that lets a
 subscriber swap the underlying `BroadcastConsumer` of a
 `RemoteBroadcast` without rebuilding it. No caller change.

2. **feat(iroh-live): `TransportSource`, `SourceSet`,
 `SelectionPolicy`**. Pure types. No callers yet. Tests for
 ordering and hysteresis.

3. **feat(iroh-live): `Live::subscribe_multi`**. Implementation of
 `MultiSourceSubscription`. Existing `subscribe` and
 `subscribe_from_relay` become wrappers. Tests: single-source
 parity, multi-source happy path.

4. **feat(iroh-live): `Broadcaster` for multi-source publish**.
 Tests: two-session fan-out, removal cancels the right announce.

5. **feat(iroh-live): extend `LiveTicket` with `relays`** and add
 `Live::subscribe_from_ticket`. Tests for serialize/deserialize
 compatibility across old and new fields.

6. **feat(iroh-live/rooms): `PeerState.relay` hint**. Room actor
 threads relay hints into `SourceSet`. `RoomBuilder` adds
 optional relay configuration (new; rooms currently have no
 builder). `Room::join` keeps working with default settings.

7. **feat(iroh-live/rooms): runtime relay toggling**.
 `RoomHandle::enable_relay(target)` and `disable_relay()`.
 Changes gossip announcement and broadcaster targets. Peers
 re-evaluate.

8. **feat(iroh-live-cli): flags for dynamic mode**. `irl play`,
 `irl record`, `irl publish` accept a ticket with embedded relay
 offers via the existing positional argument. The explicit
 `--relay` / `--endpoint-id` / `--api-key` flags remain for raw
 single-source use.

9. **test(iroh-live): end-to-end integration tests**. Four
 scenarios:
 - Single-watch starts via relay, direct becomes available, swap.
 - Single-watch starts direct, direct session closes, fall over
 to relay.
 - Three-peer room: two direct, the third reachable only via
 relay, all three see each other's broadcasts.
 - Mid-session publisher flip: room runs P2P, publisher enables
 relay, subscribers switch.

10. **docs**: module-level docs and a short `docs/rooms.md` guide
 covering the three room modes and ticket flows. Bundle with
 step 6 or 8; no standalone docs commit.

## Risks and open questions

- **Session ownership.** Today `Moq::connect` dedupes direct
 sessions across callers. If a room owns a session to a peer and
 another caller also connects, dedup returns the same session.
 Plan: let dedup stand; track session ownership via `Arc` semantics
 in the higher layers.

- **Catalog disagreement across sources.** A direct and a relay
 source for the same broadcast should deliver the same catalog.
 In practice they will when the publisher's encode pipeline is a
 single producer. Plan: the active source's catalog wins; on swap
 the adaptation layer re-picks the best rendition.

- **JWT lifetimes.** Room tickets carry room-scoped JWTs; operators
 mint them with a policy-appropriate expiry. No auto-refresh this
 session. Surface expiry in the API so callers can rotate.

- **Switch flapping.** Hysteresis prevents pathological oscillation
 under mixed signals. Defaults: downgrade after 500 ms of sustained
 bad signal, upgrade after 4 s of sustained good signal, matching
 the rendition-switching plan.

- **Ordering during swap.** Two sources may have different playout
 latencies. The subscribe pipeline rebases on the new source's
 PTS, so a brief jump is possible. Accept this the same way the
 rendition-swap pattern does; call it out in docs.

- **Peer-as-relay.** Not implemented. The data model handles it. A
 `PeerRelay` variant on `TransportSource` becomes real once a peer
 can advertise "I relay X" in a gossip message and accept pull
 requests on its own MoQ session. Separate plan.

## Adversarial review checklist

Filled in before implementation starts (logged in the worklog).

- [ ] The same broadcast published to two sessions: does the
 publisher fan-out double-count frames? Test first.
- [ ] When a session dies mid-subscribe, does policy pick another
 source fast enough to keep the frame_channel fed? Write a
 failing test before the implementation.
- [ ] When every source fails, what does the subscriber see? Define
 the surface.
- [ ] Gossip KV anti-entropy delivers updated `PeerState` when a
 peer flips relay on/off: measured latency acceptable for
 user-visible switching? Record what we observe.
- [ ] Tickets with stale relay offers: subscriber tries the relay,
 fails, falls back to direct. Covered by tests.

## Commit discipline

Conventional prefixes required: `feat:`, `fix:`, `refactor:`,
`test:`, `docs:`, `chore:`. Refactors land first with no behaviour
change. Feature commits add behaviour. Test changes land with the
code they exercise. No standalone doc-only commits.

## Phase 2: relay-only rooms and three-mode unification

### User stories

**Mode A (gossip + direct).** Two friends starting a call. No
infrastructure.

```rust
let ticket = RoomTicket::generate();
let room = live.join_room(ticket).await?;
room.publish("cam", &broadcast).await?;
```

**Mode B (relay-only).** Web app provider hosting users behind NAT.
No peer-to-peer; everything routes through the relay.

```rust
let ticket = RoomTicket::for_relay(relay_offer);
let room = live.join_room(ticket).await?;
room.publish("cam", &broadcast).await?;
```

**Mode C (hybrid: direct + relay fallback).** Streaming app where
direct is preferred when reachable, relay handles the long tail.

```rust
let ticket = RoomTicket::generate().with_relay(relay_offer);
let room = live.join_room(ticket).await?;
room.publish("cam", &broadcast).await?;
```

The user-facing surface is identical across modes. Mode selection
lives entirely on the ticket.

### Wire-level naming

A room's broadcasts ride a uniform name shape on the wire:

```
<peer_endpoint_id>/<broadcast_name>
```

For mode A (direct), the publisher registers
`Live::publish_broadcast_producer(format!("{my_id}/{name}"), ...)`.
The bare-name registration the prior plan envisioned becomes a
subdirectory on the publisher's MoQ origin. Direct subscribers see
the same `<my_id>/<name>` path as relay subscribers do, so the
multi-source [`Subscription`] can use one broadcast name across
sources without per-source name plumbing.

For modes B and C, the publisher opens an outbound H3 session to
the relay rooted at `room/<topic_hex>/<my_id>`. A bare `cam`
publish on that session lands at `room/<topic_hex>/<my_id>/cam`
on the relay's primary origin.

For modes B and C, the discovery session connects rooted at
`room/<topic_hex>` and consumes `<peer>/<name>` announces; the
relay strips the discovery root automatically.

`<topic_hex>` is the room's [`TopicId`] hex-encoded. The same
identifier doubles as the gossip topic id in modes A and C.

### Discovery

Mode A: gossip KV. Existing path. Each peer's `PeerState` carries
the user-facing names (e.g. `cam`); the room actor turns them into
`<peer>/<name>` for subscribe.

Mode B: relay's [`OriginConsumer::announced`]
(moq_lite::OriginConsumer::announced) stream filtered by prefix.
The room opens one discovery session rooted at `room/<topic>` and
walks the announce stream. Each `(path, Some(consumer))` becomes
"peer announced"; each `(path, None)` becomes "peer ended".

Mode C: both. The actor has both a gossip KV update stream and a
relay announce stream. Events from either source feed the same
`(peer, broadcast)` map; the actor dedupes. Either signal is
sufficient to start a subscription.

### Transport

Subscriptions use the unified `Live::subscribe` path. The
`SourceSet` per `(peer, broadcast)` is built mode-aware:

- Mode A: `[Direct(peer)]`.
- Mode B: `[Relay(target_room_root)]`. The broadcast name is
 `<peer>/<name>` (relative to the relay root).
- Mode C: `[Direct(peer), Relay(target_room_root)]`. Same name on
 both.

`PreferOrdered` keeps direct preferred when both work.

### Room actor refactor

`Discovery` enum captures the three modes:

```rust
enum Discovery {
 Gossip(GossipDiscovery),
 Relay(RelayDiscovery),
 Hybrid { gossip: GossipDiscovery, relay: RelayDiscovery },
}
```

Each variant implements one event-producing method. The actor's
top-level select polls events from each active variant and feeds
them into the same `(peer, broadcast)` reconciliation logic.

`RelayDiscovery` owns:
- The discovery session (rooted at `room/<topic>`).
- A spawned task that walks `OriginConsumer::announced` and forwards
 parsed `(peer_id, broadcast_name, present)` tuples through an
 mpsc.

`GossipDiscovery` is the existing gossip+KV plumbing renamed.

### Publisher refactor

`ActivePublish` gains both a direct entry (when gossip/hybrid) and
a relay entry (when relay/hybrid):

```rust
struct ActivePublish {
 /// Global publish under `<my_id>/<name>`. Present for modes A,
 /// C. Absent in mode B.
 direct: Option<BroadcastConsumer>,
 /// Outbound publisher session at `room/<topic>/<my_id>`,
 /// announcing `<name>` (relative). Present for modes B, C.
 relay: Option<RelayPublishHandle>,
 /// Held for close-detection regardless of mode.
 close_consumer: BroadcastConsumer,
}
```

`enable_relay`/`disable_relay` mutate the relay portion at runtime
in modes A and C. Mode B requires the relay; disable_relay is a
no-op (or returns an error).

### RoomTicket layout

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTicket {
 /// 32-byte identifier; doubles as gossip topic and relay
 /// path component.
 pub topic_id: TopicId,
 /// Gossip bootstrap peers; empty disables gossip.
 pub bootstrap: Vec<EndpointId>,
 /// Optional relay attachment. Required in `RoomMode::Relay`.
 pub relay: Option<RelayOffer>,
 /// Explicit mode. Constructors set this; callers may toggle
 /// to switch modes.
 pub mode: RoomMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum RoomMode {
 /// Discovery via gossip; transport direct (with optional
 /// relay fallback).
 Gossip,
 /// Discovery and transport via relay; no gossip.
 Relay,
 /// Discovery via both gossip and relay; transport via direct
 /// preferred, relay as fallback.
 Hybrid,
}
```

Constructors:

```rust
RoomTicket::generate() // Gossip + random topic
RoomTicket::new(topic, bootstrap) // Gossip explicit
RoomTicket::for_relay(offer) // Relay + random topic
RoomTicket::for_relay_at(topic, offer) // Relay explicit
ticket.with_relay(offer) // Gossip -> Hybrid, attach offer
ticket.with_bootstrap(peer) // add a bootstrap peer
```

### Mode validation

`Live::join_room` validates the ticket against the live instance:

- `RoomMode::Gossip` / `Hybrid` requires `Live` to have gossip
 enabled. Returns an error if not.
- `RoomMode::Relay` does not require gossip. Returns an error if
 the ticket's `relay` field is `None`.

### Tests

- Mode A: existing room.rs tests cover.
- Mode B: new test using a real moq-relay (the bridge tests
 already spin one up). Three peers join via relay, publish
 broadcasts, see each other's via the announce stream. No gossip.
- Mode C: extension of room_relay.rs to exercise both gossip and
 relay simultaneously.

### Risks

- Relay JWT scoping. The publisher's session needs to be allowed
 to publish at `room/<topic>/<my_id>`. The discovery session
 needs subscribe access to `room/<topic>`. The simplest token
 shape is: root `room/<topic>`, subscribe `[""]`, publish
 `["<my_id>"]`. Callers (or the relay operator) mint the token
 out of band; the room ticket carries the JWT verbatim.

 For the V1 we accept any JWT in the ticket and document the
 requirement. The permissive-mode tests do not require a JWT.

- Multiple sessions to one relay. Each `Subscription` opens its
 own H3 session. With N peers and M broadcasts each, that's
 N\*M sessions. For typical room sizes this is fine; for very
 large rooms, follow-up work may share a single session across
 subscriptions.

- Gossip + relay event dedup. In hybrid mode, both gossip and
 the relay announce stream may report the same broadcast. The
 actor deduplicates by `(peer, name)` and keeps a single
 `Subscription` per pair.

- Path leakage. The relay publisher session's path
 `room/<topic>/<my_id>` is encoded in its JWT/URL. A relay
 operator can see all peers in all rooms by watching their
 primary origin. This is the intended trust model.
