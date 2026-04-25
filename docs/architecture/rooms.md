# Rooms

| Field | Value |
|-------|-------|
| Status | beta |
| Applies to | iroh-live, iroh-live-relay |

A room is a multi-party broadcast session. Every participant
publishes some number of named broadcasts and subscribes to every
other participant's broadcasts under the same name. The
`iroh-live` API hides the choice of transport: peers can find
each other through gossip, through a relay, or both, and the
underlying multi-source subscription picks the active transport
per peer based on the configured policy.

This page describes the room layer as it stands today: the
public types, the wire-level paths, the three room modes, the
state the actor maintains, the cap and timeout knobs, and the
trust assumptions.

## Public types

`iroh_live::Live` is the entry point. It holds an iroh endpoint,
a MoQ actor, an optional gossip handle, and a router. The room
layer uses every one of those.

`Room` and `RoomHandle` form a split-style API. `Room` owns the
event stream; `RoomHandle` is the cloneable publish surface.
`Room::split` separates them so an event-consuming task can
receive `RoomEvent`s while another task publishes. Both halves
are produced by `Room::new` (or the builder
`RoomBuilder::spawn`) and dropped together when the underlying
actor task is aborted.

`RoomTicket` is the on-the-wire description of a room: a topic
id, a list of bootstrap peers, an optional relay attachment, and
a `RoomMode` discriminator. Tickets serialise through
`iroh_tickets::Ticket` (postcard plus base32 prefix) and
`from_bytes` validates against `MAX_BOOTSTRAP_PEERS` plus the
embedded `RelayOffer`'s own `validate()` before returning.

`Subscription` is the multi-source-aware subscriber. Each
subscription tracks a `SourceSet` of candidate transports
(direct peer, relay, or both), runs an attach loop that opens
sessions in parallel via `FuturesUnordered`, and exposes the
active source through a `Watchable<Option<SourceId>>`. The
`SelectionPolicy` (default `PreferOrdered`) chooses which
attached candidate the consumer reads from. Subscriptions are
cloneable; clones share the actor task.

`SourceSet` and `SourceSetHandle` are the source list and its
mutation handle. The handle wraps the set in a `Mutex` (for
ordered mutation) and a `n0_watcher::Watchable` (for change
notification). `update`, `push`, and `remove` are atomic; the
post-mutation snapshot is published through the watchable so
subscribers react.

`TransportSource` enumerates the two source kinds (`Direct` for
an iroh endpoint, `Relay` for a `RelayTarget`). `SourceId` is
the stable identifier used for dedup and active-source tracking.
A relay is identified by `(endpoint, path)` so two relay sources
on the same endpoint with different paths coexist.

`RelayOffer` is the wire-level description of a relay
attachment: an endpoint id, a URL path, and an optional opaque
api key. It rides inside tickets and gossip `PeerState`
messages. The `Debug` impl elides the api key. `validate()`
enforces a 256-byte path cap and rejects query, fragment, and
control characters.

`RelayTarget` is the runtime form built from an offer plus
defaults. `connect_relay` opens an HTTP/3-over-iroh session to
`https://<endpoint>/<path>?jwt=<api_key>`.

`SeamlessMediaTracks` is the video and audio surface returned by
`Subscription::media`. The video side wraps a single
`FrameReceiver<VideoFrame>` that survives across active-source
swaps; the audio side is rebuilt on swap (with documented gap).
Both layers run their own swap task that observes the
subscription's active source and rebuilds the inner pipeline
when needed.

The room actor itself is private. It is spawned by
`RoomBuilder::spawn`, owns every other room-scoped resource
(gossip subscription, relay sessions, active subscriptions,
active publishes), and exits when its api inbox is closed or
both discovery streams drop.

## Wire-level paths

A peer's broadcast has one logical name (`"camera"`, `"screen"`)
and several wire forms. The forms converge so a multi-source
subscription can use one broadcast name across direct and relay
candidates.

