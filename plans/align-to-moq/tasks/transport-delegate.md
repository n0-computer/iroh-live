# transport-delegate

> Campaign: align-to-moq | Kind: task plan | Branch: align/transport-delegate |
> Read ../0-overview.md first.

Wave: 1
Depends on: Wave 0 pin bump (the moq release carrying the merged native stack, `moq-net`/`moq-native`/`hang` bumped in `Cargo.toml`)
Coupling: independent

## Goal

iroh-moq hand-rolls the MoQ-over-iroh handshake in `MoqSession::session_connect`
and `MoqSession::session_accept`, wiring a `moq_lite::Client`/`Server` over a
`web_transport_iroh::Session` and offering a single hardcoded ALPN. That code
duplicates `moq_native::iroh` almost exactly, over the same `moq-net` crate the
workspace already depends on, and the sibling `iroh-live-relay` crate already
uses the moq-native transport. This task deletes the duplicated handshake and the
hardcoded ALPN constant, delegates connect and accept to `moq_native::iroh`
(`EndpointConfig` / `Client::with_iroh` / `Server::with_iroh`), and registers and
offers the full `moq_net::ALPNS` list instead of one pinned version. iroh-moq
keeps its actor (connection dedup, node-wide broadcast fan-out, the iroh
`Router` `ProtocolHandler`, and the incoming-session stream), which has no
moq-native equivalent. Net effect: iroh-moq drops from 573 LOC to roughly
350 to 400 LOC, and its wire negotiation converges on the same multi-version
list the browser, the relay, and iroh-live-relay already speak.

## Evidence

- Deletion ledger, iroh-moq rows: `../../upstream/cut-plan.md` section 2, the `iroh-moq`
  table. The `lib.rs handshake + ALPN (~200)` row is verdict **cut**, replacement
  `moq_native::iroh` connect/accept plus `moq_net::{Client, Server}` with the full
  `moq_net::ALPNS` list registered and offered, prerequisite `local`, marked
  wire-visible with an e2e re-run required. The `lib.rs actor (~370)` row is
  verdict **keep**, no moq-native equivalent. The grouping note below the table
  fixes the partition: the ~200-LOC cut is the duplicated handshake plus the ALPN
  constant and connect/accept glue; the ~370-LOC keep is the actor core plus the
  public `Moq`/`MoqSession`/`IncomingSession` wrappers.
- Overlap analysis with `file:line` on both sides:
  `../upstream/comparisons/maps/moq-net-origin.md` section 6 (moq-native iroh
  transport surface) and `../upstream/comparisons/maps/room-layer.md` section 7
  (the overlap assessment: same crate underneath, same iroh-to-WebTransport
  bridge, same ALPN, moq-native already a workspace dependency, and the list of
  four things iroh-moq genuinely owns).
