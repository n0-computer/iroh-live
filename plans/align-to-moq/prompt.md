# Campaign prompt: align iroh-live to moq

> Campaign: align-to-moq | Kind: prompt | Load `plans/prompt-base.md` first,
> then this, then read `0-overview.md` in full.

`plans/prompt-base.md` defines your role as fleet coordinator, the
survive-restart prime directive, the work cycle, the branching and worktree
model, the model economics, the document conventions, and the safety rules.
This prompt adds only what is specific to this campaign.

## Mission

Make iroh-live smaller and better by adopting moq's primitives where moq already
covers what we built by hand, and rebuild the room layer on moq's origin,
announce, and token primitives. This work lands in the iroh-live repository on
`align/<task>` branches as normal commits; there is no external PR and no moq
pair. Deleting codec, capture, and render code is not this campaign: those cuts
ride the upstream campaign's paired counterpart branches
(`../upstream/counterpart/`).

## Authorities

- `0-overview.md` is the campaign brain: the two-campaign framing, the task
  tree, the wave ordering, the coordination points, and the task template.
  Everything structural lives there; this prompt does not restate it.
- `tasks/` holds the self-contained task plans, one per node of the tree.
  `cut-plan.md` is this campaign's deletion ledger (transport, pubsub, rooms,
  adaptive, sync; stages A0 to A2) and `room-layer.md` is the room redesign the
  rooms task executes. `../upstream/comparisons/` (start at `0-index.md`) is
  the shared evidence; the media ledger and module register are
  `../upstream/cut-plan.md` and `../upstream/DISPOSITION.md`.

## Done-signal per task

A task is done when its adoption is committed on its `align/<task>` branch, its
proof test passes on the new path, the check suite passes, and the plan's
acceptance checklist is ticked against real code. Record the branch and passing
test in `status.md`. Commits on campaign branches are in scope; pushing is not,
absent an explicit human ask.

## Model routing for this campaign

- Cheap tier: refining a task plan, verifying anchors, mechanical edits, nit
  sweeps.
- Mid tier: implementing an adoption against a task plan (real Rust refactoring
  with the proof test), and room-layer phases that are mechanical wiring.
- Your tier: the room-redesign decisions the overview leaves open (the
  multi-room scoping question), adversarial review of anything touching a
  zero-copy or sync-critical path, and judging when a moq release genuinely
  carries what a task waits on.

## First-run status seed

On the first run, create `status.md` with a row per task: the pin bump (Wave 0)
and the five independent tasks (transport-delegate, catalog-adopt, pubsub-align,
rooms-announce, sync-adaptive-align), each with its wave, dependencies, and
status not-started. Create `worklog.md` and `learnings.md`, then follow the
startup protocol and begin with the pin bump.

## Blockers that need a human

Pushing any branch and the multi-room scope design question are outside your
unilateral control. Record any blocker in `status.md` and `worklog.md` and
continue with what is not blocked.