| Path | Format | Producer | Consumer |
|------|--------|----------|----------|
| Direct broadcast | `<peer_id>/<broadcast_name>` | `Live::publish_broadcast_producer` registers this globally on the MoQ actor. | Direct iroh subscribers issue `session.subscribe(<peer_id>/<broadcast_name>)`. |
| Relay discovery root | `/room/<topic_hex>` | `RelayDiscovery::spawn` opens an H3 session at this path. | The relay's primary origin announces every `<peer_id>/<broadcast_name>` under this root. |
| Relay publisher root | `/room/<topic_hex>/<peer_id>` | `RelayPublisher::spawn` opens an H3 publisher session at this path. | The relay's `OriginProducer` mounts the publisher under this root, so a bare `publish("camera", ...)` on the publisher session lands at `/room/<topic_hex>/<peer_id>/camera` on the primary origin. |
| Gossip key | `b"s"` (`PEER_STATE_KEY`) | The room actor writes its `PeerState` here on every refresh tick. | All members of the gossip topic receive every other peer's `PeerState` via `iroh_smol_kv`. |
| Relay JWT carrier | `?jwt=<token>` | `RelayTarget::url` appends the api key to the H3 URL. | The relay's auth layer extracts and verifies the token. |

`<topic_hex>` is the lower-case hex of the 32-byte topic id.
`<peer_id>` is the full canonical 32-byte iroh endpoint id, base32
encoded, so the segment round-trips through `EndpointId::from_str`.

`PeerState` is postcard-encoded: `(broadcasts: Vec<String>,
display_name: Option<String>, relay: Option<RelayOffer>)`. The
field layout is positional and append-only;
`skip_serializing_if` is forbidden because postcard would drop
the length byte and misalign the reader.

## Room modes

`RoomMode` selects which discovery and transport channels the
actor uses.

**`Gossip`.** Discovery via `iroh_gossip` topic, transport via
direct iroh sessions. Each peer writes its `PeerState` to the
shared KV; every other peer reads it. The optional relay on the
ticket is carried as a hint for direct subscribers but not used
as a transport. Trust assumption: gossip members are the trust
boundary; no relay required.

**`Relay`.** Discovery and transport through the relay only.
The actor opens two sessions: a `RelayDiscovery` rooted at
`/room/<topic_hex>` to read announces, and a `RelayPublisher`
rooted at `/room/<topic_hex>/<my_id>` to push broadcasts. No
gossip is used. Trust assumption: the relay is the single source
of truth; the operator mints tokens that scope `publish` claims
to `room/<topic>/<peer_id>/`.

**`Hybrid`.** Both channels, deduplicated. The actor runs gossip
and relay discovery in parallel. A `RoomEvent::RemoteAnnounced`
is emitted at most once per `(peer, broadcast)` pair; the
`announced` set in the actor filters duplicates across the
channels. The default `RoomSubscribeMode::PreferDirect` orders
direct first and relay second in every per-peer source set, so
the `PreferOrdered` policy picks direct when available and
falls over to relay when direct fails. Trust assumption: gossip
provides the roster and the relay is a transport fallback;
operators still mint per-peer tokens for the relay.

A ticket can be promoted at construction time:
`RoomTicket::new(topic, [bootstrap])` is `Gossip`,
`with_relay(offer)` promotes `Gossip` to `Hybrid`, and
`for_relay(offer)` produces a `Relay`-only ticket.

## Lifecycle

### Joining a room

`Live::join_room(ticket)` calls `RoomBuilder::new(ticket).spawn`
which validates mode preconditions (gossip present for `Gossip`
or `Hybrid`, relay attached for `Relay` or `Hybrid`), spawns the
actor, and returns the `Room` and `RoomHandle` pair.

The actor opens the channels its mode requires:

- `Gossip` and `Hybrid` open an `iroh_smol_kv` subscription on
  the topic with `Filter::ALL` and `SubscribeMode::Both`. The
  subscription uses `stream_raw()` so the actor sees both
  `Entry` and `Expired` items.
- `Relay` and `Hybrid` call `RelayDiscovery::spawn` and
  `RelayPublisher::spawn`. Both are reconnect-aware wrappers
  that open the initial session synchronously (so a
  configuration error fails the join) and run the rest of their
  lives in background tasks.

