# Room layer: analysis and redesign on moq origin/announce

Task 4 of the moq-alignment planning effort. Question: can iroh-live rebuild its
room layer on moq's origin, announce, and session primitives, and what does it
gain and lose by doing so?

Sources: [maps/room-layer.md](../upstream/comparisons/maps/room-layer.md) (current iroh-live room
design), [maps/moq-net-origin.md](../upstream/comparisons/maps/moq-net-origin.md) (moq net layer),
`plans/old/review-moq-usage.md` (prior ALPN finding), and the code itself
(`iroh-moq/src/lib.rs`, `iroh-live/src/rooms.rs`, moq working tree at
`/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`). On 2026-07-21 moq merged `dev`
into `main`, so the routing, subscription, session, and token features that
earlier drafts of this document called "dev-only" are now plain moq main. This
rewrite removes the main-vs-dev split entirely; the only version gate left is
that iroh-live still pins the crates.io `moq-net 0.1.11` / `moq-native 0.17.1`
release and picks these features up when it bumps to the next release cut from
main. Upstream's own concept documentation states that `announced(prefix)` "is
extremely useful for conference rooms, as you can live discover when
participants join and leave" and that moq-relay clustering is built on it
(`doc/concept/layer/moq-lite.md:60-65`). Upstream designed the announce bus for
exactly the job our gossip KV does today.

Terminology note: in this workspace `moq_lite` is an alias for `moq-net`
(`Cargo.toml:55`). Both names below refer to the same crate. moq net naming is
module-scoped: `origin::{Producer, Consumer}`, `broadcast::{Producer, Consumer,
Route}`, `track::{Consumer, Subscriber}`.

---

## 1. Current design, precisely

Three layers stack up today.

### 1.1 iroh-moq (572 LOC): transport adapter plus per-node session manager

The `Moq` handle (`iroh-moq/src/lib.rs:77`) wraps the iroh `Endpoint` and an
internal actor. `Moq::new` mints one `Origin::random()` per node, shared across
every session (`lib.rs:91-95`). The actor (`lib.rs:406`) owns three behaviors:

1. **Connection dedup.** One `MoqSession` per `EndpointId`; concurrent dials to
   the same remote coalesce into one pending entry (`handle_connect`,
   `lib.rs:545-571`).
2. **Node-wide broadcast fan-out.** `Moq::publish` republishes the producer
   onto every current session (`lib.rs:529-534`) and replays all published
   broadcasts onto each new session (`lib.rs:499-503`). Publishing is node
   scoped, not session scoped.
3. **Router integration and incoming fan-in.** `MoqProtocolHandler` implements
   `iroh::protocol::ProtocolHandler` (`lib.rs:177-204`); accepted connections
   are wrapped in `web_transport_iroh::Session::raw` and surfaced through
   `IncomingSessionStream` with accept-or-reject (`lib.rs:208-252`).

Around that sits duplication: `MoqSession::session_connect` and
`session_accept` (`lib.rs:294`, `lib.rs:316`) hand-roll the moq handshake,
about 120 lines that build a `moq_lite::Client`/`Server` with
`.with_publish(...)`/`.with_consume(...)` over a `web_transport_iroh::Session`.
This is the same bridge `moq-native/src/iroh.rs` provides, over the identical
`moq-net` crate, which the workspace already depends on through
`moq-native = "0.17.1"` (`Cargo.toml:58`) and already uses in
`iroh-live-relay`. The ALPN is a single hardcoded version,
`pub const ALPN: &[u8] = b"moq-lite-04"` (`lib.rs:35`), while moq-native
registers the full `moq_net::ALPNS` list plus `web_transport_iroh::ALPN_H3` and
negotiates among them (`iroh.rs:143-151`). The prior review flagged this as its
most concrete interop finding (`plans/old/review-moq-usage.md`, finding 2): the
const has since been bumped from lite-03 to lite-04, but the recommendation to
offer the full ALPN list on connect and register the handler for every entry
remains open.

### 1.2 rooms.rs (695 LOC): gossip topic plus signed KV, then point-to-point MoQ

A room is an iroh-gossip `TopicId`. The room `Actor` (`rooms.rs:307`)
subscribes to the topic with the ticket's bootstrap peers (`rooms.rs:333-335`)
and layers an `iroh_smol_kv::Client` over it with anti-entropy and a 2-minute
expiry horizon (`rooms.rs:336-346`). The mechanism:

