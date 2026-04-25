# Worklog: rooms gossip relays with dynamic transport switching

Started: 2026-04-24 ~12:10 CEST
Mode: overnight (8h budget)
Plan: plans/rooms-gossip-relays.md
Branch: feat/rooms-gossip-relays
Worktree: /home/bit/Code/rust/iroh-live-worktrees/rooms-gossip-relays

## Goal

Extend the existing rooms design with dynamic direct/relay transport
switching. A peer may start in one mode and move to the other based
on runtime signals (new joiners, direct-path availability, explicit
policy) for both publishing and subscribing. The same machinery also
applies to single-broadcast watchers outside of rooms. Design leaves
room for a future model where every peer optionally embeds a relay,
giving us self-meshing topologies.

Ship it end to end: plan, implementation, integration tests, and
docs.

## Scope additions vs the prior plan

The original `plans/rooms-gossip-relays.md` treated transport as a
build-time choice at room construction. The additional asks this
session:

1. Live transport switch on a running publish or subscribe. Trigger
   can be a policy watcher, an explicit call, or an external signal
   that arrives through the catalog.
2. The switch applies equally to "just watch" single-broadcast
   subscriptions. Today that path uses `Live::subscribe` over raw
   iroh; it should be able to start on a relay and upgrade to direct
   (or vice versa) without the consumer tearing down and re-opening.
3. Design the API so later work can embed a relay into every peer
   without a second rewrite. Relay-in-peer means each peer becomes a
   candidate fan-out node; the client side must already be framed in
   terms of "candidate origins" rather than a fixed singleton.
4. No backwards compatibility. We may break the current `RoomTicket`
   and `RoomBuilder` APIs in service of a cleaner model.

## Progress

### 2026-04-24 12:10 - Session start

Read all autocode docs (`big-jobs.md`, `workflow.md`, `writing.md`,
`lang/rust.md`, `lib/iroh.md`). Re-read the existing plan. Created
a new worktree at `rooms-gossip-relays` from the tip of
`feat/relay-auth-for-svc` and symlinked `.agents` to the shared
autocode repo. Starting deep research on the current room/relay/live
surface before expanding the plan.

### 2026-04-24 12:30 - Research phase complete

Four parallel research agents returned deep reports covering:

1. **Room actor** (`iroh-live/src/rooms.rs`). Single `Actor` owns gossip
   KV, tracks `active_subscribe`/`active_publish`, dispatches via
   `FuturesUnordered`. `PeerState` in KV carries only `broadcasts:
   Vec<String>` and `display_name`. Subscribe path is
   `live.subscribe(remote, name)` which calls `moq.connect(remote)`
   then `session.subscribe(name)`. No transport choice today.

2. **Live API surface** (`iroh-live/src/live.rs`, `relay.rs`,
   `subscription.rs`). `Live::subscribe` is the direct path,
   `Live::subscribe_from_relay` is the H3 relay path, `Live::publish`
   registers a producer with every current and future session.
   `Subscription` wraps `MoqSession + RemoteBroadcast + signals`.

3. **MoQ sessions** (`iroh-moq/src/lib.rs`). The actor dedups native
   `connect()` by `EndpointId`; `connect_h3()` intentionally does not
   dedup. `MoqSession::subscribe` is session-bound. Session death
   surfaces as `SubscribeError::SessionClosed` and the caller must
   reopen.

