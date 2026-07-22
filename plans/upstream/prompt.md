# Campaign prompt: upstream iroh-live's codec and capture work to moq

Load `plans/prompt-base.md` first; it defines your role as fleet coordinator, the
survive-restart-from-empty prime directive, the work cycle, the model economics,
the oversight rules, and the safety and authorization rules. This prompt
specializes the base for the upstream campaign. Follow both.

## Mission

Drive the plans in `plans/upstream/` to the point where every base and leaf pull
request against moq (and moq-vaapi) is authored, reviewed, and ready for a human to
open. You do not open the PRs; you produce the branches, the diffs, and the PR
descriptions, and you get each to a ready state.

## The authorities you and every worker code against

- `plans/upstream/0-overview.md` is the campaign brain. Its "frozen base API
  contract" is the exact API every leaf codes against; treat it as fixed. Its
  dependency tree, wave ordering, and eleven coordination points govern what may be
  dispatched when. Read it in full on every start.
- `plans/upstream/base/`, `codec/`, `capture/`, and `render/` hold the self-contained
  plans, one per node of the tree. `plans/upstream/comparisons/` (start at
  `0-index.md`) is the evidence.
- `plans/upstream-review-0722-plan.md` and the `plans/upstream/review-0722-*.md`
  files are the standing adversarial review; its findings are already folded into
  the plans. Do not re-litigate them; do re-run its cross-checks against source when
  a plan looks stale.

## Structural rules specific to this campaign

- The base merges first. A pull request to upstream moq targets moq `main`, not a
  fork branch, so the base API (B1, B2, B3, B5) must merge on moq main before any
  leaf PR can be reviewed in isolation. Wave 0 is a hard serializing gate. Locally
  you may keep a base integration branch so leaves compile against the proposed API,
  but that is a development convenience, not the PR path. B4 is a later, conditional,
  breaking PR, only on the Path B (Android) decision.
- Two PR targets. Most leaves target the moq monorepo. The VAAPI VA-layer work
  targets the separate external `moq-dev/vaapi` repo; the moq-video backend wiring is
  a separate monorepo PR (coordination point 11). Track the target per task.
- Coordination hazards to enforce: the shared candidate tables in
  `encode/backend/mod.rs` and `decode/backend/mod.rs` (each leaf adds only its own
  additive row; serialize and rebase), the shared moq-vaapi crate (vaapi-decode owns
  the decode-stack contribution, vaapi-encode the encode validation, coordinated on
  moq-vaapi's types), the rav1d fork prerequisite (av1 blocks until it resolves), the
  pre-encoded-source concept buy-in (libcamera), and the licensing, CI-hardware-gating,
  and semver concerns (coordination points 8 through 10).

## Done-signal per task

A task is done when its branch exists, its plan's acceptance checklist is fully
ticked against real code, its tests (including the cfg-gated hardware round-trip
where the plan calls for one) are written and pass where the hardware allows, and
its PR description is written, so the only remaining step is a human opening the PR.
Record that ready state in `status.md`; opening the PR is the human's explicit,
separate authorization. Because the base merges first, a leaf's done-signal is
"ready against the merged base or the local base integration branch", and it flips
to "PR-ready against main" once the base has actually merged upstream.

## Model routing for this campaign

- Cheap tier: writing or refining a leaf plan from the template, verifying source
  anchors, nit sweeps, mechanical `moq_net::Timestamp` and error-type adaptation,
  and drafting PR descriptions.
- Mid tier: implementing a leaf backend against the frozen contract (real Rust with
  care, but the plan is explicit) and porting our reference code into moq's shape.
- Your tier: the base API design if the contract needs a change, adversarial review
  of an implemented PR, resolving a contradiction between two leaves, and the
  moq-vaapi dependency-spine and B4 decisions (which are maintainer conversations you
  surface, not decide alone).

## First-run status seed

On the first run, create `status.md` with a row per plan: the five base plans (B1,
B2, B3, B4, B5), the ten codec leaves, the three capture leaves, and the render
leaf, each with its wave, its dependencies and coordination gates from the overview,
its PR target, and status not-started. Create `worklog.md` and `learnings.md`. Then
follow the startup protocol and begin with Wave 0.

## Blockers that need a human

Opening any PR, pushing any branch, the moq-vaapi dependency-spine decision, the B4
Path-B decision, the rav1d fork resolution, and the pre-encoded-source concept
buy-in are human decisions. Get each task to ready, record the blocker in
`status.md` and `worklog.md`, and continue with what is not blocked.
