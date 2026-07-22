# Align iroh-live to moq: shrink and improve by adopting moq primitives

This campaign is about iroh-live, not moq. Its goal is to make iroh-live smaller
and better by adopting moq's primitives where moq now covers what we built by
hand, deleting the owned code that adoption replaces, and rebuilding the room
layer on moq's origin, announce, and token primitives. It is the counterpart to
the upstream campaign in `plans/upstream/`: that campaign moves our codec and
capture code into moq; this one removes code from iroh-live once moq carries an
equivalent, and aligns the pieces that stay to moq's shapes.

Read this overview first. It frames the two campaigns and their dependency, the
task tree with what is doable now versus what waits on upstream, the wave
ordering, the coordination points, and where the detailed plans live. moq is a
single codebase (`/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`).

## The two campaigns and their dependency

- `plans/upstream/` contributes iroh-live's codec and capture work into moq
  (VAAPI, V4L2, Android, AV1, the frame vocabulary, the renderer crate, and so
  on) as pull requests to moq and moq-vaapi.
- `plans/align-to-moq/` (this campaign) removes and realigns iroh-live code by
  adopting moq. It splits into two kinds of task:
  - Independent tasks that need no upstream work and can start now against the
    moq release iroh-live already tracks, once the pins are bumped: delegating
    the iroh-moq transport to moq-native, adopting the moq-mux catalog and
    ordered-consumer primitives, rebuilding rooms on moq announce, and aligning
    the sync and adaptive layers to moq-mux's per-rendition estimation.
  - Upstream-gated tasks that delete iroh-live code only after the matching
    upstream contribution lands in moq and releases: removing `rusty-codecs` and
    `rusty-capture` module by module, and adopting the out-of-tree
    `moq-video-render` crate in place of iroh-live's `render/`.

The dependency runs one way: the upstream-gated removals here consume what the
upstream campaign produces. The independent tasks do not, and are the natural
place to start because they shrink iroh-live without waiting on any moq release.

## Scope and the two anchor documents

Two documents carry the analysis this campaign executes, both grounded in the
comparisons under `../upstream/comparisons/` (start at
`../upstream/comparisons/0-index.md`):

- `cut-plan.md`: the deletion ledger. Every iroh-live module with a verdict (cut,
  cut-after-upstream, keep, keep-and-upstream-copy, or merge), the moq replacement,
  the prerequisite, and the staged order. It models two scenarios: adopt current
  moq main as-is (about 4,800 LOC removed, one version bump) and adopt plus land
  the upstream contributions (about 17,400 LOC, 42% of the 41,564-LOC core). This
  is the source of truth for what gets deleted and when.
- `room-layer.md`: the room-layer redesign onto moq's origin, `announced(prefix)`,
  and token path-scoping. Rooms move from gossip plus signed KV to moq announce,
  with gossip retained for bootstrap, and moq-token path-scoping providing
  cryptographic announce-under-your-own-id. Phased, with the honest tradeoffs.

Out of scope: contributing code to moq (that is `plans/upstream/`), and the audio
device layer (the AEC engine, the playback sink, the symphonia file source),
which stays local and is a separate future effort.

## Task tree

Each task is a self-contained unit a small agent can take over from its plan in
`tasks/`. The tree groups them by dependency.

```
iroh-live (41,564 LOC core)
|
+-- Independent tasks (no upstream dependency; start now)
|   +-- transport-delegate   iroh-moq handshake -> moq-native::iroh + full ALPN list
|   +-- catalog-adopt        hand-rolled catalog -> moq-mux catalog::Producer + CatalogExt
|   +-- pubsub-align         group ordering/latency -> moq-mux ordered consumer
|   +-- rooms-announce       gossip+KV discovery -> moq announce + token path-scoping
|   +-- sync-adaptive-align  read moq-mux per-rendition Estimate{jitter,bitrate}
|
+-- Upstream-gated tasks (delete only after the matching upstream PR releases)
    +-- codec-remove         delete rusty-codecs modules as moq gains each backend
    +-- capture-remove       delete rusty-capture backends as moq gains each source
    +-- render-adopt         adopt moq-video-render crate, delete iroh-live render/
```

The independent tasks are the campaign's near-term value: they shrink iroh-live
and align it to moq without any moq release beyond the pin bump. The
upstream-gated tasks are the large LOC removals, and each keys on a specific
upstream contribution reaching a moq release.

## Wave ordering

- Wave 0, the pin bump. iroh-live currently pins the older `moq-net 0.1.11` /
  `moq-native 0.17.1` / `hang 0.19.1` line. Bump to the moq release that carries
  the merged native stack. This unblocks every independent task and is the
  prerequisite for all of them.
- Wave 1, the independent alignments. transport-delegate, catalog-adopt,
  pubsub-align, and sync-adaptive-align. These are self-contained refactors of
  iroh-live against the bumped moq.
- Wave 2, the room redesign. rooms-announce, in the phases of `room-layer.md`.
  It is independent of the codec work and can run in parallel with Wave 1.
- Wave 3, the upstream-gated removals. codec-remove, capture-remove, and
  render-adopt, each gated on its upstream contribution releasing, and each
  following the cut-plan's principle that nothing is deleted until its
  replacement passes an end-to-end test on the new path.

## Coordination points

1. Nothing is cut until its replacement is proven in iroh-live: an example or an
   end-to-end test passes on the new path before the old module is deleted
   (cut-plan principle P1). This is the hard rule for every removal.
2. No cut regresses a zero-copy path. A removal that would drop a zero-copy path
   waits for the render-adopt task and the upstream frame vocabulary, so the
   decode-to-render and capture-to-encode paths survive the transition
   (cut-plan principle P2).
3. Upstream gating. Each upstream-gated task names the exact upstream
   contribution and moq release it waits on. Do not start a removal before its
   replacement is in a release iroh-live can pin.
4. The bridge period. While a platform is partly migrated, iroh-live runs a mixed
   stack; the cut-plan's atomic-per-platform recommendation governs the order so
   the repository never holds two frame models within one platform at once.
5. Rooms security. The announce redesign relies on moq-token path-scoping to
   enforce announce-under-your-own-id; the multi-room scoping question in
   `room-layer.md` phase 2 must be settled before that phase's implementation.

## How a task is executed

Each task plan in `tasks/` is written so a capable but non-expert agent can
execute it end to end: the goal, the iroh-live code it changes with `file:line`,
the moq primitive it adopts, the ordered steps, the test that must pass before any
deletion, and the coordination points it touches. An agent reads this overview,
then its task plan, then the referenced comparison sections under
`../upstream/comparisons/`, and works on a branch. It does not delete a module
until the task's proof test passes on the new path.

## Status

The two anchor documents (`cut-plan.md`, `room-layer.md`) are in place. The
per-task plans under `tasks/` are the next organization step, breaking the cut
ledger and the room phases into the self-contained units the tree above names.
