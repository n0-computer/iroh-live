# moq-net Origin / announce / session / subscription map

SOURCE: moq main, HEAD `3a3e0ea8` (2026-07-21); dev merged into main.

Scope: the networking layer of the upstream moq repo at `/home/bit/Code/rust/moq`,
read for the iroh-live room-layer redesign. On 2026-07-21 `dev` merged into `main`,
so the old MAIN-vs-DEV split in this document is obsolete: every feature previously
in the "DEV" column (transparent subscription migration, `(Session, Driver)`,
`Option<Role>`, module-scoped names, `Subscription`, moq-stats, lite-06 announce
ids) is now plain `main`. This rewrite collapses the two columns and folds in the
net-layer commits that landed *after* the old dev pin `261c2048`:

- **#2396** `7671feb8` — route everything through `create_broadcast`; gate announce on `Route.announce`.
- **#2424** `f5fa0263` — route by cumulative cost on lite-06 announcements.
- **#2419** `1bf9d9c9` — unannounce as soon as the last route detaches (linger removed).
- **#2414** `99ff58e6` — accept an empty PATH, default to `""` across protocols.
- **#2416** `6b86e612` — moq-token: scope signing keys to publish/subscribe paths.
- **#2423** `5e72d2da` — pre-bump API polish (signature churn).

Companion document: [room-layer.md](room-layer.md) maps the iroh-live side
(`iroh-moq` + `iroh-live` rooms). Section 6 here does the cross-mapping. Naming is
module-scoped throughout: `origin::{Producer,Consumer}`, `broadcast::{Producer,
Consumer,Route}`, `track::{Consumer,Subscriber}`, `stats::Handle`.

---

## 1. Origin / Path / announce model

**`Origin` is a relay identity, not a registry** — a non-zero 62-bit id used in
hop chains (`rs/moq-net/src/model/origin.rs:26-96`):

```rust
pub struct Origin { id: u64 }                    // private field, non-zero < 2^62
    pub fn new(id: u64) -> Result<Self, InvalidOrigin>   // rejects 0 and >= 2^62
    pub fn random() -> Self
    pub fn id(self) -> u64
    pub fn produce(self) -> Producer             // via Info::produce
#[non_exhaustive]
pub struct Info { pub id: Origin, pub pool: cache::Pool }   // origin owns a memory budget
    pub fn new(id) -> Self;  pub fn with_pool(self, pool) -> Self;  pub fn produce(self) -> Producer
pub struct OriginList(Vec<Origin>);              // hop chain, MAX_HOPS = 32
```

The registry/announce bus is the **`origin::Producer` / `origin::Consumer`** pair,
an in-process path-tree of broadcasts plus a coalesced announce stream. As of
**#2396 the sole ingress is `create_broadcast`** (`origin.rs:833-900`):

```rust
impl origin::Producer {
    // #2396: the ONLY way content enters an origin. `route` is initial metadata;
    // Route.announce gates whether the path is advertised. Spawns a lifecycle task.
    pub fn create_broadcast(&self, path: impl AsPath, route: broadcast::Route)
        -> Result<broadcast::Producer, Error>       // Unauthorized if outside scope
    pub fn scope(&self, prefixes: &[Path]) -> Option<Producer>     // origin.rs:907
    pub fn with_root(&self, prefix: impl AsPath) -> Option<Self>   // auto-strip prefix, origin.rs:960
    pub fn dynamic(&self) -> Dynamic                // serve broadcasts on demand (NOT announced)
    pub fn consume(&self) -> Consumer
    pub fn announces(&self) -> AnnounceProducer     // symmetric announce-bus handle
    pub fn allowed(&self) -> impl Iterator<Item=&Path>   // permitted publish/subscribe prefixes
    pub fn absolute(&self, path) -> Path
}
```

`publish_broadcast` is **gone**: attaching an externally-produced broadcast now
goes through `create_broadcast` + `Route`, and the old boolean return became the
route-selection machinery. The origin owns the broadcast consumers actually see
and splices tracks across every source created at the same path (local publishers
and sessions attaching announces from the network), serving from the best route.

`origin::Consumer` is a cheap read handle; the announce cursor is split out
(`origin.rs:1877-2098`):