The actor seeds its own `PeerState` with one `update_kv()` and
sets a refresh interval at one third of the configured KV
horizon (default 40 seconds against a 2-minute horizon). The
refresh tick is a `select!` arm in the main loop.

### Publishing a broadcast

`RoomHandle::publish` sends `ApiMessage::Publish { name,
producer }` to the actor. The actor:

1. In `Gossip`/`Hybrid`, registers `<my_id>/<name>` on the
   global MoQ actor via `Live::publish_broadcast_producer`. Every
   inbound iroh session sees the announce.
2. In `Relay`/`Hybrid`, calls `RelayPublisher::publish(name,
   consumer)`. The publisher inserts the pair into its tracked
   map and issues a bare `session.publish(name, consumer)` on
   the current relay session. After every reconnect, the
   publisher re-issues every tracked publish.
3. Inserts an `ActivePublish` entry, pushes a future on
   `publish_closed` that waits for the producer's `closed()`
   signal, and calls `update_kv` so the new broadcast appears
   in the next gossip refresh.

When the producer is dropped, the `publish_closed` future
resolves; the actor calls `RelayPublisher::unpublish(name)` and
removes the `ActivePublish` entry.

### Subscribing to a remote broadcast

Discovery surfaces a `(peer, broadcast_name)` pair through one
of:

- A gossip KV update with a fresh broadcast name in
  `PeerState.broadcasts`.
- A `RelayDiscoveryEvent::Announce` from the relay's primary
  origin announce stream.

The actor calls `ensure_subscription(remote, name)`. That
method builds a per-peer `SourceSet` via `sources_for_peer`:

- `Gossip`: direct only.
- `Relay`: room relay only.
- `Hybrid`: direct, room relay, and the peer's own relay hint
  (deduped by `SourceId` if the hint matches the room relay).

`Live::subscribe(set, wire_name)` spawns a `Subscription`. Every
candidate runs an `attach_source` future in parallel: direct
sources call `transport.connect(peer)` plus
`session.subscribe(name)`; relay sources call `connect_relay`
and the same `subscribe`. Each successful attach inserts an
`AttachedSubscription` (session, broadcast, signals watcher,
close watchdog) into the subscription's state map and emits
`SubscriptionEvent::Attached`.

When the policy picks a new active id, the subscription emits
`SubscriptionEvent::ActiveChanged`. The room actor's per-broadcast
watcher task forwards the event as `RoomEvent::BroadcastSwitched
{ remote, broadcast_name, via_relay }`.

The first attach drives the room's `BroadcastSubscribed` event,
which carries the active session and the `RemoteBroadcast`. The
consumer reads decoded media off that broadcast.

### Peer departure

Three paths converge to `RoomEvent::PeerLeft`:

1. **Subscription exhaustion.** The active-source watcher
   observes `ActiveChanged { current: None }` after at least
   one earlier active source. It signals the actor through a
   per-room `closed_tx` channel; the actor removes the
   `active_subscribe` entry and, if no other broadcasts remain
   for that peer, removes the entry from `known_peers` and
   `peer_relays` and emits `PeerLeft`.
2. **Relay unannounce.** The relay's announce stream reports
   the broadcast vanished. `handle_relay_discovery_event`
   removes the matching `active_subscribe` entry, clears the
   announced flag, and emits `PeerLeft` if no broadcasts
   remain.
3. **Gossip expiry.** The KV's apply-horizon timer surfaces a
   `SubscribeItem::Expired` event for a peer's `PeerState`.
   `handle_gossip_expiry` clears the relay hint, drops every
   subscription rooted at the peer, and emits `PeerLeft`. This
   path covers peers that joined and disappeared without ever
   serving a session, which the watcher path cannot detect.

Suppression: `BroadcastSwitched` is only emitted when the new
active is `Some`. The death window (current going to None)
flows through the close signal channel and ultimately through
`PeerLeft`; emitting a `BroadcastSwitched(via_relay=false)` for
a no-source state would falsely look like a swap to direct.

