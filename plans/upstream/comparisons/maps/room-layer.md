# Room / transport-integration layer map

> Campaign: upstream | Kind: map | Read ../../0-overview.md first; index at ../0-index.md.

Scope: the `iroh-moq` crate (`iroh-moq/src/lib.rs`, ~572 LOC) and the `iroh-live`
crate (`iroh-live/src/{lib,live,rooms,call,subscription,ticket,types,util}.rs`
plus `rooms/publisher.rs`). This is the layer that binds MoQ-over-iroh transport
to a room/session/call API. Written for a refactor-planning effort; all claims
carry `file:line` evidence.

Critical up-front fact for the whole document: in this workspace **`moq_lite` is an
alias for `moq-net`**. The workspace manifest declares
`moq-lite = { package = "moq-net", version = "0.1.11" }`
(`Cargo.toml:55`). Every `moq_lite::…` path in `iroh-moq`/`iroh-live` therefore
resolves to the same `moq_net` crate that `moq-native` is built on. This is the
key to the overlap analysis in the final section.

---

## 1. `iroh-moq` - what the crate owns

`iroh-moq/src/lib.rs:1` describes itself as "MoQ transport layer over iroh".
Functionally it is a **MoQ-over-iroh transport adapter plus a per-node session
manager**: it wraps the iroh `Endpoint`, drives the moq-lite handshake over a
`web_transport_iroh::Session`, and runs an internal actor that deduplicates
connections, fans broadcasts out across sessions, and surfaces incoming
sessions. It is not a relay and has no room/gossip concept.

### Public surface

- `pub const ALPN: &[u8] = b"moq-lite-04";` (`iroh-moq/src/lib.rs:35`). Hardcoded
  single wire version. This exact string is `ALPN_LITE_04`, the first entry of
  `moq_net::ALPNS` (see §7).
- `Moq` (`iroh-moq/src/lib.rs:77`) - the handle. Core type:

  ```rust
  #[derive(Debug, Clone)]
  pub struct Moq {
      tx: mpsc::Sender<ActorMessage>,
      incoming_session_tx: broadcast::Sender<MoqSession>,
      shutdown_token: CancellationToken,
      origin: Origin,
      _actor_handle: Arc<AbortOnDropHandle<()>>,
  }
  ```

  Constructed by `Moq::new(endpoint: Endpoint)` (`iroh-moq/src/lib.rs:88`), which
  mints one `Origin::random()` per node (`:95`) - "One non-zero origin id
  identifies this node in broadcast announce hop chains … shared across every
  session" (`:91`) - and spawns the `Actor` (`:96-99`).
  Methods: `protocol_handler()` (`:110`), `publish(name, BroadcastProducer)`
  (`:118`), `published_broadcasts()` (`:130`), `connect(remote) -> MoqSession`
  (`:144`), `incoming_sessions() -> IncomingSessionStream` (`:160`),
  `shutdown()` (`:167`).
- `MoqProtocolHandler` (`iroh-moq/src/lib.rs:177`) - implements
  `iroh::protocol::ProtocolHandler` (`:197`); `accept()` wraps the raw iroh
  `Connection` in `web_transport_iroh::Session::raw(connection)` and runs
  `MoqSession::session_accept` (`:185-186`). This is the iroh-`Router` mount
  point.
- `MoqSession` (`iroh-moq/src/lib.rs:259`) - a live session. Core type:

  ```rust
  #[derive(Clone)]
  pub struct MoqSession {
      wt_session: web_transport_iroh::Session,
      _moq_session: Arc<moq_lite::Session>,
      publish: OriginProducer,
      subscribe: OriginConsumer,
  }
  ```

  `session_connect`/`session_accept` (`:294`, `:316`) build a `moq_lite::Client`
  / `moq_lite::Server` with `.with_publish(publish_prod.consume())` and
  `.with_consume(subscribe_prod)` and run the handshake over the WT session
  (`:301-304`, `:323-326`). `subscribe(name)` awaits
  `self.subscribe.announced_broadcast(name)` (`:354`); `publish(name, consumer)`
  calls `self.publish.publish_broadcast(...)` (`:362`). Also exposes
  `origin_producer()`/`origin_consumer()` (`:366`, `:371`) and `conn()` (`:342`).