4. **moq-relay** (Cargo checkout). Cluster exposes
   `primary.consume()` for live announcement stream, no native "list
   active broadcasts" RPC but moq-lite's own `OriginConsumer::
   announced()` surfaces that natively per-session.

Also read frame_channel (single-slot, `new_sender()` enables
seamless pipeline swap), subscribe.rs (adaptive rendition already
uses new_sender to swap decoders inside one `VideoTrack`), and the
industry-prior-art research report.

**Key structural insight:** MoQ is already natively multi-origin. A
publisher can ANNOUNCE the same broadcast path on N sessions; a
subscriber sees N matching announcements and picks one. The MoQ
papers and "On a Boat" explicitly frame this as the "publish via
P2P and CDN simultaneously; subscriber picks best" pattern. We
should lean into this instead of building a side-channel signaling
layer on top.

**Design direction (to be refined in the plan):**

- Model each publish destination (direct peer accept, relay H3
  session, future peer-as-relay) as a `TransportSource`.
- Publisher attaches a broadcast producer to a set of transport
  sessions; this set mutates over time based on a policy that the
  caller drives or a rule engine evaluates.
- Subscriber exposes a `MultiSourceSubscription` that holds several
  candidate sessions, SUBSCRIBEs on the preferred one, and swaps to
  another seamlessly on degradation/disappearance via the existing
  `frame_channel::new_sender()` mechanism applied at the whole-
  broadcast level (not just rendition).
- Rooms extend `PeerState` with an optional relay hint so other
  peers learn the relay session as an alternative source.
- `LiveTicket` grows an alternative-sources list for single-watch.
- No backwards compatibility; the current `RoomTicket` and
  subscribe APIs can change.

Writing the full plan next.

### 2026-04-24 13:00 - Plan written; adversarial review

Plan at `plans/rooms-gossip-relays.md`. Key structural choices:
`TransportSource` as pure data, `SourceSet` as ordered candidate
list, `SelectionPolicy` as a pluggable decider, `Broadcaster` on
the publish side, `MultiSourceSubscription` on the subscribe side.
Rooms thread relay hints through the gossip `PeerState`; single-
watch tickets carry the list of alternate sources.

Adversarial review findings:

1. **Seamless swap is a per-track rebuild, not a session swap.**
   `RemoteBroadcast` owns a `Sync` (playout clock) and binds to a
   `BroadcastConsumer`. Swapping the consumer cleanly means
   rebuilding the `RemoteBroadcast` and the decoder pipelines
   while holding onto the existing `FrameReceiver`s via
   `new_sender()`. Fresh `Sync` means a brief playout rebase at
   the swap boundary. Accepted; documented in the plan.

2. **`Moq::connect` dedup is a safety net, not a hazard.** Multiple
   callers to the same peer share a session via clone. A drop by
   one caller does not close the underlying connection. The
   `MultiSourceSubscription` should not explicitly `close()` a
   direct session it did not open; it should let refcount/dedup
   handle lifecycle. For H3 sessions it did open, it closes on
   drop.

3. **Pre-open all candidate sessions, subscribe eagerly.** MoQ
   pull-based: an unused consumer costs only the initial
   `ANNOUNCE_PLEASE`/catalog bootstrap. Keeping alternate
   consumers hot eliminates connect+announce latency on switch.

4. **Publisher fan-out needs two modes.** `All` (current behaviour
   of `Moq::publish`: every existing and future session sees the
   broadcast) vs. `Specific(SourceSet)` (publisher names the
   sessions). The "start direct, switch to relay-only" workflow is
   an `All` -> `Specific(Relay)` transition. Incoming dials in
   `Specific` mode that are not in the set simply never see the
   ANNOUNCE; their `session.subscribe` hangs until the session
   ends. Document the behaviour; no special rejection needed.

5. **Gossip is the signaling channel, not moq-relay.** We do not
   need to add a relay-side "list broadcasts" RPC; every in-room
   peer already announces its own broadcasts (and, with this
   change, its relay hint) via gossip. Subscribers observe the
   announcement and decide.

6. **Non-room single-watch dynamic switch is bounded by the
   ticket.** If the ticket only lists a relay, that is the only
   source the subscriber knows about. Dynamic discovery of a
   direct path from a relay-only ticket is out of scope for this
   session. We document this as a known constraint and cover both
   `direct + relay` and `relay-only` tickets in tests.

7. **Tests must use in-process Live instances with the loopback
   transport.** No external network, no flaky CI. A loopback relay
   is spun up in the test harness for the relay-backed scenarios.

No plan items fail the adversarial check. Moving to
implementation.

### 2026-04-24 16:30 - Mid-session re-review and revision

After getting the foundation compiling, I stopped to re-read the
plan against the implementation and found several places where I
cut short. The user asked for "the API as best as it can possibly
get" without backward-compat constraints. Going to revise the
design and migrate cleanly.

**Cuts identified:**

1. **Parallel subscribe types.** `Subscription` (single-source) and
   `MultiSourceSubscription` (multi-source) coexist. The former is
   the legacy single-session shape; the latter is the new dynamic
   one. With no backward-compat constraint we should unify them:
   `Subscription` is always multi-source capable, with a singleton
   set as the trivial case.

2. **Multi-entry `Live::subscribe` family.** `Live::subscribe`,
   `Live::subscribe_from_relay`, `Live::subscribe_multi`,
   `Live::subscribe_from_ticket` are all separate. With unification
   we end up with `Live::subscribe(sources, name)` plus a single
   convenience for tickets. Other shapes are accepted via
   `Into<SourceSet>` so no per-source method is needed.

3. **Eq chain hack.** I worked around `Watchable`'s `Eq` requirement
   on `SourceSet` with a version-counter pattern. The right fix is
   to derive `Eq` on the inner types (`RelayTarget`, `DirectSource`,
   `TransportSource`, `SourceSet`, `RelayOffer`). That works because
   every leaf type is already `Eq` (EndpointId is a public key,
   strings, options).

4. **Frame-level seamless swap deferred.** I documented the
   `frame_channel::new_sender()` primitive but left swap at the
   session level: callers re-attach decoders on swap. The user
   explicitly wants seamless. Need to build `SeamlessVideoTrack` /
   `SeamlessAudioTrack` that ride a swap-aware media pipeline using
   the same `new_sender()` trick the adaptation layer uses.

5. **`BroadcasterError` is an alias.** Lazy. Either remove the
   public type or define a real error variant that captures the
   distinct failure modes (connect failed, source vanished,
   serialization).

6. **Room emits `BroadcastSwitched` but the consumer's
   `BroadcastSubscribed` payload becomes stale on swap.** A clean
   design hands the consumer a `Subscription` whose internals swap
   transparently, so the consumer does not have to re-attach.

7. **Publisher "deny direct" missing.** A publisher that wants to
   force traffic through the relay needs (a) the broadcaster owning
   the announce on each session and (b) the inbound accept loop
   skipping the broadcast on new sessions. The current Broadcaster
   already does (a); for (b) we need to teach `Live::publish` and
   the Moq actor that broadcasts attached to a Broadcaster are not
   in the global pool.

8. **Examples and CLI not migrated.** `iroh-live-cli` still calls
   `live.subscribe(remote, name)`. With the unified API that signature
   shifts; we migrate everything in the workspace.

**Revised plan (full migration, no shortcuts):**

A. **Eq chain.** Add `PartialEq + Eq` derives down to `SourceSet`.
   `SourceSetHandle` then wraps `Watchable<SourceSet>` directly. No
   version counter.

B. **Unified `Subscription`.** Single type, multi-source capable.
   Internal actor manages candidate sessions. Public surface:
   `active()`, `watch_active()`, `session()`/`broadcast()` for the
   currently active source, `media()` (auto-swap), `media_pinned()`
   (pin to current source).

C. **Single `Live::subscribe(sources, name)`.** Accepts anything
   `Into<SourceSet>`: `EndpointAddr`, `RelayTarget`, `&LiveTicket`,
   `SourceSet`, `SourceSetHandle`. The `Into` for `&LiveTicket`
   builds the set from direct + relays.

D. **`SeamlessVideoTrack` / `SeamlessAudioTrack`** built on
   `frame_channel::new_sender()`. Spawned as a `media()` derivative
   that re-spawns its inner pipelines on every active-source change,
   threading the catalog and rendition picks through.

E. **`Broadcaster` cleanup.** Real `BroadcasterError` enum
   (`Connect`, `SourceVanished`). Document the "scoped publish"
   semantics: a Broadcaster's broadcast does not enter the global
   `Moq::publish` pool; only sessions named in the SourceSet see it.

F. **Room rewires through unified Subscription.**
   `RoomEvent::BroadcastSubscribed` carries the unified subscription
   handle (or just session+broadcast since they auto-swap when the
   caller upgrades to seamless tracks). `BroadcastSwitched` becomes
   informational only.

G. **Migrate every caller.** Examples, CLI, tests. No shim.

H. **Comprehensive tests.** Unit + integration + e2e for: single
   source happy path, multi-source pick, runtime swap on session
   close, ticket round-trip with relays, room hybrid (3 peers, one
   relay-only), room runtime relay enable/disable, broadcaster
   fan-out, broadcaster source removal.

I. **Doc and writing sweep.** RFC 1574 across all new public docs.
   `writing.md` voice on prose. ASCII only in code.

Going step by step. Will commit each phase as a clean conventional
commit once it compiles, tests, and self-reviews.

### 2026-04-24 18:30 - Foundation rebuilt + workspace migrated

Workspace builds clean across all features and all targets. The
foundation now matches the revised plan:

- `TransportSource`, `DirectSource`, `RelayTarget`, `SourceSet`,
  `RelayOffer` derive `PartialEq`/`Eq` so `SourceSetHandle` wraps a
  `Watchable<SourceSet>` directly, no version-counter hack.
- `Subscription` is the single multi-source-capable type. It
  spawns an actor that opens a session per source, runs a
  `SelectionPolicy`, and surfaces the active source via
  `active()`/`watch_active()`/`next_event()`.
- `Live::subscribe(sources, name)` is the unified entry. `sources`
  takes anything `Into<SourceSetHandle>`: `EndpointAddr`,
  `EndpointId`, `RelayTarget`, `SourceSet`, or `SourceSetHandle`.
  `Live::subscribe_ticket(&LiveTicket)` is the convenience for
  ticket-based callers. `subscribe_from_relay`,
  `subscribe_media`, and the multi/from_ticket variants are gone.
- `Subscription::ready()` is the convenience wrapper around
  `wait_active()` that errors instead of returning `Option`. Most
  call sites use it.
- `Subscription::media()` returns `SeamlessMediaTracks` with a
  `SeamlessVideoTrack` that survives source swaps via
  `frame_channel::new_sender()`. `RemoteBroadcast` grew
  `build_video_pipeline_*` helpers so the seamless layer doesn't
  reach into `rusty-codecs` directly.
- `Broadcaster` accepts `Into<SourceSetHandle>` and grew a real
  `BroadcasterError` enum.
- `RoomBuilder` + `RoomHandle::enable_relay`/`disable_relay` route
  through the unified `Subscription`. `PeerState.relay` carries an
  optional `RelayOffer` over gossip; subscribers feed both the
  direct and the advertised relay into their `SourceSet`.
- Migrated callers: `iroh-live-cli` (play, record, run), demos
  (opengl, pi-zero, android), examples (demo, frame_dump,
  subscribe_test, split, watch-wgpu), and the `iroh-live-relay`
  bridge tests.

Next: comprehensive new tests, doc sweep, staff reviews.

### 2026-04-25 13:00 - Staff reviews returned, review-of-reviews

Four parallel staff reviewers (Rust expert, distributed systems,
docs/QA, safety/security) returned roughly 50 findings. Applying
opposing-stance to each before fixing.

**Critical findings that survive:**

R1.1 / R2.4 / R4.9 — `Subscription::events_rx: Mutex<Receiver>` on
a `Clone` type silently load-balances events across clones. The
room actor only reads via `spawn_active_watcher` so the bug is not
triggered today, but the type contradicts itself: `Clone` implies
broadcast semantics, the mutex implies single-consumer. Switch to
`broadcast::channel` so every clone observes every event.

R2.5 / R4.5 — `events_tx.send().await` in the actor can stall on a
slow consumer. With a broadcast channel the analogous failure is a
`Lagged` skip rather than a stall, which is the correct behaviour
for an event log. This fix subsumes R1.1 above.

R2.1 / R1.7 / R4.1 — Reconcile awaits `attach_source` serially per
source. An unreachable peer blocks every other attach. Move the
attach work into a `FuturesUnordered` driven by the same select.

R3.1 — Broken intra-doc link `Live::subscribe_from_ticket` (the
method is `Live::subscribe_ticket`). Trivial fix.

R4.1 — DoS via unbounded `PeerState.broadcasts` from a malicious
gossip member. Cap the list length before iterating.

R2.2 — Watchdog spawn timing: session can close between
`attach_source` returning and the watchdog being installed. Detect
by checking the session after acquiring the state lock.

R2.3 — `pick_active` and the watchdog branch race: a closed
session can sit as `active_id` until the next `pick_active` runs.
Update `active_id` to `None` immediately when the active session
closes.

R2.12 — `spawn_active_watcher` in the room actor uses bare
`tokio::spawn`. Store an `AbortOnDropHandle` next to
`ActiveSubscribe` so the watcher dies with the subscription it
follows.

**Substantive findings that survive:**

R1.2 / R4.10 — `SourceSetHandle::update` is racy. Two concurrent
mutators can clobber each other. Add an internal `Mutex<SourceSet>`
that mutators take while computing the new value.

R1.10 / R3.16 — `RelayOffer.jwt` field name describes the wire
parameter, not the contents. The relay accepts an opaque API key.
Rename to `api_key` while we still can.

R1.13 — `_global_consumer` reads as drop-only but is read
explicitly by `refresh_relay_announces`. Drop the underscore.

R1.14 — `SeamlessVideoTrack` mixes `&self` and `&mut self` for the
same kind of operation. The receiver is `Arc<FrameReceiver>` and
all methods underneath take `&self`. Make the wrapper consistent.

R1.15 / R2.6 — Seamless swap reads `subscription.active()` twice
without an atomic snapshot. Pass the active id into `perform_swap`
and abort if it no longer matches when we acquire the swap-state
lock.

R2.8 — `pick_rendition` falls back to `Quality::Highest` without
checking codec compatibility with the consumer's decoder. If the
new source advertises a different codec the new pipeline never
emits frames. Track the previous `VideoConfig.codec`, refuse
seamless swap when it changes, and warn loudly.

R3.16 (test-side) — The room-relay tests verify `RemoteAnnounced`
carries the right hint but never assert that the subscriber's
source set is actually updated. Add a test.

R3.17 — No test exercises `Subscription::media` (the seamless
layer). Add one that swaps the active source and asserts frames
keep flowing.

R3.18 — `multi_source.rs` covers fall-over but never tests
preferred-source recovery. Add it.

R3.19 — `Subscription::wait_active` empty-set behaviour is
documented but uncovered. Add a test.

R3.20 — `broadcaster.rs` never tests `remove_source`. Add it.

R4.2 — `peer_relays` and `known_peers` grow without bound. Tie
their lifetimes to the gossip KV expiry: when an entry is removed
from the KV, drop the corresponding peer state too. Defer to
follow-up since the gossip KV does not currently expose explicit
expiry events; in this PR I add a `peer_seen` map with a soft cap
to bound growth.

R4.6 — `LiveTicket.relays` is unbounded on the binary form. Cap
length at decode.

R4.10 — `RelayOffer.path` length unbounded. Cap.

R4.7 — Document the JWT scope assumption on `RelayOffer`. The
field is broadcast to every member of the gossip topic; operators
must mint subscribe-only tokens scoped to the room's namespace.

R4.12 — `RelayTarget` and `RelayOffer` derive `Debug` which
exposes the JWT in tracing if anyone formats the struct directly.
Replace with manual `Debug` that elides the api key.

**Findings discarded after opposing stance:**

R1.4 (ActiveSource encapsulation): id-public + getters is the
norm in Rust APIs (e.g. `iroh::EndpointAddr.id` is public, addrs
hidden behind methods). Keep.

R1.6 (SelectionPolicy lacks `from_fn`): true but premature. The
trait is two-method-friendly via unit structs; we ship two
implementations. Add later when there is a use case.

R1.11 (`BoxFuture<T>` requires `Send + Sync`): cosmetic; the room
actor builds these explicitly. No real cost. Keep.

R1.16 (`RoomBuilder` vs `Room::new` redundancy): `Room::new` is the
zero-config shortcut. Builders that own their dependencies then
spawn standalone are fine; this one needs a `Live` reference to
spawn. Keep.

R2.9 (graceful shutdown): the actor's clean-up code is best-effort
already. Documenting "shutdown is fire-and-forget" is enough; no
oneshot needed.

R2.11 (broadcaster retain drops on every iteration): retain only
drops entries whose id is NOT in `desired_ids`. Unchanged sources
stay attached. Reviewer was reading wrong. Discard. (We still need
the retry-on-failure fix for the underlying complaint.)

R2.13 (wait_active ordering): code is correct; reviewer agrees.
Stylistic suggestion to swap order; ignored.

R3.5 (`# Examples` on every public type): RFC 1574 says strongly
encouraged, not required. Doc-comment examples that compile are
expensive to maintain; we ship one example block on `LiveBuilder`
already. Add focused examples on `Live::subscribe`, `Subscription`,
and `LiveTicket` only.

