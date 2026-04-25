# Worklog: overnight follow-ups on rooms-gossip-relays

Started: 2026-04-25
Mode: overnight
Plan: plans/overnight-followups.md
Branch: feat/rooms-gossip-relays

## Goal

Close every deferred item from the prior cycles that does not
require external upstream work, then run a full-branch
consistency review and a round of staff reviews. The branch
should land in a state where remaining deferrals are genuinely
blocked on upstream changes.

## Progress

### 2026-04-25 - organize and catalog

Read autocode guides in full. Confirmed the two writing
constraints I had drifted on: ASCII only in code and docs (no
em dashes), and one logical commit per change.

Cataloged deferred items from worklog and plan into ten
follow-ups (FU-1 through FU-10). Wrote
`plans/overnight-followups.md` with a priority order. Spawned
four investigation agents in parallel for the items that
needed source-level reconnaissance: FU-1 (kv expiry events),
FU-3 (relay reconnect shape), FU-7 (relay path scoping), and
FU-9 (RemoteBroadcast constructor surface).

Investigation outcomes recorded in the plan:

- FU-1 reduces to switching the room's KV subscription from
  `.stream()` to `.stream_raw()` and matching the existing
  `SubscribeItem::Expired` variant. The horizon is set on the
  KV writer so entries actually expire.
- FU-3 reduces to a watchdog task per relay session with
  exponential backoff, owned by the actor through an abort
  handle. `MoqSession::closed()` is the trigger.
- FU-7 needs an upstream change in `moq-relay`. Document and
  defer.
- FU-9 is a theoretical leak the existing call sites do not
  trigger. The design intentionally keeps the signal producer
  as a free utility. Skip this cycle.

Also fixed a single em dash that snuck into
`iroh-live/examples/subscribe_test.rs` line 22.

### 2026-04-25 - FU-1 implemented

Switched the room actor's KV subscription from `.stream()` to
`.stream_raw()` and added `handle_gossip_expiry` to react to
`SubscribeItem::Expired`. The handler removes the cached relay
hint, tears down every active subscription rooted at the
expired peer, and emits `RoomEvent::PeerLeft`. Extended the
actor with a periodic refresh tick at one third of the configured
KV horizon so live peers do not appear expired to neighbours.

`RoomBuilder::kv_expiry(horizon, check_interval)` exposes the
config so tests can shorten the horizon. Added
`peer_expiry_fires_peer_left` test (room.rs) that verifies a
peer that joined and then disappeared without unannouncing
surfaces as `PeerLeft` after the horizon.

Notable upstream observation: `iroh-smol-kv` ignores
`ExpiryConfig::check_interval` and runs `apply_horizon` on a
hardcoded 30-second period, so the test runs on that cadence
rather than the configured 200ms. Worth a follow-up upstream.

Commit: 3761231.

### 2026-04-25 - FU-3 implemented

Added `iroh-live/src/rooms/relay_session.rs` with self-
reconnecting `RelayDiscovery` and `RelayPublisher` wrappers.
Both use a capped exponential backoff (100ms initial, 2x
multiplier, 10s cap, reset to initial after 30s of stable
session) and reconnect transparently to the room actor.

`RelayPublisher` tracks active publishes as
`(wire_name, BroadcastConsumer)` pairs and re-issues every one
against each new session. The room actor uses
`publish` / `unpublish` to mutate the set; the prior
`RelayAnnounce` per-broadcast wrapper and
`refresh_relay_announces` plumbing are gone.

Backoff math is unit-tested. End-to-end coverage of reconnect
needs a restartable relay test fixture, which is a separate
piece of work and noted as the only outstanding hole on this
feature.

Commit: b7839d8.

### 2026-04-25 - FU-5 implemented

Replaced the CLI's `--relay <id> --api-key <jwt> --relay-path
<path>` triple with a single repeatable `--relay <SPEC>` flag.
`SPEC` parses as `<endpoint_id>[=<jwt>][@<path>]`. `SubscribeSource`
collapses to one shape carrying a broadcast name and a
populated `SourceSet`, ready for `Live::subscribe`. Both
publish-side fan-out and subscribe-side multi-source candidates
flow through the new shape.

The first cut introduced a `RelaySpec` newtype on the CLI that
duplicated `RelayOffer`'s fields. Phase 3 collapsed that into
`RelayOffer` plus a CLI-local `parse_relay_offer` function
wired through clap's `value_parser`. Five unit tests cover the
wire format.

Commits: 5c22825 (initial), 807328c (RelaySpec collapse).

### 2026-04-25 - FU-7 and FU-2 documented as upstream blockers

`RelayOffer`'s security doc now spells out that `moq-relay` does
not bind a JWT subject to the path's `<peer_id>` segment.
Operators who need that binding must mint per-peer JWTs.
`iroh-live-relay`'s crate-level Auth section repeats the
constraint so operators see it in both places.

`SeamlessMediaTracks`'s audio-gap doc points at `OutputHandle`'s
fade machinery as the path to closing the seamless audio swap,
instead of leaving the future-work note unspecific.

Commit: f74f32d.

### 2026-04-25 - Phase 3 consistency review

Spawned a focused exploration agent to audit the full diff for
API consistency, ergonomics, half-finished implementations, and
code warts. Nineteen findings; ten survived the opposing-stance
test:

- F-3 was real: `RelaySpec` and `RelayOffer` had identical fields.
  Collapsed to `RelayOffer` plus a CLI parser fn. Commit 807328c.
- F-13 was real: `Broadcaster::reconcile` lacked a doc header.
  Added one. Commit 807328c.
- The remaining findings were either false positives (F-1 claimed
  a name collision that does not exist; F-11 claimed missing docs
  on items that all have one-line doc comments), intentional
  design (F-2 RelayOffer/RelayTarget split, F-5 `Subscription`
  Clone semantics, F-6 Room/RoomHandle delegating methods), or
  genuinely deferred (F-9 audio swap, F-10 reconnect tunables).
  Reasoning recorded here so the next reader does not retread.

### 2026-04-25 - Phase 3.5 cleanup

Walked the worklog and plan files to remove em dashes and en
dashes (autocode rule: ASCII only in code and docs). The
existing `worklogs/2026-04-24-rooms-gossip-relays.md` had 27
occurrences and the older plan `plans/rooms-gossip-relays.md`
had five. Replaced each with a colon or a sentence break as
context dictated.