- Prior ALPN finding: `../../old/review-moq-usage.md` finding 2 (line 74, "ALPN
  pinned"), which recommends offering the full `moq_net::ALPNS` list on connect
  (primary plus `additional_alpns`) rather than a single hardcoded string. The
  finding recorded the const as `moq-lite-03`; the current code has since bumped
  to `moq-lite-04` (`iroh-moq/src/lib.rs:35`), so the pin has moved once already,
  which is exactly why hardcoding it is the wrong shape.
- Wave and staging: `../../upstream/cut-plan.md` section 3, stage 0, names this delegation as
  a local quick win, notes the merged main collapsed the older two-phase accept to
  one phase (so part of this is known rework at bump time), and states the ALPN
  change is wire-visible, so `e2e.rs` and `room.rs` must pass before and after.

## moq primitive adopted

`moq_native::iroh` plus `moq_net::{Client, Server}`, over `moq-net` (the crate
`moq_lite` already aliases):

- `moq_native::iroh::EndpointConfig` (`rs/moq-native/src/iroh.rs:100`) and its
  `bind(&self, quic)` (`iroh.rs:149`), which builds the iroh `Endpoint` and
  registers every MoQ ALPN plus H3: `moq_net::ALPNS` mapped to bytes at
  `iroh.rs:180`, then `web_transport_iroh::ALPN_H3` pushed at `iroh.rs:181`.
- `moq_native::iroh::connect(endpoint, url, addrs)` (`iroh.rs:253`, offers
  `moq_net::ALPNS[0]` as primary plus `ALPNS[1..]` as additional at
  `iroh.rs:269-270`) and `accept(conn)` (`iroh.rs:219`, single-phase, dispatches
  on `moq_net::ALPNS.contains(&alpn)` at `iroh.rs:245`). Both are `pub(crate)`, so
  the supported public entry points are the builders below.
- `Client::with_iroh(endpoint)` (`rs/moq-native/src/client.rs:212`) with
  `with_publisher`/`with_subscriber` (`client.rs:228`) and
  `Client::connect(url) -> (Session, Driver)` (`client.rs:337` `connect_inner`).
- `Server::with_iroh(endpoint)` (`rs/moq-native/src/server.rs:238`).
- `moq_net::ALPNS` (`rs/moq-net/src/version.rs:24`), most-preferred first; on
  current main `ALPNS[0]` is `ALPN_LITE_05` (`version.rs:25`), ahead of the
  `moq-lite-04` iroh-moq pins today, so adopting the list also gains lite-05
  negotiation.

Note the `(Session, Driver)` return of `Client::connect`
(`../upstream/comparisons/maps/moq-net-origin.md` section 3): the merged net layer
spawns nothing behind your back, so the delegated path must own driving the
`Driver` (spawn it, or step `Driver::poll`), which the current `session_connect`
does not have to think about. This is part of the known bump-time rework.

## iroh-live code changed

All in `iroh-moq/src/lib.rs` (573 LOC total, verified by read):

- Delete `pub const ALPN: &[u8] = b"moq-lite-04"` (`:30-35`). Its re-export
  `pub use iroh_moq::ALPN` in `iroh-live/src/lib.rs:19` and its use in
  `register_protocols` (`iroh-live/src/live.rs:168`,
  `.accept(iroh_moq::ALPN, ...)`) must move to the moq ALPN set. `live.rs` mounts
  the `ProtocolHandler` per ALPN, so it registers one accept arm per entry of
  `moq_net::ALPNS` (map to bytes as `EndpointConfig::bind` does at `iroh.rs:180`),
  or is restructured to bind through `EndpointConfig`.
- Replace the body of `MoqSession::connect` (`:275-286`), which today calls
  `endpoint.connect(addr, ALPN)` then `session_connect`, with a delegation to the
  moq-native client dial path (`with_iroh` plus `connect`).
- Delete `MoqSession::session_connect` (`:294-311`) and
  `MoqSession::session_accept` (`:316-333`): the hand-built `OriginProducer`
  pair plus `moq_lite::Client::new().with_publish(...).with_consume(...)` /
  `Server::new()...` handshake is exactly what `Client`/`Server` `with_publisher`
  /`with_subscriber` do. Rewire `MoqProtocolHandler::handle_connection`
  (`:183-194`), which calls `session_accept` at `:186`, onto the delegated accept.
- Keep, unchanged in behavior: the `Actor` (`:406-572`) with `handle_connect`
  dedup (`:545-571`), `handle_publish_broadcast` fan-out (`:529-543`),
  `handle_session` replay and lifecycle (`:499-527`); the public `Moq`
  (`:77-170`), `MoqProtocolHandler` shell (`:177-204`), `IncomingSessionStream`
  /`IncomingSession` (`:208-252`), and the `MoqSession` accessors
  (`remote_id`/`conn`/`subscribe`/`publish`/`origin_producer`/`origin_consumer`
  /`close`/`closed`, `:335-383`). The `origin: Origin` threading (`:82`, `:95`,
  `:113`, `:179`) stays; it seeds `with_publisher`/`with_subscriber` the same way
  it seeds the deleted `OriginProducer::new(origin)` calls.

The deleted surface is the ~200 LOC the ledger names; the ~370 LOC actor plus
wrappers stay.

## Steps

Adoption before deletion, each step a compiling commit.

1. Confirm the pin bump (Wave 0) has landed so `moq-native` on the merged-stack
   release is available and matches the `moq-net` alias; `iroh-live-relay` already
   depends on `moq_native::iroh::EndpointConfig` (room-layer.md section 7), so the
   dependency and features exist to extend.
2. Add a delegated connect helper alongside the existing `MoqSession::connect`,
   built on `Client::with_iroh(endpoint).with_publisher(...).with_subscriber(...)`
   and `connect(url)`, owning the returned `Driver`. Keep the old path in place;
   do not delete yet. `refactor:` commit.
3. Add a delegated accept helper for `MoqProtocolHandler::handle_connection`,
   built on the moq-native accept surface, again keeping `session_accept` present.
4. Register the full `moq_net::ALPNS` list (plus H3 if the WebTransport path is
   kept) in `iroh-live/src/live.rs` `register_protocols` and in endpoint binding,
   replacing the single `iroh_moq::ALPN`. This is the wire-visible change; land it
   as its own commit with the e2e evidence in the message (cut-plan section 6,
   wire-visible commits are the revert points).
5. Switch `MoqSession::connect` and `MoqProtocolHandler::handle_connection` to the
   delegated helpers; run the proof suite.
6. Delete `session_connect`, `session_accept`, the `ALPN` const, its
   `iroh-live/src/lib.rs` re-export, and any now-dead imports. `chore:` deletion
   commit containing nothing else, so a revert restores the old path cleanly.
7. Update the module doc on `iroh-moq/src/lib.rs:1-5` and the `ALPN` doc comment
   removal; fold doc edits into the code commits, never standalone (cut-plan
   section 6).

## Proof before deletion

Coordination point 1 gate: `iroh-live/tests/e2e.rs` (4 tests) and
`iroh-live/tests/room.rs` (6 tests) must pass on the new delegated path before
the `session_connect`/`session_accept`/`ALPN` deletion commit lands. Because the
ALPN change is wire-visible, run both suites before and after the ALPN commit and
record the result in the commit message (cut-plan section 3 stage 0, and the
risk register R-g note that stage 0 relies on `e2e.rs`/`room.rs` alone). A manual
interop check against a browser or relay peer that negotiates `moq-lite-05` is a
recommended extra, since the local suites exercise only the iroh-to-iroh path.

## Coordination

- Wire-visible change (coordination point in cut-plan section 6): the ALPN commit
  is the revert point if interop breaks; keep it isolated.
- Known bump-time rework (cut-plan churn accounting): the merged main collapsed
  the two-phase accept to one phase and returns `(Session, Driver)` from connect,
  so the delegated path must drive the `Driver`; this is expected, not a surprise.
- No zero-copy path is touched (coordination point 2 does not apply).
- This task produces the shared origin wiring that rooms phase 2
  (`rooms-announce`, Wave 2) builds on: cut-plan section 3 states stage 0 gates
  stage 5 phase 2. Keep the single shared `Origin` per node intact through the
  delegation so that later work has an origin to scope.

## Acceptance checklist

- [ ] `session_connect`, `session_accept`, the `ALPN` const, and its
      `iroh-live/src/lib.rs` re-export are deleted; connect and accept run through
      `moq_native::iroh` plus `Client`/`Server` `with_iroh`.
- [ ] The full `moq_net::ALPNS` list is registered on accept and offered on
      connect; no single hardcoded ALPN remains.
- [ ] The actor (dedup, fan-out, `ProtocolHandler`, incoming-session stream) and
      the public `Moq`/`MoqSession`/`IncomingSession` surface are unchanged in
      behavior.
- [ ] `iroh-live/tests/e2e.rs` and `iroh-live/tests/room.rs` pass, before and
      after the ALPN commit, with the evidence noted in the wire-visible commit.
- [ ] `cargo make check-all` is green (workspace, all features, tests, examples).
- [ ] iroh-moq is roughly 350 to 400 LOC (down from 573).