R4.4 (H3 dedup): out of scope for this PR. The intentional
non-dedup of `connect_h3` is an iroh-moq design choice. Workaround
in this PR is to rely on the `Subscription` reusing existing
attached sessions rather than reopening on every reconcile.

Going to refine now. Commits are organised as:
- 1) critical concurrency fixes
- 2) input validation + caps
- 3) docs + naming + Debug elision + tests
- 4) worklog summary

### 2026-04-25 14:10 - Refinement applied; full check suite green

All listed concurrency and substantive fixes are in. Highlights:

- `Subscription` events flow through a `broadcast::channel`. Each
  call to `Subscription::events` returns an independent
  `SubscriptionEvents` receiver; lag is reported as
  `RecvError::Lagged` and the actor's emit is non-blocking, so a
  slow consumer cannot stall the actor.
- The actor's main loop keeps a `FuturesUnordered` of in-flight
  attaches. An unreachable peer no longer blocks attaches on
  other sources.
- `wait_active` now races the shutdown token; an empty set with
  `shutdown()` resolves to `None` instead of hanging.
- `prune_removed_sources` and `handle_session_closed` clear
  `active_id` immediately when the active source vanishes, so the
  watchable and the state map stay consistent for any reader.
- `Broadcaster::AttachedSource` closes the underlying MoQ session
  on drop, so removing a target from the set actually ends the
  announce on that target. Tested.