### Relay session reconnect

Both `RelayDiscovery` and `RelayPublisher` own a background
task that runs forever. Each task watches `session.closed()`,
backs off, and reopens. The `Backoff` schedule is 100 ms
initial, 2x multiplier, 10 s cap, with 50% to 100% jitter on
each step. The schedule resets to the initial step after a
session has been stable for 30 seconds, so a relay flap inside
the 30 s window does not exceed the cap.

`RelayPublisher` carries the active publishes across the
reconnect: on every successful connect, it re-issues every
tracked `(name, consumer)` pair against the new session. The
room actor sees no break: incoming announces still arrive on
the same `mpsc::Receiver`, outbound publishes resume
automatically.

`RelayDiscovery` has no state to re-issue. The relay's primary
origin re-announces every active broadcast on reconnect.

### Active source change

The video swap loop watches `subscription.watch_active()` and
on every transition rebuilds the decoder pipeline against the
new source's broadcast, attaching to the same `FrameReceiver`
through `FrameReceiver::new_sender`. Consumers see no gap in
frames; the swap boundary is the next frame the new pipeline
produces (the next keyframe on the new source). The codec must
match across sources; on a mismatch the swap is refused and
the consumer's existing pipeline keeps running on the old
source until it dies.

The audio swap loop is symmetric in shape but currently
rebuilds the audio track on swap. The path to closing that gap
reuses `OutputHandle`'s fade machinery (the same ramp that
declicks pause and resume).

## State maps

The room actor's state lives in five maps and one set, all
keyed by either `EndpointId` or `BroadcastId = (EndpointId,
String)`:

| Map | Holds | Grows on | Shrinks on | Cap |
|-----|-------|----------|------------|-----|
| `known_peers` | `EndpointId -> Option<display_name>` | first sight of a peer through gossip or relay announce | peer departure (any of the three paths) | `MAX_KNOWN_PEERS = 256`; the actor refuses new entries past the cap with a throttled warning |
| `peer_relays` | `EndpointId -> Option<RelayOffer>` | gossip update with a relay hint | peer departure | bounded by `known_peers` |
| `active_subscribe` | `BroadcastId -> ActiveSubscribe { multi, _event_watcher }` | `ensure_subscription` after `RemoteAnnounced` | unannounce, expiry, or subscription exhaustion | per-peer worst case is `MAX_PEER_BROADCASTS = 32` |
| `active_publish` | `String -> ActivePublish { global_consumer, _direct_wire }` | `ApiMessage::Publish` | producer `closed()` | none; bounded by application |
| `announced` | `HashSet<BroadcastId>` of pairs already surfaced as `RemoteAnnounced` | every emitted `RemoteAnnounced` | unannounce, expiry, exhaustion | bounded by `active_subscribe` |
| `closed_rx` | `mpsc::Receiver<BroadcastId>` for subscription-end signals | watcher tasks send when `active_id` goes `Some -> None` | actor drains | channel capacity is `MAX_KNOWN_PEERS * MAX_PEER_BROADCASTS` so a mass disconnect storm cannot drop signals |

Watcher tasks attached to subscriptions are owned by their
`ActiveSubscribe` entry through `AbortOnDropHandle`; dropping
the entry aborts the task. The closed-signal channel uses
`send().await` rather than `try_send` so a backed-up actor does
not silently lose lifecycle events.

## Knobs

Caps and timing values that bound the room's behaviour, all
visible in code:

