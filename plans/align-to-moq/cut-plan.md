# Cut plan: the alignment deletion ledger

> Campaign: align-to-moq | Kind: reference | Read `0-overview.md` first. This
> is the deletion and replacement ledger for the alignment tasks in `tasks/`;
> the media deletion ledger (codecs, capture, render, audio devices) is
> `../upstream/cut-plan.md`.

This document is the standing ledger for what the align-to-moq campaign
deletes or reshapes: the transport handshake, the pubsub and pipeline wiring,
the adaptive and sync layers, and the room layer. It stops at the media
boundary: rusty-codecs, rusty-capture, and moq-media's device and file modules
belong to `../upstream/cut-plan.md` and its counterpart branches. Every
replacement named here is on moq main (`/home/bit/Code/rust/moq`, HEAD
`3a3e0ea8`).

Rows cite the comparisons with the shorthand codes: 1-code-map =
`../upstream/comparisons/iroh-live-code-map.md`, 2-moq-inventory =
`../upstream/comparisons/moq-inventory.md`, 5-compare =
`../upstream/comparisons/pubsub.md`, 9-room-layer = `room-layer.md`.

## 1. Principles

**P1. Nothing is cut until its replacement is proven in-tree.** A module is
deleted only after a working example or e2e test passes on the new path. The
gates here are `iroh-live/tests/e2e.rs`, `iroh-live/tests/room.rs`, and
`moq-media/tests/pipeline_integration.rs`.

**P2. One release gate, labeled on every row.** Local cuts (the transport
handshake delegation, the announce room migration phase 2) proceed against the
release iroh-live pins today. Release cuts need the bump to the moq release
carrying the merged stack (`set_latency`, `discontinuity()`, `Reserved`,
`recv_bandwidth`, announce migration and `ROUTE_LINGER`).

## 2. The cut ledger

Verdict vocabulary as in `../upstream/cut-plan.md` and
`../upstream/DISPOSITION.md`: adopt-theirs, keep, merge. Nothing in this
ledger is upstream-ours: the alignment campaign contributes nothing to moq, it
adopts what moq already ships. LOC figures are from 1-code-map section 2.

### moq-media, alignment modules only

| Module | LOC | Verdict | Replacement | Gate | Evidence |
|---|---:|---|---|---|---|
| publish.rs | 1,508 | merge | simulcast registry, `SharedVideoSource`, and leasing stay (no moq equivalent); `start_track` per-track wiring collapses onto `encode::Producer`; the catalog priming hack is replaced by `Reserved` semantics | media stage M1, D9 track naming, release | 5-compare sec 2, 10 |
| pipeline/ | 1,212 | merge | encode pipelines (~404 LOC) collapse onto `moq_video`/`moq_audio` `encode::Producer`; decode loops stay, internals swap to the sans-IO `moq_video::decode::Decoder` under our OS threads | media stage M1, release | 5-compare sec 2, 10 |
| subscribe.rs | 1,566 | keep (merge at the edges) | quality selection, hot-swap, and the adaptation driver have no upstream counterpart; decoder-prep internals swap on adoption | release for `set_latency`, `discontinuity()` | 5-compare sec 3, 10 |
| transport.rs | 204 | merge | `MoqPacketSink` half replaced by `encode::Producer`; `MoqPacketSource` stays feeding the sans-IO decoder | media stage M1, release | 5-compare sec 2 |
| adaptive.rs + net.rs | 621 | keep | the only Rust subscriber-side ABR on either side; `sync-adaptive-align` reads moq-mux per-rendition `Estimate{jitter,bitrate}` into it | release | 5-compare sec 5, 10 |
| sync.rs + playout.rs | 512 | keep | no Rust playout clock exists upstream; `sync-adaptive-align` reads catalog `jitter` now populated by moq-mux `Metrics` | local fixes need nothing | 5-compare sec 6, 10 |
| stats.rs | 494 | keep | does not overlap moq-stats or moq-net session stats | n/a | 5-compare sec 7 |
| source_spec.rs | 499 | keep | CLI parsing, ours | n/a | 1-code-map |
| frame_channel.rs | 299 | keep | enables decoder hot-swap; no counterpart | n/a | 5-compare sec 3 |
| publish/controller.rs | 322 | keep | app-facing orchestration | n/a | 5-compare sec 2 |
| chat.rs | 182 | keep | none upstream, Rust or JS | n/a | 5-compare sec 8 |
| catalog.rs | 75 | keep | already the sanctioned `CatalogExt` shape | n/a | 5-compare sec 4 |

### iroh-moq (572 LOC)

| Module | LOC | Verdict | Replacement | Gate | Evidence |
|---|---:|---|---|---|---|
| lib.rs handshake + ALPN (~200) | ~200 | adopt-theirs | `moq_native::iroh` connect/accept plus `moq_net::{Client, Server}`; full `moq_net::ALPNS` list registered and offered (`tasks/transport-delegate.md`) | local; wire-visible, e2e re-run required | 9-room-layer sec 1.1, 3.1 |
| lib.rs actor (dedup, origin fan-out, `ProtocolHandler`, incoming stream) | ~370 | keep | no moq-native equivalent; fan-out reshapes onto one shared `OriginProducer` | stage A0 | 9-room-layer sec 3.1, 6 |