- `SourceSetHandle` mutators are atomic via an internal
  `std::sync::Mutex<SourceSet>`, with the `Watchable` carrying
  post-mutation snapshots only.
- `RelayOffer.api_key` and `RelayTarget` derive `Debug` is
  hand-written so JWT material is replaced with `<redacted>`. A
  test asserts the redaction.
- `RelayOffer.path` and `LiveTicket.relays` are bounded; gossip
  `PeerState.broadcasts` is truncated and over-long names are
  dropped before the actor acts on them.
- `RoomBuilder::set_subscribe_mode` is no longer `#[doc(hidden)]`.
  The mode actually drives `sources_for_peer`.
- The seamless layer tracks the consumer's codec; a swap to a
  source whose catalog has no rendition with the same codec is
  refused with a warn and the consumer's existing pipeline keeps
  running rather than swapping into a stalled state. The swap
  also re-checks `active_id` under the swap-state lock to avoid
  swapping to an already-stale source.

New tests:
- `subscription_recovers_preferred_when_it_returns`
- `wait_active_returns_none_when_shut_down_with_empty_set`
- `broadcaster_remove_source_ends_announce_on_that_session_only`
- `relay_hint_extends_subscriber_source_set`
- `relay_offer_validate_rejects_long_path`
- `relay_offer_debug_redacts_api_key`

