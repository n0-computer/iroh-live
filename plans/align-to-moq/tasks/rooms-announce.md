# rooms-announce

Branch: align/rooms-announce          Wave: 2
Depends on: Wave 0 pin bump (moq release cut from main carrying `create_broadcast`
+ `Route`, the scoped `announced()` bus, moq-token `Scope`, and #2419 unannounce
liveness); the transport-delegate task for phase 1 (shared per-node
`origin::Producer`, sessions wired to it). Independent of the codec work, so it
runs in parallel with Wave 1.
Kind: independent

## Goal
This task rebuilds iroh-live's room discovery on moq's announce bus. Today a room
is an iroh-gossip `TopicId` plus an `iroh_smol_kv` signed `PeerState` KV that
carries the broadcast list, display name, and membership, and every discovered
broadcast triggers a point-to-point dial plus a per-broadcast request. The
redesign moves announcement onto moq: one shared per-node `origin::Producer`
publishes each broadcast at `<room>/<endpoint-id>/<name>` via
`create_broadcast(path, Route::announced())`, and each member derives `RoomEvent`s
from `announced()` on a consumer scoped to the room prefix, so the
`broadcast::Consumer` arrives with the announce and there is no separate
dial-then-request round trip. Gossip is retained for bootstrap and presence
(Variant A of `room-layer.md` section 3.2), and moq-token path-scoping enforces
announce-under-your-own-id cryptographically. The net effect is that
`iroh_smol_kv` leaves the room hot path entirely, `rooms.rs` shrinks from 695 LOC
to roughly 450 to 550 LOC, departure signalling becomes prompt and deterministic,
and the public `Room` / `RoomHandle` API is unchanged.

## Evidence
- `room-layer.md` is the anchor: the current design in section 1, what moq
  provides in section 2, the redesign in section 3, the `RoomEvent` mapping in
  section 3.3, roaming and reconnect in section 3.4, multi-peer topologies in
  section 4, the token-scoping security argument in section 5, the risks in
  section 6, and the phases in section 7 (this task follows those phases).
- `../upstream/comparisons/maps/moq-net-origin.md` sections 1, 2, 5, and 7 for the
  Origin, announce, `Route`, unannounce-liveness, and token model, with per-commit
  moq citations.
- `../upstream/comparisons/pubsub.md` section 4 for where display name, chat, and
  user metadata belong (catalog `CatalogExt` extension versus room layer), and the
  moq-net 0.1.11 catalog-serialization gotcha that dropped chat and user
  extensions, which the metadata-move step must cover with tests.
- `cut-plan.md` for the deletion ledger verdict on `iroh_smol_kv` and the
  `PeerState` machinery.

## moq primitive adopted
The moq announce bus, driven from the shared per-node origin.
- `origin::Producer::create_broadcast(path, route)` is the sole content ingress
  (`rs/moq-net/src/model/origin.rs:869`), gated on `Route.announce`.
- `broadcast::Route::announced()` advertises a route
  (`rs/moq-net/src/model/broadcast.rs:112`); `Route::new()`
  (`broadcast.rs:103`) stays reachable by exact path but invisible to
  `announced()`, for pull-only sources.
- `origin::Consumer::announced()` yields the coalesced announce stream
  (`origin.rs:1932`); each `OriginAnnounce { path, broadcast: Option<..> }` is an
  appear (`Some`) or a disappear (`None`, an unannounce, never an in-place swap).
- `origin::{Producer,Consumer}::scope(&[prefix])` and `with_root(prefix)` restrict
  and strip a prefix (`origin.rs:907`, `origin.rs:960`, `origin.rs:2002`,
  `origin.rs:2074`), the primitive for the room prefix and per-peer authorship
  scope.
- moq-token `Scope` plus `Claims::authorize` for the cryptographic
  announce-under-your-own-id enforcement (`rs/moq-token/src/claims.rs`,
  `key.rs:447`, `key.rs:493`; the relay wiring pattern is
  `moq-relay/src/cluster.rs:446-453` and `connection.rs:117-120`).
- #2419 removed the front-level `ROUTE_LINGER`, so detaching the last route
  unannounces synchronously (`origin.rs:1123-1145`): a reconnecting peer
  re-announces rather than splicing into a lingering front, and the room layer
  owns the reconnect debounce.

## iroh-live code changed
`iroh-live/src/rooms.rs` (695 LOC) is the primary edit target.
- The `iroh_smol_kv` and gossip-KV import and setup: `use iroh_smol_kv::{..}`
  (rooms.rs:12), the `iroh_smol_kv::Client::local(..)` construction
  (rooms.rs:336-346), and the `kv` field (rooms.rs:321) all leave the room hot
  path.