```rust
impl origin::Consumer {
    pub fn announced(&self) -> AnnounceConsumer                   // origin.rs:1932
    pub fn request_broadcast(&self, path) -> kio::Pending<Requesting>   // pull; falls back to dynamic
    pub fn scope(&self, prefixes) -> Option<Consumer>
    pub fn with_root(&self, prefix) -> Option<Self>
}
pub struct OriginAnnounce { pub path: PathOwned, pub broadcast: Option<broadcast::Consumer> }  // origin.rs:737
// AnnounceConsumer::next().await -> Option<OriginAnnounce>; None(inner) = no longer available
```

**`announced(prefix)` discovery** is expressed through scoping: a consumer built
with `with_root(prefix)` / `scope(&[prefix])` only sees (and strips) announces
under that prefix. Replacement of a path is delivered as unannounce-then-announce
(`PendingUpdate::UnannounceAnnounce`, `origin.rs:405-408`), never a swap in place;
a route change alone is invisible here (observe it via `broadcast::Consumer::
route_changed`). `OriginConsumerState` keeps pending updates in a `BTreeMap` keyed
by path so a slow consumer's backlog is bounded by distinct paths.

**Path** (`rs/moq-net/src/path.rs`, 1431 lines) is a relative, slash-delimited,
`Arc<str>`-backed zero-copy type: boundary-safe `has_prefix`/`strip_prefix`
(`"foo"` does not prefix `"foobar"`), `join`, `parts`, `MAX_PARTS = 32`, plus
`PathRelative` + `Path::resolve` for `..`-aware sibling resolution (renditions
pointing at `../source`). `PathPrefixes` dedups overlapping scoping prefixes.
**#2414**: an empty PATH is now legal and defaults to `""` across protocols; the
client/server no longer reject or synthesize a path (`lite/setup.rs`, `client.rs`,
`server.rs`).

**Broadcast** (`model/broadcast.rs`): a named collection of tracks with a dynamic
`Route`. `broadcast::Producer::{create_track, remove_track, reserve_track,
unique_track, dynamic, set_route, finish, consume}`; `broadcast::Consumer::{track,
route, route_changed}`. No catalog concept at this layer (that lives in hang).

**Announce wire, lite-05/06** (`rs/moq-net/src/lite/announce.rs`, 638 lines):

```rust
pub enum AnnounceBroadcast<'a> {
    Active  { suffix: Path<'a>, hops: OriginList, cost: RouteCost },  // ANNOUNCE_START; assigns next id
    Ended   { suffix: Path<'a>, hops: OriginList },                   // pre-06 retract-by-path
    EndedId { id: u64 },                                              // ANNOUNCE_END: retract by id
    Restart { id: u64, hops: OriginList, cost: RouteCost },           // ANNOUNCE_RESTART (recv only)
}
pub struct RouteCost(pub u64);  // lite-06 only; older peers report 0. `charged(link)` saturating-adds
pub struct AnnounceRequest<'a> { prefix, exclude_hop }
pub struct AnnounceOk { pub origin: Origin, pub active: u64 }  // first reply; initial-roster count
```

`Restart` is accepted on decode but never sent (a replacement goes out as `EndedId`
+ `Active`). `AnnounceOk.active` lets a joiner block until the initial roster lands.

---

## 2. Routing (create_broadcast + cumulative cost + Route.announce gating + unannounce liveness)

The route model (the heart of the merged net layer) lives in `broadcast::Route`
(`model/broadcast.rs:52-145`) and the `FrontState` machinery in
`origin.rs:997-1370`.

```rust
#[non_exhaustive]
pub struct Route {
    pub hops: OriginList,        // chain traversed, oldest first; loop detection + tie-break
    pub cost: u64,               // #2424: marginal pull cost, accumulated per link; lower wins
    advertised: u64,             // (crate-private) cost as advertised before this link's charge
    pub announce: bool,          // #2396: gate — advertised via Consumer::announced only if true
}
    pub fn new() -> Self         // unannounced direct route (reachable by exact path only)
    pub fn announced() -> Self   // announced direct route
    pub fn with_hop / with_hops / with_cost / with_announce(...)
```