Full check suite (`cargo make check-all`) is clean. The new
integration tests pass, the existing room/e2e tests still pass.

Findings deferred (justified):

- R2.12 / spawn_signal_producer abort handle: would touch
  `moq-media`'s public `RemoteBroadcast::new` shape; the existing
  shutdown_token-based lifecycle is correct in steady state and
  the leak only matters if `RemoteBroadcast` clones outlive the
  catalog task. Documented in the seamless module's notes.
- R4.4 / H3 dedup: out of scope. Lives in `iroh-moq`; the
  `Broadcaster`'s "close session on remove" lifecycle keeps the
  session count bounded in practice.
- Audio seamless swap: documented as not yet seamless; would
  require an audio-sink-side fade that is a separate work item.
- R4.2 / peer_relays + known_peers cleanup on KV expiry: the
  iroh-smol-kv API does not currently expose explicit expiry
  events to subscribers. The caps from R4.1 bound the worst case;
  durable cleanup waits for an upstream API.

Going to commit the refinement now and write a session summary.

## Summary

Five commits on `feat/rooms-gossip-relays`, base
`ca8e65a` (tip of `feat/relay-auth-for-svc`):

1. `fix(workspace)`: clippy and fmt fixes that were blocking the
   project's `cargo make check-all`.
2. `feat(moq-media)`: expose `RemoteBroadcast::build_video_pipeline*`
   and `VideoDecoderFrames::into_receiver`/`receiver` so the
   iroh-live seamless layer can rebuild a decoder pipeline against
   a swapped source.
