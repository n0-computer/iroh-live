# Campaign prompt: align iroh-live to moq

Load `plans/prompt-base.md` first; it defines your role as fleet coordinator, the
survive-restart-from-empty prime directive, the work cycle, the model economics,
the oversight rules, and the safety and authorization rules. This prompt
specializes the base for the align-to-moq campaign. Follow both.

## Mission

Make iroh-live smaller and better by adopting moq's primitives, deleting the owned
code that adoption replaces, and rebuilding the room layer on moq's origin,
announce, and token primitives. Unlike the upstream campaign, this work lands in
the iroh-live repository, on iroh-live branches, as normal commits. The output is a
shrunk, moq-aligned iroh-live, not pull requests to an external project.

## The authorities you and every worker code against

- `plans/align-to-moq/0-overview.md` is the campaign brain: the two-campaign framing
  and the one-way dependency on the upstream campaign, the task tree split into
  independent and upstream-gated tasks, the wave ordering, the coordination points,
  and the task-plan template. Read it in full on every start.
- `plans/align-to-moq/tasks/` holds the self-contained task plans, one per node of
  the tree. `plans/align-to-moq/cut-plan.md` is the deletion ledger (what is deleted,
  when, and why) and `plans/align-to-moq/room-layer.md` is the room redesign; these
  are the reasoning behind the tasks. `../upstream/comparisons/` (start at
  `0-index.md`) is the shared evidence.

## Structural rules specific to this campaign

- Adoption before deletion, always. Nothing is deleted until its moq replacement is
  proven in iroh-live: the task's proof test passes on the new path first
  (coordination point 1). A worker that cannot make the proof pass does not delete
  the old module; it stops and reports.
- No zero-copy regression. A removal that would drop a decode-to-render or
  capture-to-encode zero-copy path waits for the render-adopt task and the upstream
  frame vocabulary, so the zero-copy paths survive the transition (coordination point
  2). render-adopt lands before or with codec-remove's decode deletions.
- Upstream gating. The upstream-gated tasks (codec-remove, capture-remove,
  render-adopt) delete iroh-live code only after the matching upstream contribution
  has landed in moq and released. Each task names the exact contribution and release
  it waits on; do not start a gated removal before its replacement is in a moq
  release iroh-live can pin. The independent tasks (transport-delegate, catalog-adopt,
  pubsub-align, rooms-announce, sync-adaptive-align) have no such gate and are where
  you start.
- The pin bump is Wave 0. iroh-live pins an older moq line; bumping to the release
  that carries the merged native stack unblocks every independent task and is the
  prerequisite for all of them.
- Rooms security. The announce redesign relies on moq-token path-scoping; the
  multi-room scoping question in `room-layer.md` phase 2 must be settled before that
  phase's implementation (coordination point 5).
- The bridge period. While a platform is partly migrated, the repository runs a mixed
  stack; the cut-plan's atomic-per-platform recommendation governs the order so it
  never holds two frame models within one platform at once (coordination point 4).

## Done-signal per task

A task is done when its adoption is committed on the iroh-live branch, its
proof-before-deletion test passes on the new path, the old module it replaces is
deleted (for the removal tasks) or the alignment is wired and verified (for the
adoption and rooms tasks), the check suite passes, and the plan's acceptance
checklist is ticked against real code. Record the branch and the passing test as the
done-signal in `status.md`. Commits on the campaign branch are within scope; a push
is not, absent an explicit human ask.

## Model routing for this campaign

- Cheap tier: writing or refining a task plan, verifying anchors, mechanical
  edits, and a nit sweep.
- Mid tier: implementing an adoption or a removal against a task plan (real Rust
  refactoring in iroh-live, with the proof test), and the room-layer phases that are
  mechanical wiring.
- Your tier: the room-redesign design decisions the overview and `room-layer.md`
  leave open (the multi-room scoping question), adversarial review of a removal that
  touches a zero-copy path, and deciding when an upstream-gated task's prerequisite
  has genuinely released.

## First-run status seed

On the first run, create `status.md` with a row per task: the pin bump (Wave 0), the
five independent tasks (transport-delegate, catalog-adopt, pubsub-align,
rooms-announce, sync-adaptive-align), and the three upstream-gated tasks
(codec-remove, capture-remove, render-adopt), each with its wave, its dependencies
and gates, its upstream prerequisite where it has one, and status not-started.
Create `worklog.md` and `learnings.md`. Then follow the startup protocol and begin
with the pin bump and the independent Wave 1 and Wave 2 tasks.

## Blockers that need a human or the other campaign

Pushing any branch, the multi-room scope design question, and every upstream-gated
task's prerequisite (which depends on the upstream campaign reaching a moq release)
are outside your unilateral control. Get each independent task done, get each gated
task to the edge of its gate, record the blocker in `status.md` and `worklog.md`, and
continue with what is not blocked.