- `PeerState` (rooms.rs:284) with its postcard serialization, the
  `PEER_STATE_KEY = b"s"` write key (rooms.rs:281), `update_kv` (called at
  rooms.rs:445 and rooms.rs:484), and the KV-update handler
  `handle_gossip_update` (dispatched at rooms.rs:388) are deleted; roughly 200 to
  250 lines per the phase-2 sizing in `room-layer.md` section 7.
- The point-to-point discovery path `live.subscribe(remote, &name)` (rooms.rs:565)
  is replaced by consuming the `broadcast::Consumer` that arrives with the
  scoped `announced()` event, so the auto-subscribe `FuturesUnordered` machinery
  in `Actor` simplifies.
- `RoomEvent` (rooms.rs:245: `RemoteAnnounced`, `BroadcastSubscribed`,
  `PeerJoined`, `PeerLeft`, `ChatReceived`) keeps its variants and meaning but is
  re-derived from `announced()` per the table in `room-layer.md` section 3.3;
  the emit sites are rooms.rs:413, :437, :449, :544, :575.
- `known_peers: HashMap<EndpointId, Option<String>>` (rooms.rs:313) membership
  tracking is retained but fed from announce paths plus presence rather than KV
  rows, and gains the reconnect-debounce grace window.
- `RoomTicket` (`iroh-live/src/rooms/ticket.rs`, re-exported at rooms.rs:24) keeps
  its `TopicId` plus bootstrap list under Variant A; gossip shrinks to presence
  beacons and bootstrap.
- Metadata (display name, user section) moves from the KV into catalog extension
  sections, touching `moq-media/src/catalog.rs` `IrohLiveExt` and its tests.
The public API on `RoomHandle` (rooms.rs:56-189, including `ticket()`,
`set_display_name`, `recv`, `try_recv`, `split`) is preserved.
`iroh-live/src/rooms/publisher.rs` (72 LOC) is reviewed for the announce-path
publish call but is small.

## Steps
Phases match `room-layer.md` section 7. Phase 1 (transport delegation) is the
separate transport-delegate task and is a prerequisite here; the phases below are
this task's own sub-steps, main-compatible ordering first, and each is small
enough to commit on its own.

Sub-step 0 (settle the coordination point before any phase-2 code). Resolve the
multi-room scope design question (coordination point 5, `room-layer.md` section 7
phase 2 open question). The consume scope is fixed when a session is wired,
sessions are deduped one-per-`EndpointId` across the node, and room membership is
dynamic, so when two peers share more than one room the scope must be the union of
their `<room>/<peer>/` prefixes and would have to change mid-session, which
moq-net does not support for a live wired producer. The document recommends
accept-and-filter in the iroh-moq actor for phase 2: wire a broad scope and filter
announces by asserting path-author equals session peer across all rooms, which
keeps dedup and needs no upstream change but makes the actor the security
boundary. Write the enforcement rule and its analysis down before writing code,
and note the observability requirement: a scope-rejected `create_broadcast` fails
`Error::Unauthorized` (`origin.rs:877`) and a filtered announce is dropped
silently, so both must be logged at warn level with the session peer and the
offending path, or authorization bugs and version skew become invisible
missing-peer symptoms.

Sub-step 1 (publish under your own id, main-compatible shape first). Publish each
local broadcast at `<room>/<endpoint-id>/<name>` via
`create_broadcast(path, Route::announced())` on the shared per-node
`origin::Producer`. Choose a hash or encoded id for the room prefix so it stays
within `MAX_PARTS` and a reasonable length. Keep `Route::new()` available for
pull-only sources that want reachability without advertisement. Verify the
existing point-to-point subscribe still works end to end before removing anything.

Sub-step 2 (derive discovery from the scoped announce stream). Watch a consumer
scoped to `<room>/` via `with_root` / `scope` and react to `announced()`: a `Some`
under `<peer>/<name>` is an appear, a `None` is a disappear. Wire the arriving
`broadcast::Consumer` straight to the existing subscribe path so
`RemoteAnnounced` and `BroadcastSubscribed` fire from the announce, replacing the
`live.subscribe(remote, &name)` dial-then-request at rooms.rs:565.

Sub-step 3 (map membership and metadata onto announce). Derive `PeerJoined` from
the first announce under `<room>/<peer>/` for an unseen peer id and `PeerLeft`
from the last unannounce for that id, per the section 3.3 table. Adopt the
presence-broadcast convention (`<room>/<peer>/presence`) if broadcast-free
presence is needed, since pure announce has no presence independent of media. Move
display name and user metadata into catalog extension sections
(`moq-media/src/catalog.rs` `IrohLiveExt`), and add tests covering the moq-net
0.1.11 catalog-serialization gotcha that previously dropped chat and user
extensions (`pubsub.md` section 4). Chat stays a track inside the broadcast, so
`ChatReceived` is unchanged.