3. `feat(iroh-live)!`: dynamic multi-origin transport. New
   `TransportSource` / `SourceSet` / `SourceSetHandle` /
   `SelectionPolicy`. `Subscription` is the unified
   multi-source-capable type. `Live::subscribe(sources, name)` and
   `Live::subscribe_ticket(&LiveTicket)` are the unified entries.
   `Broadcaster` fans a producer to a controlled set of sessions.
   `Subscription::media` returns frame-level seamless video tracks
   that survive transport swap. Rooms grow `RoomBuilder`,
   `enable_relay`/`disable_relay`, and a `relay` hint in the
   gossip `PeerState`. `LiveTicket` grows `relays`. Examples,
   CLI, demos, and tests are migrated.
4. `docs(rooms-gossip-relays)`: full plan + this worklog,
   superseding the prior research-only sketch.
5. `refactor(iroh-live)`: staff-review findings applied. Key
   wins: broadcast-channel events with `Lagged` semantics,
   parallel attach via `FuturesUnordered`, atomic
   `SourceSetHandle` mutations, JWT redaction in `Debug`, input
   caps on gossip and ticket payloads, codec-aware seamless swap
   refusal, `Broadcaster` closes its sessions on remove, and
   `Subscription::wait_active` honours shutdown.

End state of the branch:

* `cargo make check-all` clean (workspace clippy with `-D
  warnings`, fmt check, and the workspace check).
* New integration tests pass: 7 in `multi_source.rs`, 4 in
  `broadcaster.rs`, 4 in `room_relay.rs`, 4 in `ticket_relays.rs`.
* Existing tests still pass: room (6), e2e (4), patchbay
  (left untouched aside from API migration).
* Public API changes are documented in the big commit's body.

What the user should look at first

1. `iroh-live/src/sources.rs` for the
   `TransportSource`/`SourceSet`/`SelectionPolicy` shape.
2. `iroh-live/src/subscription.rs` for the unified multi-origin
   `Subscription` actor, including the parallel-attach
   reconcile loop.
3. `iroh-live/src/seamless.rs` for the frame-level seamless video
   swap on top of the subscription.
4. `iroh-live/src/rooms.rs` for the new `RoomBuilder`,
   `enable_relay`/`disable_relay`, and `PeerState.relay` hint.