**Route selection** (`route_order`, `origin.rs:392-395`): the ordering key is
`(!announce, cost, hop_len, fnv_hash)`, lower wins — **announced routes first**
(a live source beats an offline one), **then lowest cumulative cost** (#2424),
then shortest hop chain, then a deterministic FNV tie-break so every node converges
on the same route. Only the best source gets served; the rest park as hot standbys
and are promoted (with re-splice) when the active source dies. Consumers never
observe the swap; tracks resume at the first missing group.

**#2396 — publish + announce gating.** `create_broadcast(path, route)` is now the
single entry point. The `Route.announce` flag (internally called `live`, hence the
commit title "gate announce on Route.live") controls advertisement independently
of the broadcast's existence: a non-announced broadcast stays reachable by exact
path for subscribes/fetches (serving cached or on-demand content) but is invisible
to `announced()`. Toggling it via `broadcast::Producer::set_route` announces or
unannounces without touching the broadcast. `sync_front` (`origin.rs:1103-1114`)
re-derives the advertised route and the announce flag after every table change.

**#2424 — cumulative-cost multi-hop routing.** `RouteCost` rides lite-06
announcements. The original publisher seeds `cost` with its production cost (zero
for a live publish, large for a cold standby transcoder); each link adds its
configured price as the announcement crosses it, so a metered-backbone route ranks
worse than an equal-length datacenter one. The accumulation **restarts at zero at
any node actively carrying the broadcast** (those upstream legs already exist and
are not re-paid), so `cost` reflects the transfers *one more* subscription would
newly cause. A zero advertised cost on a ≥2-hop chain signals the announcing relay
is actively carrying it, which the `handover_allowed` gate (`origin.rs:1078-1084`)
keys on. Pre-lite-06 peers report zero, so hop-count remains the effective metric
exactly as before. The client seeds its own production cost via
`Client::with_cost(cost)` (`client.rs:90`).

**#2419 — unannounce liveness (linger removed).** Detaching the last source now
**closes the broadcast and unannounces synchronously** (`detach_source`,
`origin.rs:1123-1145`: when `routes.is_empty()`, set `closed=true` and abort). The
old front-level `ROUTE_LINGER = 5s` grace that kept a path announced so a
reconnecting session could transparently re-attach is **gone**; an unannounce now
fires the moment the last route detaches. (An upstream-subscription linger still
exists at the relay's subscriber layer, `broadcast.rs:1243`, unrelated to the
front's announce liveness.) Net effect for rooms: departure signalling is now
immediate and unambiguous, at the cost of the 5s seamless-reconnect window — a
reconnecting peer re-announces rather than splicing into a lingering front.

**Transparent subscription migration** (`model/resume.rs`, 1334 lines) still
underlies this: `resume::Producer` splices per-session tracks into one logical
track, switching at group boundaries so a subscription survives route/connection
changes. `broadcast::Producer` keys tracks by `resume::Producer`; `track::
Subscriber` is `Plain` or `Spliced`. Resumption is exact at the next group
boundary (no dup/skip); an in-flight group whose route died is reachable only via
`fetch_group`. With #2419 this only stays seamless if the replacement route
attaches before the last old route detaches.

---

## 3. Session model

**`(Session, Driver)` pair, caller-driven** (`session.rs:66-208`, `client.rs:98`):

```rust
pub async fn Client::connect<S: web_transport_trait::Session>(&self, session: S)
    -> Result<(Session, Driver), Error>
pub struct Session { .. }
    pub fn version(&self) -> Version
    pub fn send_bandwidth(&self) / recv_bandwidth(&self) -> Option<bandwidth::Consumer>
    pub fn stats(&self) -> ConnectionStats   // rtt, rates, bytes/packets sent/recv/lost
    pub fn abort(&self, err: Error)
pub struct Driver { .. }                      // impls Future; also Driver::poll(&mut, &kio::Waiter)
```

Nothing is spawned behind your back: spawn the driver, await it, or step
`Driver::poll`. moq-net has **no direct tokio dependency** (`kio` plumbing), so any
executor drives it (wasm-friendly). The driver holds no session handle, so the
transport closes when the last `Session` clone drops.

**Builder wiring** (`client.rs:29-95`, same surface on `Server`):

```rust
pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self
pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self
pub fn with_origin(self, origin: origin::Producer) -> Self   // publisher(&origin) + subscriber(origin)
pub fn with_stats / with_versions / with_path / with_cost(...) -> Self
```

The idea: **publisher = "the peer may consume what this origin announces";
subscriber = "announces from the peer land in this origin::Producer"**. Handing the
same producer to both makes a symmetric relay-style session. The advertised SETUP
`Role` is derived from which origins were wired (`Role::from_origins`,
`lite/setup.rs:106`): `(publishes, consumes)` → `Publisher` / `Subscriber` /
`None` (bidirectional). `Role::Both` is unrepresentable; the wire carries
`Option<Role>` and old peers decode `None`. Stats live in a real `stats` module
(`stats::Registry` + `stats::Handle`); publishing counters as MoQ tracks is the
separate `moq-stats` crate.

---

## 4. Subscription model

`Subscription` (`model/subscription.rs`, 192 lines):

```rust
#[non_exhaustive]
pub struct Subscription {
    pub priority: u8,
    pub ordered: bool,               // default false
    pub latency_max: Duration,       // "Subscriber Max Latency" on the wire (#2176)
    pub group_start: Option<u64>,    // None = start at latest (a wire request, not the local cursor)
    pub group_end: Option<u64>,      // None = unbounded
}
// builders: with_priority / with_ordered / with_latency_max / with_group_start / with_group_end
```

Each subscriber holds its own `Subscription`; the publisher observes an
**aggregate** across live subscribers (`priority` = max, `ordered` = AND,
`latency_max` = max, `group_start` = earliest explicit, `group_end` = unbounded if
anyone is). #2349 clamps latency **once on the aggregate** against the publisher's
retention window (min distributes over the max). Subscribe chain: `track::Consumer::
subscribe(subscription) -> kio::Pending<Subscribing>` → `track::Subscriber` with
`start_at`/`end_at` (local read cursor, distinct from the wire preference) plus
`update(Subscription)`. `get_group` is gone; `fetch_group` replaces it and
concurrent fetches of the same group coalesce behind a shared `Requests` queue
(#2328): N requesters cost one upstream round-trip. Relays keep at most one live
upstream SUBSCRIBE per assigned track and drive it off the downstream aggregate,
pausing (`SubscribeUpdate { priority: 0, end_group: latest }`) when the last
subscriber leaves.

---

## 5. Token path-scoping (moq-token, #2416)

moq-token has **two layers** now, both enforcing segment-aware, role-independent
path containment:

**Claims** (`rs/moq-token/src/claims.rs`) — the token payload:

```rust
#[non_exhaustive]
pub struct Claims { pub root: String, pub publish: Vec<String>, pub subscribe: Vec<String>,
                    pub expires, pub issued }   // put/get in JSON; relative to root
    pub fn authorize(&self, path: &str) -> Result<Permissions>   // claims.rs:196
pub struct Permissions { pub publish: Vec<String>, pub subscribe: Vec<String> }  // rebased to path
```

`authorize(path)` overlaps the connection path with the token root (either
direction), rebases each granted prefix relative to `path`, drops prefixes outside
it, and errors `RootMismatch` / `NoAccess`. Publish and subscribe are checked
independently; an empty prefix grants everything beneath. This is **authorization
only** — verify the signature (and expiry) with `Key::verify` first.

**Scope** (`claims.rs:5-70`, NEW in #2416) — an immutable ceiling embedded in the
signing key's JWK (`Key.scope: Option<Scope>`, `key.rs:165`):

```rust
pub struct Scope { pub root: String, pub publish: Vec<String>, pub subscribe: Vec<String> }
    pub fn allows(&self, claims: &Claims) -> bool   // every granted path ⊆ scope, per role
impl Key {
    pub fn with_scope(self, scope) -> Result<Self>          // key.rs:487
    fn validate_scope(&self, claims) -> Result<()>          // ScopeExceeded if !allows
    // called by BOTH sign() (key.rs:459) AND verify() (key.rs:447)
}
```

**What it enforces:** a scoped signing key can only mint (and only accepts on
verify) tokens whose every publish/subscribe grant sits at or beneath a scope
prefix in the same role — segment-aware (`live` ∌ `lively`), roles independent
(a publish-only scope rejects any subscribe grant). The scope is fixed at key
generation: widening means minting a new key, so a leaked scoped key can never be
talked into signing beyond its ceiling. Keys with no scope stay unrestricted
(backward compatible).

**How a session/relay validates a path-scoped token** (`moq-relay/src/{auth,
cluster,connection}.rs`): `Key::verify(token)` checks signature + expiry +
`validate_scope`, yielding `Claims`; `Claims::authorize(check_root)` yields the
rebased `Permissions`; then the relay **scopes the origin cryptographically** —
`self.origin.with_root(&token.root)?.scope(&token.publish)` for the publisher
handle and `.scope(&token.subscribe)` for the subscriber handle
(`cluster.rs:446-453`), wired via `with_publisher` / `with_subscriber`
(`connection.rs:117-120`). The session's `origin::Producer`/`Consumer` are thereby
constrained to exactly the granted prefixes: a `create_broadcast` outside them
fails `Error::Unauthorized` (`origin.rs:877`), and announces outside them never
land.

**Room announce-spoofing question — yes, addressable.** A peer *can* be
cryptographically constrained to publish/announce only under its own path: issue
each peer a token rooted at `<room>/<endpoint-id>` with `put: [""]` (and `get` as
policy dictates). The verifying side scopes that session's origin to that prefix,
so the peer cannot `create_broadcast` — and therefore cannot announce — outside
its own id. The key-level `Scope` lets a room server hold one signing key that can
*only* mint such per-peer tokens, so even the token-minting path is bounded. The
one caveat unchanged from before: this hard-wires "announce only your own
broadcasts", which conflicts with transitive/relay re-announce unless
hop-carrying re-announces are authorized under a broader scope for designated
relay nodes.

---

## 6. moq-native iroh transport (`rs/moq-native/src/iroh.rs`, 317 lines)

Stack: `web-transport-iroh` → `iroh` (transitive); `iroh://` URL scheme where the
host is the peer's public key (`iroh::EndpointId`), not a hostname;
caller-supplies-the-`iroh::Endpoint` model.

```rust
#[non_exhaustive]
pub struct EndpointConfig {                 // clap Args + serde, MOQ_IROH_* env
    pub enabled: Option<bool>, pub secret: Option<String>,   // hex key OR key-file path
    pub bind_v4: Option<SocketAddrV4>, pub bind_v6: Option<SocketAddrV6>,
    pub disable_relay: Option<bool>,        // presets::N0 vs N0DisableRelay
}
    pub async fn bind(self, quic: &crate::quic::Client) -> Result<Option<Endpoint>>
// one shared P2P endpoint for both roles; ALPNs = moq_net::ALPNS + web_transport_iroh h3

pub(crate) async fn accept(conn: iroh::endpoint::Incoming)
    -> Result<(web_transport_iroh::Session, Option<Url>, Option<PeerIdentity>)>   // iroh.rs:219
pub(crate) async fn connect(endpoint: &Endpoint, url: Url,
    addrs: impl IntoIterator<Item=SocketAddr>) -> Result<web_transport_iroh::Session>  // iroh.rs:253
```

`accept` is **single-phase**: it negotiates ALPN, completes the WebTransport
handshake (sends the H3 OK), and returns `(session, url, None)` — the deferred
two-phase `Request::accept` → `ok()`/`close()` authorization window that old MAIN
had no longer exists; the server reads SETUP immediately and authorizes from token
+ path there. `peer_identity()` is always `None` for iroh (no client-cert
identity). Raw QUIC carries no URL, so the path rides the lite-05 SETUP (now
optionally empty, #2414). Client side: `with_iroh(iroh::Endpoint)`,
`with_iroh_addrs(Vec<SocketAddr>)` (direct IPs, bypass discovery), dial branch in
`connect_inner` offers all moq ALPNs + h3. `reconnect.rs` is transport-agnostic
`Reconnect` with exponential backoff, dialing fresh each retry (no session state
carried; the #2241 migration engages at the origin/route layer, not here) and
exposes live `stats()`.

---

## 7. Room mapping analysis

### 7.1 What a room maps onto in moq-net terms

The natural mapping for "N peers publishing and subscribing to each other at a
shared prefix": each node owns **one `Origin` (one `origin::Producer`) shared
across all its sessions**, publishes its own broadcasts at
`<room>/<endpoint-id>/<name>` via `create_broadcast(path, Route::announced())`,
and wires every peer session symmetrically (`with_origin`). Discovery is the
announce bus: watch `announced()` on a consumer scoped to `<room>/` and dial into
any broadcast that appears. Loop protection is free from hop chains + `exclude_hop`;
duplicate routes (A heard directly and re-announced via B) converge deterministically
on lowest cost then shortest chain, the loser kept as a hot standby.

`iroh-moq` already implements the first half: `Moq::new` mints one
`Origin::random()` per node, shared across every session (`iroh-moq/src/lib.rs`,
room-layer.md §1), and fans every published broadcast out to every session. What
iroh-live does *not* use is the consume/announce direction across the room: rooms
discover broadcasts via **iroh-gossip + iroh-smol-kv** (signed `PeerState` under a
gossip `TopicId`) and then dial point-to-point + `request_broadcast(name)` per
broadcast. MoQ announce is used only degenerately on the single direct session.

### 7.2 announce-based room vs the current gossip+KV room

What moq announce gives that gossip+KV does not:

- **No second protocol.** Discovery and media share the moq session — no gossip
  swarm, no smol-kv CRDT, one ALPN. The room layer shrinks to "scope a prefix,
  watch announces, publish under your id".
- **Deterministic liveness, now immediate (#2419).** An unannounce arrives the
  moment the last route to a broadcast detaches — no 5s linger, no KV expiry
  horizon. The current design infers departure from broadcast-close plus a
  2-minute KV expiry (room-layer.md §3.4): slower and lossier. Trade-off: the
  removed linger means a reconnecting peer re-announces rather than splicing
  seamlessly, so brief network blips now surface as an unannounce/announce pair.
- **Loop-protected, cost-aware transitive forwarding (#2424).** Hop chains +
  `exclude_hop` + cumulative `RouteCost` let peers safely re-announce and relay
  each other, and route selection prefers the cheapest live path: if A↔C cannot
  connect directly, C receives A through B, and a later direct route wins on cost.
  Gossip spreads only names; media is strictly point-to-point today.
- **Relay compatibility.** The same origin/announce/cost model is what moq-relay
  speaks, so a room can include an infrastructure relay as just another session.
- **Initial-set sync.** lite-06 `AnnounceOk { origin, active }` gives "room
  joined, N participants present" a defined completion point.

What gossip+KV gives that announce does not:

- **Membership without pairwise sessions.** Gossip builds an overlay from a
  ticket's bootstrap peers and spreads epidemically; announce presupposes moq
  sessions already exist and has no peer-sampling. Someone must still decide whom
  to dial. (Announce paths carry `EndpointId`s, so the roster can propagate
  transitively — but only if peers re-announce across sessions, which implies a
  relay/forwarding policy.)
- **Offline-tolerant, signed, attributable state.** smol-kv entries are signed and
  merged CRDT-style. moq announces are paths trusted per-session. **This is now
  addressable at the moq layer: moq-token path-scoping (§5) constrains each
  accepted session's origin to `<room>/<peer-id>/`, cryptographically enforcing
  "you may only announce your own broadcasts"** — exactly what `with_root` + `scope`
  are for, and what the relay already does with verified tokens. It is room-layer
  code to wire, and it conflicts with transitive forwarding unless relay nodes get
  a broader scope. This closes the announce-spoofing gap that previously made
  gossip+KV's signed authorship a hard advantage.
- **Arbitrary metadata.** display names, chat presence ride the KV for free;
  announce carries only path existence (the moq answer is a metadata track inside
  each broadcast, hang-catalog style, which iroh-live half-does via its chat track).

For a relay-less p2p mesh at call scale (small N, full mesh — what iroh-live
builds), the honest summary: gossip currently does membership *and* announcement;
moq announce can take over announcement entirely, with immediate liveness (#2419),
cost-aware multi-hop (#2424), and — new — token-enforced authorship (#2416),
while membership bootstrap (who to dial first) still needs the ticket plus gossip,
transitive announce-derived dialing, or another side channel. A pragmatic redesign
keeps the ticket bootstrap, replaces `PeerState.broadcasts` + per-broadcast
dialing with a scoped shared `Origin` and `announced()` over the existing per-peer
sessions, issues each peer a token scoped to its own id, and keeps gossip (if at
all) only for membership hints and non-media state.

### 7.3 Version caveat for planning

iroh-live pins crates.io `moq-net 0.1.11` / `moq-native 0.17.1`, i.e. the old MAIN
line. Everything catalogued here arrives only with the next breaking bump: the
merged tree is visibly pre-bump (module-scoped renames, `#[non_exhaustive]`
everywhere, wire versions Lite05/Lite06Wip, `(Session, Driver)`, `Subscription`,
moq-token `Scope`). Nothing here is on the currently-pinned release yet.