- `IncomingSessionStream` / `IncomingSession` (`:208`, `:233`) - inspect
  `remote_id()` then `accept()`/`reject()`.
- Errors: `Error` (`:39`) and `SubscribeError` (`:54`).

### The actor (the real owned functionality)

`Actor` (`iroh-moq/src/lib.rs:406`) holds `sessions: HashMap<EndpointId,
MoqSession>` (`:413`), `publishing: HashMap<BroadcastName, BroadcastProducer>`
(`:411`), and `pending_connects` (`:415`). Its select loop (`run`, `:439`)
provides the three behaviours that distinguish this crate from a plain transport:

1. **Connection dedup** - `handle_connect` (`:545`) returns an existing session
   for an `EndpointId` if present (`:551`), and coalesces concurrent dials into
   one `pending_connects` entry (`:555-568`). One connection per remote node.
2. **Broadcast fan-out** - `handle_publish_broadcast` (`:529`) republishes a
   producer onto every current session (`:530-534`); `handle_session` (`:499`)
   replays all currently-published broadcasts onto each newly established session
   (`:501-503`). So `Moq::publish` is node-wide, not session-scoped.
3. **Session lifecycle + incoming fan-in** - new sessions are broadcast on
   `incoming_session_tx` (`:506`) and their close is tracked in `session_tasks`
   (`:512-526`), removing them from the map on exit (`:451-452`).

iroh usage: `Endpoint`, `EndpointAddr`, `EndpointId`, `Connection`,
`ProtocolHandler`/`AcceptError` (`:13-17`). moq usage: `BroadcastConsumer`,
`BroadcastProducer`, `Origin`, `OriginConsumer`, `OriginProducer` (`:18`) plus
`moq_lite::{Client, Server, Session}` inside `MoqSession`. Transport bridging is
entirely via `web_transport_iroh` (`:28`, `:185`, `:284`).

---

## 2. `iroh-live/src/live.rs` - top-level entry (300 LOC)

`Live` (`iroh-live/src/live.rs:23`) is the application handle:

```rust
#[derive(Clone, Debug)]
pub struct Live {
    endpoint: Endpoint,
    moq: Moq,
    gossip: Option<Gossip>,
    router: Option<Router>,
}
```

It composes an iroh `Endpoint`, an `iroh_moq::Moq`, an optional
`iroh_gossip::Gossip`, and an optional `iroh::protocol::Router`. Construction is
through `LiveBuilder` (`:48`): `Live::builder(endpoint)` (`:121`) or
`Live::from_env()` (`:154`), which binds an `Endpoint::builder(presets::N0)` with
`secret_key_from_env()` (`:155-158`). `LiveBuilder::spawn()` (`:93`) is where the
wiring happens: it optionally spawns `Gossip::builder().spawn(endpoint)`
(`:96`), always creates `Moq::new(endpoint.clone())` (`:102`), and if
`with_router` was set, calls `register_protocols(Router::builder(...))` then
`.spawn()` (`:110-113`).

`register_protocols` (`:167`) mounts the two protocol handlers onto a
`RouterBuilder`: `.accept(iroh_moq::ALPN, self.moq.protocol_handler())` and, if
gossip is enabled, `.accept(iroh_gossip::ALPN, gossip.clone())` (`:168-172`).

Key methods: `publish(name, &LocalBroadcast)` → `moq.publish` (`:204`);
`publish_broadcast_producer` (`:215`); `subscribe(remote, name) ->
Subscription` (`:229`) which dials via `moq.connect`, calls `session.subscribe`,
wraps the consumer in `RemoteBroadcast::new`, and returns
`Subscription::new(session, broadcast)` (`:236-240`); `subscribe_media[_with_decoders]`
(`:248`, `:265`); `join_room(ticket) -> Room` → `Room::new(self, ticket)`
(`:282`); `shutdown()` (`:290`). `transport()` (`:181`) and `gossip()` (`:186`)
expose the inner handles.