- **Announce** means writing a postcard-serialized
  `PeerState { broadcasts, display_name }` (`rooms.rs:283`) into the KV under
  your own signed write scope, key `b"s"` (`rooms.rs:281`, `rooms.rs:592-595`).
- **Discover** means watching KV updates (`rooms.rs:368-374`); each advertised
  broadcast name becomes a `BroadcastId(EndpointId, String)` and, if newly
  seen, triggers `live.subscribe(remote, &name)`, a point-to-point MoQ dial
  plus a per-broadcast request on that single session (`rooms.rs:557-571`). MoQ
  announce is used only degenerately, to fetch a broadcast already known by
  name from the one direct session.
- **Membership** is `known_peers: HashMap<EndpointId, Option<String>>`
  (`rooms.rs:313`). First sighting of a peer's KV state emits
  `RoomEvent::PeerJoined` (`rooms.rs:539-553`). Departure is inferred: when a
  subscribed broadcast closes and no broadcasts remain for that peer, the actor
  emits `RoomEvent::PeerLeft` (`rooms.rs:428-441`), and stale state also ages
  out via the 2-minute KV horizon. Chat rides on a per-broadcast MoQ chat
  track (`rooms.rs:447-458`).

`RoomEvent` (`rooms.rs:245`) is `RemoteAnnounced`, `BroadcastSubscribed`,
`PeerJoined`, `PeerLeft`, and `ChatReceived`.

### 1.3 Tickets

`RoomTicket { bootstrap: Vec<EndpointId>, topic_id: TopicId }` (`rooms.rs:617`)
identifies a room purely by gossip topic; bootstrap peers are gossip entry
points, and `RoomHandle::ticket()` seeds new joiners with `bootstrap =
vec![self.me]` (`rooms.rs:63`). Separately, `LiveTicket` (`ticket.rs:19`)
carries a full `EndpointAddr` plus broadcast name for point-to-point
subscribes; it is not re-exported from `iroh-live/src/lib.rs`.

The key observation: gossip currently does two jobs, membership (who is in the
room, how do I reach them) and announcement (what broadcasts they offer). MoQ
can take over the second job natively; the first is the open question.

---

## 2. What moq main provides to build rooms on

All of the following is current moq main (HEAD `3a3e0ea8`), verified per commit
in [maps/moq-net-origin.md](../upstream/comparisons/maps/moq-net-origin.md). iroh-live picks it up when
it bumps its `moq-net` / `moq-native` pins to the next release cut from main.

- **Origin as relay identity.** `Origin { id: u64 }` is a non-zero 62-bit id
  used in hop chains, `OriginList` with `MAX_HOPS = 32`
  (`rs/moq-net/src/model/origin.rs:26-96`).
- **`origin::Producer` / `origin::Consumer` as the announcement bus.** An
  in-process path-tree of broadcasts plus a coalesced announce stream. Since
  #2396 the single ingress is `create_broadcast(path, route)`
  (`origin.rs:869`); the old `publish_broadcast` is gone. The consumer side
  yields `OriginAnnounce { path, broadcast: Option<broadcast::Consumer> }`
  through `announced()` (`origin.rs:1932`), where `None` is an unannounce and
  replacement is always delivered as unannounce-then-announce, never a swap in
  place.
