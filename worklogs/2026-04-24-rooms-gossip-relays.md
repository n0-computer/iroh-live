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