---

## 3. `iroh-live/src/rooms.rs` (695 LOC) + `rooms/publisher.rs` - the room layer

**Discovery/membership mechanism (the central redesign question): rooms are
formed by a shared iroh-gossip topic, and membership + broadcast announcements
are carried as a CRDT-style signed key-value state over that topic via
`iroh-smol-kv`.** There is no relay and no MoQ origin/announce hop involvement at
the room layer - MoQ is only used *after* discovery, to pull each announced
broadcast point-to-point.

### `Room` and handles

```rust
#[derive(Debug)]
pub struct Room {                    // rooms.rs:40
    handle: RoomHandle,
    events: mpsc::Receiver<RoomEvent>,
}

#[derive(Debug, Clone)]
pub struct RoomHandle {              // rooms.rs:54
    me: EndpointId,
    ticket: RoomTicket,
    tx: mpsc::Sender<ApiMessage>,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}
```

`Room::new(live, ticket)` (`rooms.rs:137`) requires gossip
(`live.gossip()…context("Cannot join room: Gossip is disabled")`, `:138-140`),
then spawns the room `Actor` (`:147-158`). `Room::split()` (`:189`) separates the
`RoomEvents` receiver from the cloneable `RoomHandle`. Public API is thin
message-passing to the actor: `publish`/`publish_producer` (`:73`, `:82` on the
handle; `:195`, `:204` on `Room`), `set_chat_publisher`/`send_chat`
(`:100`, `:114`), `set_display_name` (`:124`). `RoomHandle::ticket()` (`:63`)
returns a ticket with `bootstrap = vec![self.me]` so the ticket can seed new
joiners off this peer.

### How join / announce / discover / membership actually work

The room `Actor` (`rooms.rs:307`) owns the mechanism:

1. **Join / topic:** `Actor::new` calls
   `gossip.subscribe(ticket.topic_id, ticket.bootstrap.clone())` (`:333-335`) and
   wraps that gossip topic in an `iroh_smol_kv::Client::local(topic, …)` with
   anti-entropy and a 2-minute expiry horizon (`:336-346`). Each peer writes
   under its own `WriteScope` keyed by its secret key (`kv.write(me.clone())`,
   `:347`).
2. **Announce (publish):** on `ApiMessage::Publish` (`:467`) the actor publishes
   the producer node-wide through `live.publish_broadcast_producer` (`:471`),
   records it in `active_publish`, and calls `update_kv()` (`:484`).
   `update_kv` (`:585`) serialises a `PeerState { broadcasts, display_name }`
   (`:283`) with postcard and `put`s it under key `PEER_STATE_KEY = b"s"`
   (`:281`, `:592-595`). So "announcing" = writing your broadcast-name list into
   the shared gossip KV.
3. **Discover (subscribe):** the actor subscribes to KV updates
   (`kv.subscribe_with_opts(Subscribe { mode: Both, filter: ALL })`,
   `:368-374`). `handle_gossip_update` (`:508`) ignores its own writes (`:510`)
   and non-`PEER_STATE_KEY` keys (`:514`), deserialises the peer's `PeerState`
   (`:518`), and for each advertised broadcast name builds a
   `BroadcastId(EndpointId, String)` (`:299`, `:557`) and, if newly seen, kicks
   off `live.subscribe(remote, &name)` on the `connecting` `FuturesUnordered`
   (`:558-571`). On success it emits `RoomEvent::BroadcastSubscribed`
   (`:413`). This is the crucial auto-connect: gossip announcement →
   point-to-point MoQ dial+subscribe.
4. **Membership:** tracked in `known_peers: HashMap<EndpointId, Option<String>>`
   (`:313`). First sighting of a peer's KV state emits `RoomEvent::PeerJoined`
   (`:539-553`). Departure is inferred: when a subscribed broadcast closes
   (`subscribe_closed`, `:428`) and no broadcasts remain for that peer, the actor
   removes it from `known_peers` and emits `RoomEvent::PeerLeft` (`:434-441`).
   Peer state also naturally ages out via the KV `ExpiryConfig` horizon
   (`:341-344`). Chat rides on a per-broadcast MoQ chat track, re-enqueued on
   `chat_messages` (`:447-458`).

