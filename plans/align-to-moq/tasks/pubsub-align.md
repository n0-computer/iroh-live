# pubsub-align

Branch: align/pubsub-align          Wave: 1
Depends on: Wave 0 pin bump (the moq release carrying `moq_mux::container::Consumer::set_latency` and `discontinuity()`)
Kind: independent

## Goal

Earlier notes claimed iroh-live still reimplemented group ordering and latency
skipping through a hang `WatchTrack` plus `TrackConsumer::set_max_latency`. The
round-2 comparison confirmed against current source that this is stale: that was
the hang-0.10 model, the code has neither, and group ordering plus
latency-bounded skipping are already delegated entirely to
`moq_mux::container::Consumer` (aliased as `OrderedConsumer`), configured at
construction via `with_latency`. So there is no ordering or skip machinery to
retire; that reimplementation is already gone. This task is therefore narrowly
scoped: adopt the two mid-stream consumer capabilities iroh-live does not yet use
but now needs for the adaptive and latency-policy paths, `set_latency` and
`discontinuity()`, and respect the `#2426` empty-batch read contract. It also
records precisely what is already aligned so no work is invented against code that
is correct.

## What is already aligned (do not touch, do not re-implement)

- Group ordering, cross-group advance, and latency-bounded skipping are delegated
  to `moq_mux::container::Consumer` (`../upstream/comparisons/pubsub.md`
  section 3, "our `MoqPacketSource` ... just adapts `read()`"; capability matrix
  section 9, "Group ordering + latency skip: delegated to
  `moq_mux::container::Consumer`"). Verified in source:
  `moq-media/src/lib.rs:31` aliases
  `OrderedConsumer = moq_mux::container::Consumer<moq_mux::catalog::hang::Container>`,
  and every subscription entry point builds it with `.with_latency(max_latency)`
  (`subscribe.rs:559-560`, `:597-598`, `:751-752`, `:779-780`, and the adaptive
  switch at `:1519-1520`). `MoqPacketSource` (`transport.rs:34-83`) only adapts
  `read()` into a `MediaPacket` and already completes on `Ok(None)`
  (`transport.rs:65-67`).
- The stale `WatchTrack`/`set_max_latency` model is absent from the tree
  (pubsub.md section 3, "A note on stale terminology"). There is nothing to
  delete for it.
- The subscribe-layer additions with no upstream counterpart stay unchanged:
  quality-based rendition selection over the catalog, the latest-wins
  `frame_channel`, playout-clock gating, decoder hot-swap, and the
  `adaptation_task_v2` driver (`subscribe.rs:1293`) with `switch_rendition_v2`
  (`subscribe.rs:1497`).

## Evidence

- `../upstream/comparisons/pubsub.md` section 3 (subscribe path, the stale-model
  correction, the `set_latency`/`discontinuity()` gap, and the `#2426`
  empty-batch contract), section 9 (capability matrix rows "Mid-stream latency
  change: no; resubscribe required" and "Discontinuity signal: no; inferred via
  decode error + reset"), and section 10 (subscribe verdict: "keep. Ordering and
  skipping are already delegated ... Enablers to adopt on the pin bump:
  `set_latency` ..., `discontinuity()` ...").
- `../cut-plan.md` section 2, moq-media table, `subscribe.rs` row: 1,566 LOC,
  verdict **keep (merge at the edges)**, "quality selection, hot-swap, and
  adaptation driver have no upstream counterpart," prerequisite "release for
  `set_latency`, `discontinuity()`."
- moq-side anchors: `moq_mux::container::Consumer::with_latency`
  (`rs/moq-mux/src/container/consumer.rs:148`), `set_latency`
  (`consumer.rs:479`), `discontinuity()` (`consumer.rs:161`), the ordering
  contract (`consumer.rs:15-37`), the latency skip test
  (`max_timestamp.saturating_sub(oldest) >= self.latency`, `consumer.rs:302`).

## moq primitive adopted

`moq_mux::container::Consumer`'s mid-stream controls, on the merged main pending
the bump:

- `Consumer::set_latency(&mut self, Duration)` (`consumer.rs:479`): retune the
  skip threshold on a live consumer without tearing down the subscription. Today
  changing `PlaybackPolicy::max_latency` requires resubscribing every track
  (`playout.rs` doc at `:36-41`), and the planned `PlayoutMode::Auto { min, max }`
  from phase-3b wants to tune the threshold continuously against measured jitter.
  Adaptive rendition switching does *not* need this (every switch builds a fresh
  consumer, `subscribe.rs:1519-1520`); the latency policy does. This is the
  precise scoping the comparison draws (pubsub.md section 3, "On `set_latency`
  specifically").
- `Consumer::discontinuity() -> u64` (`consumer.rs:161`): a counter that lets the
  decode loop flush decoder and render buffers on timeline rewinds. Today
  `decode_loop` discovers discontinuities only through decode errors followed by
  `reset()` and wait-for-keyframe (`subscribe.rs`/`pipeline/video_decode.rs:331-336`,
  pubsub.md section 3). Reading `discontinuity()` gives a clean, explicit flush
  signal instead of inferring it from a decode failure.

## iroh-live code changed

- `moq-media/src/playout.rs` (92 LOC) and its `PlaybackPolicy`
  (`playout.rs:42-54`): add a path to apply a new `max_latency` to live consumers
  via `set_latency` rather than requiring resubscription; update the stale doc at
  `playout.rs:36-41` that documents the resubscribe requirement. This is the
  enabler for `PlayoutMode::Auto { min, max }` (phase-3b), so wire it so the sync
  layer can retune continuously.
- `moq-media/src/subscribe.rs`: hold the `OrderedConsumer` handle where a running
  track can receive a `set_latency` call (the consumers are built at
  `:559-560`, `:597-598`, `:751-752`, `:779-780`). No change to how ordering or
  skipping works; only the ability to retune the already-delegated threshold.
- `moq-media/src/pipeline/video_decode.rs` `decode_loop` (`:213-409`): read
  `Consumer::discontinuity()` and flush/`reset()` on an increment, replacing the
  decode-error-driven discovery at `:331-336`. Keep the keyframe-wait as the
  recovery step; the change is the trigger, not the recovery.
- `moq-media/src/transport.rs` `MoqPacketSource::read` (`:49-75`): verify it keys
  group completion off `Ok(None)` and never off an empty batch, per the `#2426`
  contract (pubsub.md section 3, "a correctness trap for our `MoqPacketSource`").
  Current source already returns on `Ok(None)` (`:65-67`) and treats
  `Ok(Some(frame))` as a real frame, so this is a guard-and-document step, not a
  rewrite; add a comment pinning the contract so it is not regressed.
- Fix the stale `adaptation_task` doc comment (`subscribe.rs:1281-1287`,
  pubsub.md section 10 subscribe verdict), riding a code commit that touches the
  same area (cut-plan section 6, no standalone doc commits).

No module is deleted by this task: the reimplementation that a deletion would
target is already absent. The net change is the adoption of two consumer methods,
one contract guard, and doc corrections.

## Steps

1. Confirm the pin bump (Wave 0) landed so `set_latency` and `discontinuity()`
   are on the pinned moq-mux; re-diff `container/consumer.rs` against the release
   (cut-plan risk R-b: signatures may drift from `3a3e0ea8`).
2. Adopt `discontinuity()` in `decode_loop`: read the counter each iteration,
   flush and `reset()` on increment, keep the keyframe-wait recovery. Verify a
   timeline-rewind flushes cleanly instead of surfacing as a decode error.
   `feat:` commit.
3. Adopt `set_latency` for `PlaybackPolicy`: thread the live consumer handle so a
   policy change retunes the threshold in place; update the `playout.rs` doc.
   `feat:`/`refactor:` commit.
4. Guard the `#2426` empty-batch contract in `MoqPacketSource` with a pinning
   comment and, if not already covered, a unit test.
5. Correct the stale `adaptation_task` doc comment as part of one of the above
   commits.

## Proof before deletion

There is nothing to delete, so the P1 gate applies to the behavior changes
instead: `moq-media/tests/pipeline_integration.rs` and `iroh-live/tests/e2e.rs`
must pass with `set_latency` and `discontinuity()` adopted. Add a targeted test
that changes `PlaybackPolicy::max_latency` on a live subscription and asserts the
skip threshold retunes without a resubscribe, and a test that drives a timeline
discontinuity and asserts a clean decoder flush rather than an error-path
recovery. Note the risk-register gap (cut-plan R-g): there is no adaptive-switching
integration test driving `adaptation_task_v2` end to end today, so the
`set_latency` policy test is the first coverage of live-latency retuning and
should be checked in with the change.

## Coordination

- No zero-copy path is touched (coordination point 2 does not apply).
- `discontinuity()` adoption borders the decode-pipeline internals that swap onto
  the sans-IO `moq_video::decode::Decoder` under the upstream-gated codec adoption
  (cut-plan stage 2/4). Keep this task to the consumer-level signal so it stays
  independent of the codec swap; the flush trigger is orthogonal to which decoder
  runs.
- `set_latency` is the enabler for the phase-3b `PlayoutMode::Auto { min, max }`
  jitter/sync work (`sync-adaptive-align`); land the plumbing here so that task can
  consume it, but do not pull the jitter estimation into this task.

## Acceptance checklist

- [ ] The document records, and the branch preserves, that group ordering and
      latency skipping are already delegated to `moq_mux::container::Consumer`;
      no ordering or skip reimplementation is introduced or was found to delete.
- [ ] `Consumer::set_latency` is adopted so `PlaybackPolicy::max_latency` retunes
      live consumers without resubscription; the stale `playout.rs:36-41` doc is
      corrected.
- [ ] `Consumer::discontinuity()` drives decoder flushes in `decode_loop`,
      replacing decode-error-driven discovery.
- [ ] `MoqPacketSource` provably completes on `Ok(None)` per the `#2426`
      empty-batch contract, guarded by comment and test.
- [ ] The stale `adaptation_task` doc comment (`subscribe.rs:1281-1287`) is fixed.
- [ ] `pipeline_integration.rs` and `e2e.rs` pass; `cargo make check-all` is green.
