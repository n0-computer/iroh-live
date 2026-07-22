# Align iroh-live to moq: shrink and improve by adopting moq primitives

> Campaign: align-to-moq | Kind: overview | Read this first, then the task plans
> under `tasks/`. The campaign prompt is `prompt.md`; the shared evidence is
> `../upstream/comparisons/` (start at `0-index.md`).

This campaign is about iroh-live, not moq. Its goal is to make iroh-live smaller
and better by adopting moq's primitives where moq now covers what we built by
hand, and to rebuild the room layer on moq's origin, announce, and token
primitives. It is the counterpart to the upstream campaign in `plans/upstream/`:
that campaign moves our codec, capture, audio-device, and render code into moq
and carries, on each contribution's paired iroh-live branch, the deletion of the
local code the contribution replaces. What remains here is the work that needs
no upstream contribution at all: pure alignment of iroh-live to what moq already
ships. moq is a single codebase (`/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`).

## The two campaigns and their split

- `plans/upstream/` contributes iroh-live's media stack into moq as paired
  branches: a moq branch that adds, an iroh-live branch that depends on it and
  cuts. All codec, capture, render, and audio-device deletions live there, in
  `../upstream/counterpart/`, governed by the deletion ledger
  `../upstream/cut-plan.md` and the module register
  `../upstream/DISPOSITION.md`.
- `plans/align-to-moq/` (this campaign) adopts what moq's current releases
  already provide: delegating the iroh-moq transport to moq-native, adopting the
  moq-mux catalog and ordered-consumer primitives, rebuilding rooms on moq
  announce, and aligning the sync and adaptive layers to moq-mux's per-rendition
  estimation. Every task here is independent of the upstream campaign and can
  start once the pins are bumped.

The dependency runs one way: the upstream campaign's counterpart branches build
on the alignments made here (a bumped pin, an adopted catalog), never the
reverse.

## Anchor documents

- `cut-plan.md`: the alignment deletion ledger. Per-module verdicts, staging
  (stages A0 to A2), and risks for everything this campaign deletes or
  reshapes: transport, pubsub and pipeline wiring, adaptive, sync, and rooms.
  It stops at the media boundary; the media ledger is
  `../upstream/cut-plan.md`.
- `room-layer.md`: the room-layer redesign onto moq's origin,
  `announced(prefix)`, and token path-scoping: rooms move from gossip plus
  signed KV to moq announce, with gossip retained for bootstrap and moq-token
  path-scoping providing cryptographic announce-under-your-own-id. Phased,
  with the tradeoffs stated.

## Task tree

Each task is a self-contained unit a small agent can execute from its plan in
`tasks/`. All are independent of upstream work.

```
iroh-live (41,564 LOC core)
+-- Wave 0
|   +-- pin-bump             bump moq-net/moq-native/hang pins to the merged native stack
+-- Wave 1 (independent alignments)
|   +-- transport-delegate   iroh-moq handshake -> moq-native::iroh + full ALPN list
|   +-- catalog-adopt        hand-rolled catalog -> moq-mux catalog::Producer + CatalogExt
|   +-- pubsub-align         group ordering/latency -> moq-mux ordered consumer
|   +-- sync-adaptive-align  read moq-mux per-rendition Estimate{jitter,bitrate}
+-- Wave 2 (room redesign, parallel to Wave 1)
    +-- rooms-announce       gossip+KV discovery -> moq announce + token path-scoping
```

Branch names are `align/<task>`; the registry is `../branches.md`.

## Wave ordering

- Wave 0, the pin bump. iroh-live pins the older `moq-net 0.1.11` /
  `moq-native 0.17.1` / `hang 0.19.1` line. Bump to the moq release that carries
  the merged native stack. This unblocks every other task.
- Wave 1, the independent alignments: transport-delegate, catalog-adopt,
  pubsub-align, sync-adaptive-align. Self-contained refactors against the bumped
  moq.
- Wave 2, the room redesign: rooms-announce, in the phases of `room-layer.md`.
  Independent of Wave 1 and may run in parallel with it.

## Coordination points

1. Adoption is proven before anything is removed: an example or end-to-end test
   passes on the new path before the old wiring is deleted. This is the hard
   rule for every task that replaces working code.
2. Rooms security. The announce redesign relies on moq-token path-scoping to
   enforce announce-under-your-own-id. Open question: the multi-room scoping
   model, discussed in `room-layer.md` phase 2, with the current proposal stated
   there; settle it before that phase's implementation.
3. Handoff to upstream counterparts. When an upstream pair-side branch needs an
   alignment from this campaign (for example the bumped pin), it builds on the
   merged align branch; coordinate through `../branches.md` rather than
   duplicating work.

## How a task is executed

Each task plan in `tasks/` is written so a capable but non-expert agent can
execute it end to end: the goal, the iroh-live code it changes with `file:line`,
the moq primitive it adopts, the ordered steps, the proof test, and the
coordination points it touches. An agent reads this overview, then its task
plan, then the referenced comparison sections, and works on its `align/<task>`
branch in its own worktree.

## Task plan template

```
# <task-name>

> Campaign: align-to-moq | Kind: task plan | Branch: align/<task-name> |
> Wave: 0 | 1 | 2 | Read ../0-overview.md first.
Depends on: <pin bump / other tasks>

## Goal
One paragraph: what iroh-live gains and what it replaces.

## Evidence
Links into ../upstream/comparisons/ and room-layer.md for the verdict.

## moq primitive adopted
The moq type/API this replaces our code with (with file:line in moq).

## iroh-live code changed
The modules and file:line this edits or deletes, with current LOC.

## Steps
Ordered, each small enough to commit, adoption before deletion.

## Proof
The example or end-to-end test that must pass on the new path before old wiring
is removed (coordination point 1).

## Coordination
Any point this task must defer on.

## Acceptance checklist
The gate for calling the task done.
```

## Status

`room-layer.md` and the per-task plans under `tasks/` are in place, one per node
of the task tree above. Read a task plan for what to build; read `room-layer.md`
and the comparisons for the reasoning behind it.