`RoomEvent` (`rooms.rs:245`) is the output enum: `RemoteAnnounced`,
`BroadcastSubscribed`, `PeerJoined`, `PeerLeft`, `ChatReceived`.

### `RoomTicket` (defined inline in `rooms.rs`, module `ticket`)

```rust
#[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
#[display("{}", iroh_tickets::Ticket::encode_string(self))]
pub struct RoomTicket {              // rooms.rs:617
    pub bootstrap: Vec<EndpointId>,
    pub topic_id: TopicId,
}
```

`generate()` mints a random `TopicId` with no bootstrap (`:634`); `new_from_env`
reads `IROH_LIVE_ROOM`/`IROH_LIVE_TOPIC` (`:646`). It implements
`iroh_tickets::Ticket` with `KIND = "room"` and postcard body (`:683-693`). A
room is therefore identified purely by its gossip `TopicId`; bootstrap peers are
just gossip entry points.

### `rooms/publisher.rs`

`RoomPublisherSync` (`publisher.rs:24`) is a thin wrapper over
`moq_media::publish::PublishCaptureController` that pushes capture producers into
a `RoomHandle`. `new` eagerly publishes the camera+audio producer as
`Broadcasts::Camera` (`:31-43`); `set_state` diffs opts and, on a new screen
producer, publishes it as `Broadcasts::Screen` (`:47-70`). All capture/encode
logic lives in `moq-media`; this file only bridges producers to
`RoomHandle::publish_producer`.

---

## 4. `call.rs`, `subscription.rs`, `ticket.rs`, `types.rs` - purpose + key types

**`call.rs` - `Call` (`call.rs:39`).** Standalone 1:1 call sugar over MoQ
primitives; the doc comment calls it "Pure sugar" and notes "Everything Call does
can be done directly with `Live::transport()` + `LocalBroadcast` +
`RemoteBroadcast`" (`:23-37`). Holds `{ session: MoqSession, local:
LocalBroadcast, remote: RemoteBroadcast, signals }`. `dial` connects via
`live.transport().connect` (`:59`); `accept` takes an existing session (`:72`);
shared `setup` (`:82`) publishes the local broadcast under the fixed name
`"call"` (`CALL_BROADCAST_NAME`, `:46`, `:83`), subscribes to the remote `"call"`
(`:85-91`), and auto-wires `spawn_stats_recorder` + `spawn_signal_producer`
(`:93-98`). `closed()` maps QUIC `ConnectionError` variants to `DisconnectReason`
(`:145-157`). Errors: `CallError` (`:11`).

**`subscription.rs` - `Subscription` (`subscription.rs:16`).** Bundles a
`MoqSession`, a `RemoteBroadcast`, and a `watch::Receiver<NetworkSignals>` into
one handle returned by `Live::subscribe`. Its `new` (`:28`) auto-wires the same
stats recorder + signal producer as `Call`. Convenience `media[_with_decoders]`
(`:62`, `:73`) delegate to `RemoteBroadcast`; `into_parts()` (`:84`)
destructures. This is the non-room subscribe result type.

**`ticket.rs` - `LiveTicket` (`ticket.rs:19`).** A *point-to-point broadcast*
ticket (distinct from `RoomTicket`): `{ endpoint: EndpointAddr, broadcast_name:
String, relay_urls: Vec<String> }`. It encodes connection info by postcard-
serialising the full iroh `EndpointAddr` and base64url-encoding it into the URI
`iroh-live:<base64url(postcard(EndpointAddr))>/<name>` (`serialize`, `:60-65`);
`deserialize` also accepts a legacy `name@base32` form (`:68-112`). Because it
embeds the whole `EndpointAddr`, it self-describes how to reach the publisher
(endpoint id + optional relay URLs). Tests assert it stays QR-sized (`:172`).
Note this type is **not re-exported from `iroh-live/src/lib.rs`** - only
`rooms::RoomTicket` is (see §6).

**`types.rs` - `DisconnectReason` (`types.rs:6`).** A small
`#[non_exhaustive]` enum `{ LocalClose, RemoteClose, TransportError }` with a
`Display` impl (`:15`); used by `Call::closed`.