| Knob | Default | Where | What it bounds |
|------|---------|-------|----------------|
| `MAX_BOOTSTRAP_PEERS` | 16 | `iroh-live/src/rooms.rs` | bootstrap list in `RoomTicket` |
| `MAX_KNOWN_PEERS` | 256 | `iroh-live/src/rooms.rs` | distinct peers tracked per room |
| `MAX_PEER_BROADCASTS` | 32 | `iroh-live/src/rooms.rs` | broadcasts per peer in a `PeerState` |
| `MAX_BROADCAST_NAME_LEN` | 128 | `iroh-live/src/rooms.rs` | per-broadcast name length |
| `MAX_DISPLAY_NAME_LEN` | 64 | `iroh-live/src/rooms.rs` | per-peer display name length |
| `MAX_RELAY_OFFERS` | 16 | `iroh-live/src/sources.rs` | relay offers per ticket |
| `MAX_RELAY_PATH_LEN` | 256 | `iroh-live/src/sources.rs` | relay offer path length |
| `RECONNECT_INITIAL` | 100 ms | `iroh-live/src/rooms/relay_session.rs` | first reconnect sleep |
| `RECONNECT_CAP` | 10 s | `iroh-live/src/rooms/relay_session.rs` | longest reconnect sleep |
| `RECONNECT_MULTIPLIER` | 2 | `iroh-live/src/rooms/relay_session.rs` | per-attempt growth factor |
| `STABLE_THRESHOLD` | 30 s | `iroh-live/src/rooms/relay_session.rs` | session lifetime that resets the backoff |
| KV horizon | 2 min | `RoomBuilder::kv_expiry` default | how long a peer survives without refresh |
| KV check interval | 10 s | `RoomBuilder::kv_expiry` default | scan cadence (note: `iroh_smol_kv` currently ignores this and runs apply-horizon on a 30 s timer) |
| KV refresh interval | horizon / 3 | computed in `Actor::new` | how often the actor re-writes its own `PeerState` |
| Backoff jitter | 50% to 100% multiplier | `iroh-live/src/rooms/relay_session.rs` | per-step variance to avoid thundering herd |

## Trust model

The relay sees every URL it serves. In room mode, the URL is
`/room/<topic>/<peer_id>/<broadcast>`, so a relay operator can
list every peer in every room they host. The relay also sees
every JWT presented to it. The relay does not see broadcast
contents (those flow through MoQ tracks, opaque at the relay
layer).

Gossip topic members see every other member's `PeerState`,
including the relay offer (with its `api_key`). A gossip
attacker can forge a `PeerState` for any peer id they choose
because gossip writes are signed by the writing peer's key only;
there is no namespace authority binding a peer id to gossip
write privilege. Display names are informational; broadcast
names are routing-only; the relay offer is treated as a hint
rather than authority.

Tickets carry their full payload. A `LiveTicket`'s binary form
holds every relay offer including its `api_key`. The URL form
strips relays so JWT material does not leak into scannable
representations. Tickets are validated at parse time
(`from_bytes`) so a hostile ticket cannot bypass the per-offer
caps.

The relay enforces two things: the JWT signature against its
configured JWK, and the path-against-claims prefix match (a
publisher with a `publish` claim of `/foo` may publish under
`/foo/...` and nothing else). The relay does not bind the
JWT's subject claim to the path's `<peer_id>` segment, so an
operator who wants per-peer integrity must mint a token per
peer with the publish claim scoped to
`room/<topic>/<peer_id>/`. A topic-wide token with claim
`room/<topic>/` lets any holder publish under any peer id
inside the topic. Path-segment-to-subject binding is tracked as
an upstream `moq-relay` change.

## Where to look

| Topic | File |
|-------|------|
| Room api | `iroh-live/src/rooms.rs` |
| Reconnect-aware relay sessions | `iroh-live/src/rooms/relay_session.rs` |
| Room ticket and modes | `iroh-live/src/rooms.rs` (ticket module) |
| Sources, source set, policies | `iroh-live/src/sources.rs` |
| Multi-source subscription actor | `iroh-live/src/subscription.rs` |
| Seamless media surface | `iroh-live/src/seamless.rs` |
| Relay target builder | `iroh-live/src/relay.rs` |
| Live ticket | `iroh-live/src/ticket.rs` |
| Relay server crate | `iroh-live-relay/src/lib.rs` |
| Remote broadcast tracks | `moq-media/src/subscribe.rs` |
| Room integration tests | `iroh-live/tests/room.rs`, `iroh-live/tests/room_relay.rs`, `iroh-live-relay/tests/relay_room.rs` |