Grouping note: the strictly duplicated handshake is roughly 120 LOC and the
actor core roughly 200; the two rows instead partition the whole 572-line
file. The cut row (~200) is the duplicated handshake plus the ALPN constant
and connect/accept glue that moq-native delegation obsoletes; the keep row
(~370) is the actor core plus the public session and stream wrappers (`Moq`,
`MoqSession`, `IncomingSession`).

### iroh-live (1,734 LOC)

| Module | LOC | Verdict | Replacement | Gate | Evidence |
|---|---:|---|---|---|---|
| rooms.rs | 695 | merge | KV half (~200-250 LOC) replaced by scoped moq `announced()` streams; gossip retained for bootstrap (Variant A); event derivation added (~100-150 LOC); unannounce debounce added locally, deleted once the release carrying #2241 is pinned (`tasks/rooms-announce.md`) | stage A0 first; local suffices for phase 2 | 9-room-layer sec 3, 6 |
| util.rs | 185 | merge | `available_bps` cwnd math (~20 LOC) replaced by `moq_net::bandwidth` `recv_bandwidth`; PathStats kept for loss and congestion | release | 5-compare sec 7 |
| live.rs, call.rs, subscription.rs, rooms/publisher.rs, ticket.rs, lib/types | 854 | keep | public API layer, tickets, sugar | n/a | 9-room-layer sec 6 |

### Expected LOC removed

| Crate | Scenario A (release bump) |
|---|---:|
| moq-media (alignment rows) | ~110 now (priming hack via `Reserved`, `set_latency` path, stale docs); ~700 with the media stage M1 done (+encode wiring, transport sink half) |
| iroh-moq | ~200 (handshake, ALPN) |
| iroh-live | ~165 (rooms KV half, debounce, bandwidth math, net of additions) |

Combined with the media ledger, the two campaigns total about 4,800 LOC (12%)
on the release bump alone and about 17,400 (42%) with the upstream leaves
accepted; the breakdown is in `../upstream/cut-plan.md`.

## 3. Ordering

Stage A0 gates A2; A1 waits on the media codec adoption (stage M1 in
`../upstream/cut-plan.md`) because the producers assume moq's codec layer; A2
runs independently of media entirely.

**Stage A0: transport quick wins (local).**
Delegate the iroh-moq handshake to moq-native and register the full ALPN list
(`tasks/transport-delegate.md`); fix the stale sync.rs jitter-field claims and
the stale `adaptation_task` doc comment, and read catalog `jitter` into the
clock (`tasks/sync-adaptive-align.md`); report the moq-net dynamic-handler
registration race and the catalog-priming sharp edge upstream as issues (the
priming hack itself is only properly replaced by `Reserved`, stage A1). Entry
condition: the Wave 0 pin bump. The ALPN change is wire-visible, so `e2e.rs`
and `room.rs` must pass before and after.

**Stage A1: pubsub and adaptive re-plumb (release-gated, after media M1).**
Collapse the encode pipelines and `start_track` wiring onto `encode::Producer`
(needs D9 deterministic track naming for simulcast); replace the catalog
priming hack with `Reserved` gating; adopt publish-side `Metrics` so
advertised bitrate and jitter are measured; adopt `set_latency` for runtime
`PlaybackPolicy` and `discontinuity()` for clean decoder flushes; replace the
`available_bps` cwnd math with `recv_bandwidth` (`tasks/catalog-adopt.md`,
`tasks/pubsub-align.md`, `tasks/sync-adaptive-align.md`). Entry condition:
media stage M1 (the producers assume their codec layer), the release bump, D9
answered. Without D9 the simulcast registry cannot name tracks
deterministically, and without `Reserved` early subscribers race the catalog.

**Stage A2: rooms (room-layer.md phases 2 and 3).**
Phase 2: announce-based rooms on the shared origin with gossip retained for
bootstrap (Variant A), smol-kv removed, metadata moved to catalog extensions,
and an unannounce debounce standing in for migration. Phase 3, on the release
bump: delete the debounce in favor of #2241 migration and `ROUTE_LINGER`,
adopt `AnnounceOk` roster completion and `Publish` guards, and optionally
moq-stats (`tasks/rooms-announce.md`). Entry condition: stage A0 for phase 2
(no origin to scope otherwise); the release bump for phase 3. Removing the
debounce before the bump reintroduces join/leave flapping on reconnect.

## 4. Risk register

- **D9 track naming.** Without a deterministic naming answer from upstream,
  stage A1's simulcast wiring cannot land; open question discussed in
  `../upstream/comparisons/pubsub.md` section 2, current proposal: derive
  rendition track names from the catalog rendition key.
- **Rooms security scoping.** The multi-room scoping question of
  `room-layer.md` phase 2 must be settled before that phase's implementation
  (campaign coordination point 2).
- **Test gaps.** There is no adaptive-switching integration test driving
  `adaptation_task_v2` end to end for stage A1, and the stage A0 ALPN change
  relies on `e2e.rs`/`room.rs` alone; close these before the respective
  stages.
- **API churn.** The same freshness protocol as the media ledger applies:
  citations pinned to `3a3e0ea8` are direction, not contract; re-verify
  before each stage (`../upstream/cut-plan.md` R-b).

## 5. Commit strategy

As in `../upstream/cut-plan.md` section 6: small compiling commits with
conventional prefixes, `cargo make check-all` green at every commit, deletions
in deletion-only commits, no doc-only commits, no push without an explicit
ask. Wire-visible changes (the stage A0 ALPN commit, the stage A2 announce
paths) land as single commits with the e2e evidence noted in the commit
message, since they are the revert points if interop breaks.