---

## 5. `util.rs` (support, not in the enumerated list but load-bearing)

`secret_key_from_env()` (`util.rs:13`) reads `IROH_SECRET`;
`spawn_signal_producer` (`:46`) polls the QUIC connection's selected path every
200 ms and derives `NetworkSignals { rtt, loss_rate, available_bps,
congestion_events }` from `conn.paths()`/`selected.stats()` (`:63-97`);
`spawn_stats_recorder` (`:114`) records RTT/loss/bandwidth into
`moq_media::stats::NetStats`. Both are wired automatically by `Call`/`Subscription`.

---

## 6. `lib.rs` exports

`iroh-live/src/lib.rs` re-exports `Call`/`CallError`, `Live`/`LiveBuilder`,
`Subscription`, `DisconnectReason` (`:23-28`), plus `pub use iroh_moq as moq`,
`pub use iroh_moq::ALPN`, `pub use moq_media as media`, `pub use hang::catalog`
(`:18-21`). Modules `rooms`, `ticket`, `util` are public; `call`, `live`,
`subscription`, `types` are private modules whose types are re-exported.

---

## 7. moq-native iroh transport (for overlap analysis)

Read from the moq working tree at `/home/bit/Code/rust/moq/rs` (branch `main`).

### What `moq-native/src/iroh.rs` provides

- `EndpointConfig` (`iroh.rs:75`) - a `clap::Args`/serde config
  (`MOQ_IROH_*` env, secret as hex-or-file, `bind_v4/v6`, `disable_relay`). Its
  `bind()` (`:117`) builds the iroh `Endpoint` with the same `presets::N0` /
  `N0DisableRelay` (`:146-150`) and, crucially, registers **all** MoQ ALPNs plus
  H3: `moq_net::ALPNS` mapped to bytes, then `web_transport_iroh::ALPN_H3`
  (`:143-144`, `:151`).
- `Request` (`iroh.rs:167`) - an accepted incoming iroh connection, either raw
  QUIC (`moq_net::ALPNS.contains(alpn)` → `QuicRequest`) or WebTransport/H3
  (`ALPN_H3` → `H3Request`) (`accept`, `:177-196`); `ok()` completes the
  handshake and yields a `web_transport_iroh::Session` (`:199-210`).
- `connect(endpoint, url, addrs)` (`iroh.rs:231`) - parses an `iroh://<endpoint-id>`
  URL into an `EndpointAddr` (optionally with direct IPs), dials with
  multi-ALPN `ConnectOptions`, negotiates H3 vs raw QUIC, and returns a
  `web_transport_iroh::Session` (`:255-278`). This is the *same*
  `Endpoint → web_transport_iroh::Session` bridge `iroh-moq` performs in
  `MoqSession::connect`/`session_accept`.

### How it plugs into the moq-native public surface

`moq-native/src/lib.rs:50` gates `pub mod iroh` on the `iroh` feature and
re-exports `Client`/`Server`. `Client` (`client.rs:94`) holds an optional
`iroh: Option<web_transport_iroh::iroh::Endpoint>` (`:107`), set via
`with_iroh` (`:186`), and `connect_inner` branches on `url.scheme() == "iroh"`
to call `crate::iroh::connect(...)` then `self.moq.connect(session)`
(`client.rs:259-265`). `Server` (`server.rs:127`) holds `iroh:
Option<iroh::Endpoint>` (`:132`) set via `with_iroh` (`:227`) and multiplexes
`endpoint.accept()` into its unified `accept()` loop alongside quinn/quiche/noq/
websocket (`server.rs:292-301`, `:380-389`), producing a `Request` whose `ok()`
runs `self.server.accept(...)` (`:570-574`). `with_publish`/`with_consume` take
`moq_net::OriginConsumer`/`OriginProducer` (`client.rs:201-209`,
`server.rs:232-240`) - the very same origin types `MoqSession` threads through.