Sub-step 4 (add the reconnect debounce). Because #2419 removed `ROUTE_LINGER`, a
reconnecting peer unannounces synchronously and re-announces from scratch. Hold
`PeerLeft` and decoder teardown for a short grace window after the last unannounce
for a peer, and if that peer re-announces within the window treat it as a
reconnect rather than a join/leave pair. This debounce is permanent room-layer
policy, not throwaway code awaiting a transport feature (`room-layer.md`
section 3.4).

Sub-step 5 (enforce authorship, delete the KV). Apply the scope resolution from
sub-step 0: scope each accepted session's subscriber-side origin to
`<room>/<remote-endpoint-id>/` for the single-room case, backed by a per-peer
moq-token rooted at `<room>/<endpoint-id>` with `put: [""]`, and apply the
accept-and-filter enforcement for the multi-room case, with the warn-level
rejection logging from sub-step 0. Once the proof test passes on the announce
path, delete the `iroh_smol_kv` client setup, `PeerState`, `update_kv`, and the
KV-update handler, and remove the `iroh-smol-kv` dependency from the room hot
path. Shrink gossip to presence beacons plus bootstrap; keep `RoomTicket`'s
`TopicId` under Variant A.

## Proof before deletion
An end-to-end room test on the announce path, before the KV code is deleted
(coordination point 1). Two or more peers join a room by ticket, each publishes a
broadcast under `<room>/<endpoint-id>/<name>`, and every other peer observes
`PeerJoined`, `RemoteAnnounced`, and `BroadcastSubscribed` derived from
`announced()`, receives media through the `broadcast::Consumer` that arrived with
the announce, and observes a prompt `PeerLeft` on deliberate unpublish and on
session close (validating #2419 immediate unannounce). The test must also assert
that a peer cannot announce under another peer's id (authorship enforcement,
section 5), that a transient session drop and reconnect within the grace window
does not surface a spurious `PeerLeft` / `PeerJoined` pair (the section 3.4
debounce), and that display name and user metadata survive a catalog round trip
(the section 4 serialization gotcha). The existing `Room` / `RoomHandle` example
must continue to pass unchanged, since the public API does not move.

## Coordination
- Coordination point 5 (rooms security): the multi-room `scope()` design question
  in `room-layer.md` section 7 phase 2 must be settled in sub-step 0 before any
  phase-2 implementation. The token path-scoping of section 5 is the credential
  and the path scope is the mechanism; both are required for
  announce-under-your-own-id.
- Coordination point 1 (proof before deletion): the KV, `PeerState`, and
  `update_kv` are deleted only after the announce-path proof test passes.
- Depends on the transport-delegate task landing phase 1 first (the shared
  per-node `origin::Producer` wired via `with_publisher` / `with_subscriber`), and
  on the Wave 0 pin bump for `create_broadcast`, `Route`, scoped `announced()`,
  moq-token `Scope`, and #2419 liveness.
- Migration sequencing (section 6): rooms move to announce while media stays on
  the current pipeline; `moq-media` has no iroh or room dependency, so decoders,
  jitter, and catalog handling are untouched.
- Honest regressions carried forward, not bugs to fix here: eager dialing (every
  discovered peer is dialed regardless of subscription interest, section 6), the
  relay-scope caveat and residual trusted-relay assumption for any relaying
  topology (section 5), and the loss of the free seamless-reconnect window
  (absorbed by the sub-step 4 debounce).

## Acceptance checklist
- The multi-room scope enforcement rule and its analysis are written down before
  any phase-2 code, with warn-level logging of `Unauthorized` and filtered-announce
  rejections carrying the session peer and offending path.
- Broadcasts publish at `<room>/<endpoint-id>/<name>` via
  `create_broadcast(path, Route::announced())` on the shared per-node origin.
- `RoomEvent` is derived from a room-prefix-scoped `announced()` consumer per the
  section 3.3 table, and the `broadcast::Consumer` arrives with the announce with
  no separate dial-then-request.
- Announce-under-your-own-id is enforced by the scoped session origin plus a
  per-peer moq-token (single-room) and accept-and-filter (multi-room).
- The reconnect debounce absorbs a transient session drop within its grace window.
- Display name and user metadata live in catalog extension sections, with tests
  covering the catalog-serialization gotcha.
- `iroh_smol_kv`, `PeerState`, `update_kv`, and the KV-update handler are deleted;
  gossip is retained only for bootstrap and presence.
- The proof test passes on the announce path, the public `Room` / `RoomHandle`
  API is unchanged, `rooms.rs` is roughly 450 to 550 LOC, and
  `cargo make check-all` passes.