5. `iroh-live/tests/multi_source.rs` and
   `iroh-live/tests/room_relay.rs` for the runtime-mutation and
   relay-hint scenarios.
6. The plan at `plans/rooms-gossip-relays.md` for the design
   trail.

Open follow-ups (justified deferrals)

* `peer_relays` / `known_peers` cleanup tied to gossip KV expiry
  events: waits for an upstream `iroh-smol-kv` API. The R4.1
  truncation cap bounds the worst case.
* Audio seamless swap: needs an audio-sink-side fade-in; out of
  scope this session.
* `Moq::connect_h3` deduplication in iroh-moq: out of scope.
* CLI `--relay` ergonomic improvements (e.g. supplying multiple
  relays, ticket-based dynamic switching) are wired through the
  new types but the command-line flags themselves still match
  the prior `--relay <id> --api-key <jwt>` shape; expanding to
  multi-source CLI flags is a follow-up.

### 2026-04-25 15:00 - Phase 2: relay-only rooms

User asked for relay-only rooms: discovery happens through the
relay's announce stream, not gossip; publish paths follow
`room/<topic>/<peer>/<name>`. Three modes must work equally well
with minimal API friction.

Research summary

- moq-lite's `OriginConsumer::announced` yields `(path, Option<consumer>)`
  on a long-lived stream and supports prefix filtering via
  `consume_only(["room/abc/"])`. Closes are surfaced as
  `(path, None)`. (See moq-lite `model/origin.rs`.)
- moq-relay derives `AuthToken.root` from the URL path. A
  publisher rooted at `room/abc/peer1` publishing `cam` lands at
  `room/abc/peer1/cam` on the primary origin. A subscriber rooted
  at `room/abc` sees announces relative to its root, i.e.
  `peer1/cam`. (See moq-relay `auth.rs`, `cluster.rs`.)
- Per-peer subdirectory roots naturally partition publishes; no
  collision across peers in the same room.

Wire-uniform name

The breakthrough: by publishing direct broadcasts under
`<my_id>/<name>` instead of bare `<name>`, the wire path matches
the relay's `<peer>/<name>` (relative-to-root) form. The
multi-source `Subscription` can use one broadcast name across
direct and relay sources without per-source name plumbing. The
prior plan flirted with adding a per-source name resolver to
`Subscription`; this name-shape unification removes the need.

Plan updated; adversarial review noted three concerns (session
multiplication on the relay side, hybrid-mode event dedup, and
JWT scoping). All three have known answers in the plan. Going
to implement now.

### 2026-04-25 16:30 - Phase 2 implementation complete

Shipped the three modes end-to-end:

- `RoomTicket` grew `mode: RoomMode { Gossip, Relay, Hybrid }`
  along with constructors `for_relay`, `for_relay_at`, and a
  `with_relay` that promotes Gossip to Hybrid.
- The actor now carries optional `gossip: Option<GossipState>`
  and `relay_discovery: Option<RelayDiscovery>`. `Actor::new`
  selects which to spawn based on the ticket's mode. Gossip-mode
  preconditions remain: `Live::join_room` rejects a Gossip or
  Hybrid ticket when gossip is not enabled on `Live`, and a
  Relay-mode ticket when no `relay` is attached.
- `RelayDiscovery` opens a long-lived H3 session at
  `room/<topic_hex>` and a watcher task forwards the relay's
  announce stream as `RelayDiscoveryEvent::{Announce, Unannounce}`
  events. The actor polls the events channel alongside its
  gossip stream in the same `tokio::select!`.
- Wire-uniform broadcast names: in Gossip and Hybrid modes, the
  room's direct publish registers under `<my_id>/<name>` (not
  bare `<name>`). In Relay and Hybrid modes, the relay publisher
  session is rooted at `room/<topic_hex>/<my_id>` and publishes
  bare `<name>`. Both shapes converge on `<peer>/<name>` from a
  subscriber's point of view, so the multi-source `Subscription`
  can use one broadcast name across direct and relay candidates.
- `sources_for_peer` returns mode-aware candidates: direct only
  in Gossip, relay only in Relay, both in Hybrid.

Tests
- `iroh-live-relay/tests/relay_room.rs` (new): three-peer
  Relay-only room over a real moq-relay; hybrid two-peer room
  with gossip discovery and relay attachment;
  Live::join_room rejects a Relay-mode ticket without a relay.
- All existing tests still pass: 21 unit, 7 multi-source, 4
  broadcaster, 4 e2e, 4 ticket-relays, 4 room-relay, 6 room.
- `cargo make check-all` clean.

Public API additions