### Overlap assessment (evidence-backed)

The overlap is **substantial and concrete at the transport-adapter layer, and
they build on the identical underlying crate**:

- **Same crate underneath.** `iroh-live`'s `moq_lite` *is* `moq-net`
  (`Cargo.toml:55`), the crate moq-native calls `moq_net`. `iroh-moq`'s
  `moq_lite::{Client, Server, Session, Origin, OriginProducer, OriginConsumer}`
  and moq-native's `moq_net::{Client/Server via moq_net, OriginConsumer,
  OriginProducer, Session}` are the same types. The `.with_publish(origin)` /
  `.with_consume(origin)` handshake wiring in `MoqSession::session_connect`
  (`iroh-moq/src/lib.rs:301-304`) mirrors moq-native's `Client::with_publish`/
  `with_consume` (`client.rs:201-209`).
- **Same iroh→WT bridge.** Both wrap the iroh `Connection`/`Endpoint` in
  `web_transport_iroh::Session::raw(...)` and drive the moq handshake over it
  (`iroh-moq/src/lib.rs:185`, `:284`; `moq-native iroh.rs:273`,
  `:177-196`). Both use `presets::N0`/`N0DisableRelay` (`live.rs:155`;
  `iroh.rs:146-150`).
- **Same ALPN.** `iroh-moq` hardcodes `b"moq-lite-04"`
  (`iroh-moq/src/lib.rs:35`); that is literally `ALPN_LITE_04`, the first of
  `moq_net::ALPNS` (`moq-net/src/version.rs:22`, `:37`) that moq-native
  advertises and negotiates. moq-native additionally handles lite-03/legacy IETF
  versions and H3/WebTransport framing, which `iroh-moq` does not.
- **moq-native is already a workspace dependency.** `Cargo.toml:58` pins
  `moq-native = "0.17.1"`, and the sibling `iroh-live-relay` crate already uses
  `moq_native::{ServerConfig, ClientConfig, iroh::EndpointConfig, QuicBackend}`
  (`iroh-live-relay/src/lib.rs:46-63`). So the project already depends on and
  uses moq-native's transport for its relay - but `iroh-moq`/`iroh-live` bypass
  it and re-implement the iroh path directly.

**What `iroh-moq` owns that moq-native does *not* provide** (the genuine
non-overlap, and the reason it exists):

1. An iroh **`ProtocolHandler`** for the iroh `Router` (`MoqProtocolHandler`,
   `iroh-moq/src/lib.rs:177-204`). moq-native instead owns its own
   multi-transport `Server::accept()` loop and does not integrate with the iroh
   `Router`/`ProtocolHandler` model.
2. **Connection deduplication** - one `MoqSession` per `EndpointId`, with
   coalesced concurrent dials (`Actor::handle_connect`, `:545-571`).
3. **Node-wide broadcast fan-out** - publish once, auto-replayed onto every
   current and future session (`:501-503`, `:529-543`). moq-native's origins are
   configured per-session (`with_publish`/`with_consume`), with no cross-session
   fan-out.
4. A small **incoming-session stream / accept-or-reject** ergonomic
   (`IncomingSessionStream`/`IncomingSession`, `:208-252`).

Net: the ~120 lines of `MoqSession` handshake/connect code plus the ALPN/preset
choices duplicate `moq-native/src/iroh.rs` + `moq_net::Client/Server` almost
exactly (over the same `moq-net` crate the project already depends on). The
`Actor` (dedup + fan-out + Router `ProtocolHandler`, ~200 lines) is the part with
no moq-native equivalent. A redesign could keep the actor's session-manager
value while delegating the raw iroh↔moq handshake to `moq_native::iroh` +
`moq_net::{Client,Server}`, eliminating the hand-rolled version/ALPN handling and
converging on the transport already used by `iroh-live-relay`.