- **`Route.announce` gates advertisement independently of existence** (#2396).
  `broadcast::Route` (`broadcast.rs:61`) carries `hops`, `cost`, and an
  `announce: bool` flag (internally named "live", hence the commit title "gate
  announce on Route.live"). `Route::new()` (`broadcast.rs:103`) builds an
  unannounced route reachable by exact path only; `Route::announced()`
  (`broadcast.rs:112`) advertises it. A non-announced broadcast stays reachable
  by exact path for subscribes and fetches but is invisible to `announced()`;
  `broadcast::Producer::set_route` toggles advertisement without touching the
  broadcast.
- **Path prefix scoping.** `Path` is boundary-safe, slash-delimited, and
  zero-copy (`rs/moq-net/src/path.rs`); `scope(&[prefix])` and
  `with_root(prefix)` on both producer (`origin.rs:907`, `origin.rs:960`) and
  consumer (`origin.rs:2002`, `origin.rs:2074`) restrict and strip a prefix.
  This is the primitive for room scoping, per-peer authorship enforcement, and
  the token-scoped session origin (section 5). Empty PATH is now legal and
  defaults to `""` across protocols (#2414, `lite/setup.rs`), so path
  construction that assumed a non-empty leaf must not special-case the empty
  case.
- **Cumulative-cost, loop-protected routing** (#2424). Every broadcast carries
  `hops: OriginList`; `create_broadcast` refuses a broadcast whose chain
  already contains the local origin, and the announce subscriber sends
  `AnnounceRequest { prefix, exclude_hop }`. Route selection orders by
  `(!announce, cost, hop_len, fnv_hash)`, lower wins: announced live sources
  first, then lowest cumulative `RouteCost`, then shortest chain, then a
  deterministic FNV tie-break so every node converges on the same route. The
  cost accumulates per link but restarts at zero at any node actively carrying
  the broadcast, so it reflects the transfers one more subscription would newly
  cause (marginal-cost routing); losers park as hot standbys promoted on route
  death. A client seeds its own production cost via `with_cost`.
- **Unannounce liveness, no linger** (#2419). Detaching the last source now
  closes the broadcast and unannounces synchronously: `detach_source` sets
  `closed = true` and aborts the moment `routes.is_empty()`
  (`origin.rs:1123-1145`). The old front-level `ROUTE_LINGER = 5s` grace that
  kept a dead path announced so a reconnecting session could re-attach
  transparently is removed. An unannounce fires immediately when the last route
  detaches.
- **Transparent subscription migration** (`model/resume.rs`). `resume::Producer`
  still splices per-session tracks into one logical track and switches at group
  boundaries, so a subscription survives its own route and connection changes
  with no dup or skip. With #2419 this stays seamless only if the replacement
  route attaches before the last old route detaches; the publish-side front no
  longer lingers to bridge the gap.
- **Caller-driven `(Session, Driver)` sessions.** Nothing is spawned behind
  your back; any executor drives the driver, `Session::stats()` snapshots
  connection stats, and the SETUP `Role` is derived from which origins were
  wired (`with_publisher` / `with_subscriber` / `with_origin`). `Role::Both` is
  unrepresentable; the wire carries `Option<Role>` and old peers decode `None`.
- **`Subscription { priority, ordered, latency_max, group_start, group_end }`**
  with correct aggregate merging across subscribers and a single publisher-side
  clamp of `latency_max` against the retention window.
- **moq-native iroh transport.** `EndpointConfig::bind` builds the iroh
  endpoint with the full ALPN list, `Client::with_iroh` plus `iroh://<endpoint-
  id>` URLs dial it, and `Server::accept` multiplexes iroh alongside the other
  transports. The iroh accept is now single-phase (section 5).
- **moq-token path-scoping** (#2416). Tokens carry a `root` plus per-role
  `publish` / `subscribe` prefix lists, and a signing key can carry an
  immutable `Scope` ceiling (section 5). This is the primitive that closes the
  announce-spoofing gap.

---

## 3. The redesign

### 3.1 iroh-moq shrinks to its genuine value

Target: `iroh-moq` keeps exactly the four things moq-native does not provide
(Router `ProtocolHandler`, connection dedup, node-wide fan-out via a shared
origin, incoming-session stream) and delegates everything else.

- `MoqSession::session_connect`/`session_accept` (`lib.rs:294-340`) are
  replaced by `moq_net::Client::connect` and `Server::accept` driven through
  `moq_native::iroh::connect` and `accept`. The roughly 120 lines of
  hand-rolled handshake die.
- The single hardcoded ALPN (`lib.rs:35`) is replaced by the full
  `moq_net::ALPNS` list plus `ALPN_H3`, registered per entry on the Router and
  offered as `additional_alpns` on connect, exactly as
  `plans/old/review-moq-usage.md` finding 2 recommends and as `iroh-live-relay`
  already does (`iroh-live-relay/src/lib.rs:60-74`). This is a wire-visible
  change; the e2e suite must be re-run.
- The actor's fan-out changes shape: instead of republishing each
  `broadcast::Producer` per session, the node holds one `origin::Producer` and
  wires every session with `with_publisher(origin.consume())` and a
  `with_subscriber` whose scoping follows the multi-room open question in 3.2
  (accept-and-filter in the actor is the recommendation; per-session scope
  covers the single-room case). Fan-out then falls out of moq's own announce
  machinery rather than actor bookkeeping, and the `publishing` map
  (`lib.rs:411`) becomes a set of paths in the origin tree, populated via
  `create_broadcast(path, Route::announced())`.

What the `IncomingSession` gating must become: the moq-native iroh accept is now
single-phase (section 5), so the pre-handshake accept-or-reject window
`iroh-moq` exposes today no longer maps onto a moq-native affordance.
Authorization moves onto the SETUP path plus token, verified as the session is
established. iroh-live's `IncomingSession` should therefore stop pretending to
be a pre-handshake veto: either drop the reject arm and let unauthorized
sessions fail at token verification, or keep a coarse endpoint-id allow/deny
that runs before `accept` at the iroh connection layer. Fine-grained
"which room may this peer publish under" enforcement belongs to the scoped
session origin, not to a hand-rolled accept gate.

### 3.2 Rooms move announcement onto moq announce

The mapping (maps/moq-net-origin.md section 7.1):

- One shared per-node `origin::Producer`, owned by the `Moq` actor, backed by
  the existing per-node `Origin::random()` (`lib.rs:95`).
- Each peer publishes its broadcasts at `<room>/<endpoint-id>/<name>` via
  `create_broadcast(path, Route::announced())`. The room id replaces the gossip
  `TopicId` as the path prefix; a hash or encoded id keeps it within
  `MAX_PARTS` and reasonable length. A peer that wants to be reachable but not
  advertised (for example a pull-only source) uses `Route::new()` instead, and
  toggles advertisement later with `set_route`.
- Each room member watches a consumer scoped to `<room>/` via
  `with_root`/`scope` and reacts to `announced()`: a `Some` broadcast under
  `<peer>/<name>` is a broadcast appearing, a `None` is it going away.
  Subscribing is then a `broadcast::Consumer` in hand immediately, no separate
  dial-then-request round trip per broadcast.
- **Authorship enforcement per session.** On accept, the subscriber-side
  producer handed to the session is scoped to `<room>/<remote-endpoint-id>/`, so
  a peer can only announce under its own id. This is precisely what
  `scope`/`with_root` exist for (`origin.rs:907`, `origin.rs:960`). Section 5
  strengthens this with a cryptographic token scope; the path scope is the
  mechanism, the token is the credential. It deliberately forbids transitive
  re-announcement; relaxing it for relay peers is a policy decision, not a
  mechanism change (see section 4). The static scope conflicts with connection
  dedup when two peers share more than one room; see the open design question
  under phase 2 (section 6).
- **Leave liveness.** When a session closes, moq unannounces everything it
  contributed, and consumers see `None` immediately (#2419, no linger).
  Deliberate leave is dropping the broadcast producer, which unannounces the
  same way. This replaces both the broadcast-close inference and the 2-minute
  KV horizon with a deterministic, prompt signal.

**Gossip's remaining role.** Announce presupposes sessions; someone must decide
whom to dial first, and moq has no membership or peer-sampling layer. Two
serious variants:

- **Variant A, gossip retained for bootstrap only.** The ticket keeps its
  `TopicId` and bootstrap list; gossip carries only "peer X is in room R"
  presence beacons (or even just the implicit neighbor set from
  `GossipTopic::neighbors`), and every discovered peer is dialed once. All
  broadcast announcement, membership events, and metadata move off the KV.
  This preserves today's join UX (a ticket with one bootstrap peer suffices,
  and membership heals epidemically even if the bootstrap peer leaves) at the
  cost of keeping the gossip dependency and a second protocol on the wire.
- **Variant B, no gossip.** The ticket's bootstrap peers are dialed directly
  as moq sessions. New peers learn about further members transitively: announce
  paths contain `EndpointId`s, so a joiner connected to B sees `<room>/<A>/
  camera` and can dial A directly. This works if and only if some connected
  peer re-announces others' broadcasts, which the per-session authorship scope
  in 3.2 forbids by default; the honest options are a deliberate announce-only
  forwarding policy (moq-net does not provide one out of the box, so we would
  build it: forward announce paths, not media, with hop protection), or
  accepting that the bootstrap peers are a de facto rendezvous and must stay
  online, or a relay/SFU as the rendezvous. Variant B is cleaner (one protocol)
  but strictly weaker on churn: if all bootstrap peers leave, the room can
  partition for new joiners, which gossip handles today. Recommendation: ship
  Variant A, design the announce paths so Variant B remains reachable once a
  relay or forwarding policy exists.

### 3.3 RoomEvent mapping

| Today (`rooms.rs:245`) | On announce |
|---|---|
| `RemoteAnnounced` | `announced()` yields `Some` for a new `<room>/<peer>/<name>` path |
| `BroadcastSubscribed` | unchanged in meaning; fires when the consumer is wired to decoders (the `broadcast::Consumer` itself arrives with the announce) |
| `PeerJoined` | first announce under `<room>/<peer>/` for an unseen peer id |
| `PeerLeft` | last unannounce under `<room>/<peer>/` (all paths for that id gone), which coincides with session close or deliberate unpublish |
| `ChatReceived` | unchanged: chat stays a track inside the broadcast (`rooms.rs:447-458`) |

Display name and other peer metadata leave the gossip KV and move into catalog
extension sections of each peer's broadcast (the `CatalogExt` direction from
`plans/old/review-moq-usage.md` finding 1, chat and user sections), so metadata
is versioned with the media it describes and needs no side channel. The known
gotcha from the moq-net 0.1.11 migration (catalog serialization dropping
chat/user extensions) must be covered by tests here.

Note an asymmetry we accept: `PeerJoined` today can fire before any broadcast
exists, because presence is a KV row independent of media. Pure announce has no
broadcast-free presence. If we need "in the room but publishing nothing", each
peer publishes an empty presence broadcast `<room>/<peer>/presence` whose
catalog carries the metadata; that also gives `PeerLeft` a single well-defined
last-unannounce. This is a small convention, not new mechanism.

### 3.4 Roaming and reconnect, honestly

Transparent subscription migration (`model/resume.rs`) smooths a subscriber
across its own connection changes: when a subscriber's session drops and it
redials, `resume::Producer` splices the new session's tracks into the same
logical track at the next group boundary, with no dup or skip. That much is real
and carries over.

What earlier drafts got wrong is the publish side. With #2419 removing
`ROUTE_LINGER`, a peer whose session drops unannounces everything synchronously
the moment its last route detaches (`origin.rs:1123-1145`); there is no lingering
front for a reconnecting peer to splice back into. A peer that reconnects
re-announces from scratch, and every consumer observes an unannounce followed by
a fresh announce rather than an invisible re-attach. So roaming is no longer
free at the publish side: the room layer must decide whether a brief
unannounce/announce pair for a reconnecting peer is a real `PeerLeft`/`PeerJoined`
or a blip to smooth over.

The room layer therefore needs a debounce: hold `PeerLeft` (and decoder
teardown) for a short grace window after the last unannounce for a peer, and if
that peer re-announces within the window, treat it as a reconnect rather than a
join/leave pair. This debounce is room-layer policy, not something the transport
grants us any more, so it is a permanent part of the design rather than throwaway
code awaiting a transport feature.

---

## 4. Multi-peer topologies

The hop-chain machinery generalizes rooms beyond full mesh without new
protocol. Because every announce carries its `OriginList`, `exclude_hop`
prevents echo, and route selection converges all nodes on the cheapest live
route (#2424) with losers as backups, the following topologies are the same code
with different policy:

- **Full mesh (today's shape).** Every peer dials every peer; all hop chains
  have length 1; routing is trivial. The authorship scope in 3.2 is exact.
- **Peer-assisted relay (SFU-ish).** If A and C cannot connect directly, B
  wires both sessions to one shared origin (`with_origin`) and re-announces A's
  broadcasts to C with `hops = [B]`. C receives A's media through B; if a direct
  A-C route later appears, its lower cumulative cost or shorter chain wins route
  selection and B's copy becomes the backup. Because the cost restarts at zero at
  a node actively carrying the broadcast, B's re-announced cost reflects only the
  marginal transfer B would newly incur. The cost is that B forwards media
  (bandwidth) and that C must trust B's re-announces (section 5). This requires
  relaxing the per-session announce scope for designated relay peers.
- **moq-relay in the middle.** A relay is just another symmetric session at the
  same prefix; rooms hosted on a relay are the identical room code with the
  relay as one more member that happens to re-announce everyone. This reuses
  moq-relay's clustering mechanics: upstream documents that relay clustering
  discovers nodes and their broadcasts via `announced(prefix)`
  (`doc/concept/layer/moq-lite.md:65`), the same primitive our rooms sit on.
- **iroh-live-relay.** It already runs `moq_relay::Cluster` with moq-native
  transport and an iroh endpoint registered with the full ALPN list
  (`iroh-live-relay/src/lib.rs:17`, `:46-74`, `:100-104`), plus a pull-through
  path keyed on `LiveTicket` (`:158-180`, `pull.rs`). Once rooms are
  announce-based, iroh-live-relay becomes a room host with no iroh-live-specific
  room code: peers behind hard NATs dial the relay, publish under `<room>/<their-
  id>/`, and the relay's origin re-announces to everyone. It is also the natural
  answer to Variant B's rendezvous problem.

---

## 5. Security: token path-scoping closes the announce-spoofing gap

Earlier drafts framed this as an open design question and treated gossip's signed
KV state as a decisive advantage over announce, on the grounds that a moq announce
is only transport-authenticated (the iroh session proves the peer's `EndpointId`,
nothing signs the announced path) while a `PeerState` row is signed by the peer's
key and attributable no matter who relayed it. moq-token path-scoping (#2416)
turns that open question into a solved one for the direct case.

**The mechanism** (maps/moq-net-origin.md section 5). moq-token now has two
layers. A `Claims` token carries a `root` plus per-role `publish` / `subscribe`
prefix lists; `Key::verify` checks the signature and expiry and yields the
`Claims`, and `Claims::authorize(path)` rebases the granted prefixes onto the
connection path. A verifying session then scopes its origin cryptographically:
`origin.with_root(&token.root)?.scope(&token.publish)` for the publisher handle
and `.scope(&token.subscribe)` for the subscriber handle (`moq-relay/src/
cluster.rs:447-452`), wired via `with_publisher` / `with_subscriber`
(`connection.rs:117-120`). A `create_broadcast` outside the granted prefixes then
fails `Error::Unauthorized` (`origin.rs:877`), and announces outside them never
land. Above that sits `Scope` (`moq-token/src/claims.rs:16`), an immutable
ceiling embedded in the signing key's JWK: `validate_scope` runs on both `sign`
and `verify` (`key.rs:447`, `key.rs:493`), so a scoped key can only ever mint and
accept tokens whose grants sit at or beneath its ceiling, and a leaked scoped key
cannot be talked into signing beyond it.

**Applied to rooms.** Issue each peer a token rooted at `<room>/<endpoint-id>`
with `put: [""]` (and `get` as policy dictates). The verifier scopes that
session's origin to that prefix, so the peer cannot `create_broadcast` or announce
outside its own id: spoofing `<room>/<victim>/...` fails at the origin, not merely
by convention. A room server holds one signing key whose `Scope` only permits
minting such per-peer tokens, so even the token-minting path is bounded. This
gives cryptographic "announce only under your own id" enforcement, which is
exactly the authorship property gossip's signed KV provided, now delivered by the
moq layer over a single protocol.

**Remaining caveat, stated honestly.** This hard-wires "announce only your own
id", which is precisely correct for full mesh but conflicts with transitive relay
re-announce: a relay node re-announcing A's broadcast under `hops = [relay]` is,
by construction, announcing a path it does not own. Supporting relay-hosted rooms
therefore requires granting designated relay nodes a broader scope (for example
`put: [""]` rooted at `<room>` rather than `<room>/<relay-id>`), which is a real
design point, not a free consequence. And within any relaying topology the
end-to-end authorship guarantee is only as strong as the trust in the relay: hop
chains name random per-node origin integers, not keys, and nothing in the announce
protocol signs the chain, so a malicious or broadly-scoped relay can still
fabricate or suppress announces for others. The honest summary is that token
scoping weakens, rather than eliminates, the old argument that gossip's signed KV
was a decisive advantage: for the full-mesh call-scale case iroh-live actually
ships, moq now matches it; for relay topologies, an application-level signature
over the announce payload (a signed manifest track binding `EndpointId` to
broadcast names) remains the way to recover attributable authorship, and it is
the strongest reason Variant A might keep signed gossip presence in the interim.

---

## 6. Risks and open questions

- **Announce fan-out scale vs gossip.** Gossip spreads one KV write per state
  change over an epidemic overlay with per-topic cost roughly O(degree). MoQ
  announce is per-session: in a full mesh of N peers with B broadcasts each,
  every peer holds N-1 sessions and each announce traverses each of them, so
  announce traffic is O(N^2 * B) room-wide. At call scale (N <= 10 or so) this
  is noise, and it is the same asymptotic cost as the media mesh itself. It does
  mean announce-based rooms do not scale to large rooms without a relay, which
  gossip-based discovery in principle could. We accept this for call-scale full
  mesh, where every peer subscribes to every peer anyway; the media-mesh analogy
  is weaker for viewer-heavy rooms, because media sessions today are dialed on
  subscription interest while announce presupposes a session to every discovered
  peer (see the eager-dialing regression below).
- **Discovery becomes eager.** Announce presupposes a session, so every
  discovered peer is dialed and a room member holds N-1 sessions regardless of
  subscription interest, where today a peer is dialed only when one of its
  broadcasts is wanted. The design accepts this at call scale, where full-mesh
  media forces the same connection count anyway; viewer-heavy rooms would need
  subscribe-driven lazy dialing (dial on first subscribe, roster from gossip
  presence) added on top, which Variant A leaves room for but does not include.
- **Bootstrap without a relay.** Variant B's partition risk (section 3.2) is
  real: transitive announce-derived dialing only reaches peers reachable through
  currently connected sessions. Gossip's HyParView-style overlay repair has no
  moq equivalent. Until a relay is assumed, keep gossip.
- **Roster completion.** lite-06 `AnnounceOk { origin, active }` gives a
  defined "initial roster complete" point, so "room joined, N participants
  present" is a real protocol signal rather than a timer heuristic once
  iroh-live bumps to a release carrying it.
- **Relay-scope caveat for token scoping.** As in section 5, the per-peer token
  scope that gives full-mesh authorship enforcement must be widened for relay
  nodes, and relay re-announces remain trusted-relay-only without an
  application-level signature.
- **Migration sequencing.** Rooms can move to announce while media stays on the
  current pipeline. The subscribe path already ends in a `broadcast::Consumer`
  handed to `RemoteBroadcast`; announce-based discovery changes only how that
  consumer is obtained (from the scoped `announced()` stream instead of
  dial-plus-request), over the same deduped sessions. `moq-media` has no iroh or
  room dependency, so decoders, jitter, and catalog handling are untouched. The
  reverse is also true: phase 1 (transport delegation) does not require touching
  rooms at all.

---

## 7. Verdict and phases

Verdict: yes. The strongest arguments are these. Announcement and subscription
ride the same session and the same primitive: the scoped `announced()` stream
hands the room a `broadcast::Consumer` directly, so consume follows announce with
no cross-protocol handoff and no dial-then-request round trip per broadcast.
Unannounce liveness is immediate rather than expiry-based, and with #2419 it is
also synchronous with the last route detaching: session close and deliberate
unpublish both surface as a prompt `None` event instead of a 2-minute horizon.
Token path-scoping gives cryptographic announce-under-your-own-id enforcement
over that same session (section 5), so the direct-case authorship property that
made gossip's signed KV attractive now lives at the moq layer. The same code path
generalizes to relay-hosted rooms and moq-relay clustering without new wire
machinery (section 4). And smol-kv, with its signed-state machinery, drops out of
the room hot path entirely. What the migration does not do, under the recommended
Variant A, is reduce the protocol count: gossip stays for bootstrap and presence,
so the wire carries gossip plus moq rather than gossip plus KV plus moq. The win
is a thinner, purpose-built announcement path, not a single protocol.

The honest regressions that remain are the eager-dialing cost (section 6), the
relay-scope caveat and residual trusted-relay assumption in any relaying topology
(section 5), and the loss of the free seamless reconnect window: #2419 means a
reconnecting peer re-announces rather than splicing into a lingering front, so
the room layer owns a reconnect debounce (section 3.4).

Because the routing, subscription, session, and token features are all on moq
main now, the phase gating is no longer "main-compatible vs dev-dependent". It
collapses to "works against the release iroh-live currently pins" versus "works
against the next moq release iroh-live bumps to". Phase 1 needs nothing new.
Phases 2 and 3 land together once iroh-live bumps its `moq-net` / `moq-native`
pins past the merge, and the split between them is now just ordering of work, not
a wait on upstream.

### Phase 1: transport delegation (no bump required, no behavior change intended)

- Prerequisites: none; works against the released moq-net 0.1.11 / moq-native
  0.17.1 already in the workspace.
- Work: `iroh-moq` delegates handshake to `moq_native::iroh` +
  `moq_net::{Client, Server}`; full ALPN list on connect and accept (the
  review-moq-usage finding 2 recommendation); actor keeps dedup, fan-out,
  `ProtocolHandler`, and incoming stream; hoist to a single shared
  `origin::Producer` wired via `with_publisher`/`with_subscriber`. The
  `IncomingSession` gate is re-cast per section 3.1 since the single-phase iroh
  accept has no pre-handshake reject window.
- Size: `iroh-moq` 572 LOC loses the ~120-line handshake plus ALPN/version
  handling and part of the fan-out bookkeeping; expect roughly 350-400 LOC
  after, diff on the order of 300-400 lines plus e2e re-runs (wire-visible ALPN
  change).

### Phase 2: announce-based rooms, gossip retained for bootstrap (Variant A)

- Prerequisites: phase 1 (shared origin per node, sessions wired to it), and a
  bump to a moq release cut from main so `create_broadcast` + `Route`, the
  scoped `announced()` bus, moq-token scoping, and #2419 liveness are available.
- Work: room prefix `<room>/<endpoint-id>/<name>` published via
  `create_broadcast(path, Route::announced())`; announce-under-own-id authorship
  via the scoped session origin (mechanism per the open design question below,
  the per-session scope of 3.2 being the single-room special case), backed by a
  per-peer moq-token scoped to `<room>/<endpoint-id>` (section 5); `RoomEvent`
  derived from the scoped `announced()` stream per the table in 3.3; presence
  broadcast convention; metadata moves to catalog extensions; gossip shrinks to
  presence beacons and bootstrap; smol-kv dependency removed; the reconnect
  debounce of section 3.4 to absorb #2419's synchronous unannounce on transient
  session loss. The public `Room`/`RoomHandle` API is unchanged.
- Size: `rooms.rs` 695 LOC loses the KV client setup, `PeerState`
  serialization, `update_kv`, and the KV-update handler (roughly 200-250
  lines), gains scoping, token wiring, and event derivation (roughly 120-180
  lines); expect roughly 450-550 LOC after, plus catalog-extension work in
  `moq-media`. The `Actor`'s auto-subscribe `FuturesUnordered` machinery
  simplifies because the consumer arrives with the announce.
- Open design question, multi-room scoping (must be settled before
  implementation): the consume scope is fixed when a session is wired, sessions
  are deduped one-per-`EndpointId` across the node (3.1), and room membership is
  dynamic. When two peers share more than one room, the scope must be the union
  of the `<room>/<peer>/` prefixes and must change when either side joins or
  leaves a room mid-session; moq-net does not support rescoping a live session's
  wired producer. Options: per-room sessions (simple, but loses dedup and
  multiplies connections), dynamic scope updates (clean, but needs an upstream
  change), or wiring a broad scope and filtering announces in the iroh-moq actor
  (keeps dedup, needs no upstream change; the actor knows which session each
  announce arrived on, so it can enforce path-author-equals-session-peer across
  all rooms, but the actor then becomes the security boundary, and any name that
  slips an incomplete filter is spoofable and safe for display only). Where a
  per-peer token scope covers a single room cleanly, the multi-room case is the
  case that still needs actor-level enforcement or per-room sessions. This
  document recommends accept-and-filter for phase 2, with the enforcement rule
  and its analysis written down before any code. Related observability gap: a
  scope-rejected `create_broadcast` fails `Error::Unauthorized` and a filtered
  announce is dropped silently, which turns authorization bugs and version skew
  into invisible missing-peer symptoms; wherever enforcement lands, rejections
  must be logged at warn level with the session peer and the offending path.

### Phase 3: full announce-native capabilities (same bump as phase 2, later work)

- Prerequisites: the same moq bump as phase 2; a security analysis per section 5
  before any relaying is enabled.
- Work: wire `Subscription::latency_max`/`priority` from the phase-3b playout
  work to the publisher-side aggregate clamp; `AnnounceOk { active }`
  roster-complete events; relay-hosted rooms via iroh-live-relay as a room
  member, reusing moq-relay clustering with relay-scoped tokens (section 5);
  optionally replace `spawn_stats_recorder` plumbing with moq-stats. The
  reconnect debounce from phase 2 stays, since #2419 removed the linger that
  once made it disposable.
- Size: the room-layer delta itself is small, mostly wiring; the relay-hosted
  path carries the token-scope and forwarding-policy design work of sections 4
  and 5.

What survives where: `iroh-moq`'s actor (dedup, fan-out via origin, Router
handler, incoming stream) survives all phases as the crate's reason to exist; its
transport half dies in phase 1. `rooms.rs` survives as the event derivation and
API layer; its KV half dies in phase 2. `RoomTicket` survives with gossip
bootstrap in phase 2 and can drop the `TopicId` only if Variant B or a relay
rendezvous replaces gossip entirely.