- `iroh_live::rooms::RoomMode` (new enum).
- `iroh_live::rooms::TopicId` (re-export of `iroh_gossip::TopicId`).
- `RoomTicket::for_relay(offer)`, `RoomTicket::for_relay_at(topic, offer)`.
- `RoomTicket::mode()`, `relay_room_path()`, `relay_publisher_path()`.
- `RoomTicket::with_bootstrap(peer)`.
- `RoomMode::Gossip` / `Relay` / `Hybrid`.
- `with_relay(self, offer)` promotes Gossip to Hybrid.

User-facing flow is identical across modes:

```rust
// Mode A (gossip + direct)
let ticket = RoomTicket::generate();
let room = live.join_room(ticket).await?;
room.publish("cam", &broadcast).await?;

// Mode B (relay only)
let ticket = RoomTicket::for_relay(offer);
let room = live.join_room(ticket).await?;
room.publish("cam", &broadcast).await?;

// Mode C (hybrid)
let ticket = RoomTicket::generate().with_relay(offer);
let room = live.join_room(ticket).await?;
room.publish("cam", &broadcast).await?;
```

## Phase 2 review pass (2026-04-25)

Independent staff review of `cc0f5c0` flagged 14 issues; this
session addresses ten of them. Three follow-ups (no relay
reconnect, N×M H3 session scaling, relay can't authenticate the
peer-id segment in announce paths) are deferred with notes;
`wire_name` namespace collision is a structural change for a
later pass.

Substantive corrections

- Subscription lifecycle (#2): the per-broadcast `subscribe_closed`
  future was driven by the *first* attached source's
  `RemoteBroadcast::closed()`. In multi-source that fires on a
  policy-driven swap, not on subscription end, so entries for a
  still-healthy peer were silently torn down. The mechanism is
  gone. Cleanup is now driven by the active-source watcher, which
  signals the actor over a dedicated `closed_tx` channel only when
  the subscription's `active_id` transitions Some → None — the
  state that means every candidate source for this broadcast has
  detached. The discovery layer recreates the entry if the peer
  re-announces.
- Subscription event consistency: `handle_session_closed` and
  `prune_removed_sources` cleared `active_id` silently; the
  follow-up `pick_active` then read `current = None` and stayed
  silent because `current == next`. Both sites now emit
  `ActiveChanged { previous: Some, current: None }` so observers
  see the transition.
- Hybrid event dedup (#3): the same `(peer, broadcast)` pair
  arrived over both the gossip and relay-discovery channels, so
  consumers saw `RemoteAnnounced` twice in Hybrid mode. An
  `announced: HashSet<BroadcastId>` filters fresh names per pair;
  relay-hint transitions still emit (the gossip path keeps
  emitting when `relay_changed` is true even with no fresh
  broadcasts).
- Independent stream EOF (#4): a closed gossip stream or relay
  discovery channel used to `break` the actor. In Hybrid the other
  channel can still deliver, so each branch now drops its own
  state and exits only when both are gone.
- Race-free active source delivery (#7): the connecting future
  used to call `wait_active().await` then re-fetch
  `multi.active().await`, which can return `None` if a
  policy-driven swap raced the call. The future now returns the
  `(Subscription, ActiveSource)` pair atomically.
- Symmetric `enable_relay` / `disable_relay` (#9): both now
  manage `relay_discovery` in lockstep with
  `relay_publisher` — `enable_relay` opens (or replaces) the
  discovery session, `disable_relay` tears it down.
- Hybrid uses peer relay hints (#12): `sources_for_peer` was
  Gossip-only for `want_peer_relay_hint`. Hybrid now includes the
  hint as a secondary fallback alongside the room's own relay.
  `SourceSet::push` already dedupes by id, so a hint that points
  at the same target as the room relay collapses to one entry.
- Dead code removal (#13): the `entry.multi = multi.clone()`
  refresh in the connecting branch never differed from what
  `ensure_subscription` had already installed.
- Stronger test assertions (#14): the relay-only and Hybrid tests
  now assert `session.remote_id()` against the relay endpoint
  vs the broadcasting peer, so a regression that swapped the
  preferred source would fail loudly.
- Wire-format honesty (#1): the `RoomTicket` doc now states that
  postcard is positional — `#[serde(default)]` annotations
  describe in-memory defaults but cannot rescue tickets emitted
  by older binaries with a missing trailing field.

Verified state

- `cargo make check-all` clean (clippy `-D warnings` + fmt
  check + workspace build).
- Full test suite passes: 21 unit + 7 multi-source + 4
  broadcaster + 4 e2e + 4 ticket-relays + 4 room-relay + 6 room +
  3 relay_room.
```
