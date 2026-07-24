# Campaign prompt: upstream iroh-live's media stack to moq

> Campaign: upstream | Kind: prompt | Load `plans/prompt-base.md` first, then
> this, then read `0-overview.md` in full.

`plans/prompt-base.md` defines your role as fleet coordinator, the
survive-restart prime directive, the work cycle, the branching and worktree
model, the model economics, the document conventions, and the safety rules.
This prompt adds only what is specific to this campaign.

## Mission

Drive the plans in `plans/upstream/` until every base and leaf contribution to
moq (and moq-vaapi) is authored, reviewed, and ready for a human to open as a
pull request, and every leaf's iroh-live counterpart branch (the pair-side cut
and dependency switch) is ready to land once its moq side merges. You do not
open PRs or push; you produce paired branches, diffs, and PR text, per the
paired-branch model in `0-overview.md` and the registry in `branches.md`.

## Authorities

- `0-overview.md` is the campaign brain: the goal, the frozen base API contract
  every leaf codes against (treat it as fixed), the dependency tree, the wave
  ordering with its serializing Wave 0, the coordination points, and the git and
  PR model. Everything structural lives there; this prompt does not restate it.
- `base/`, `codec/`, `audio/`, `capture/`, and `render/` hold the self-contained
  moq-side plans, one per leaf. `counterpart/` holds the iroh-live pair-side cut
  plans, and `cut-plan.md` is the deletion ledger behind them.
- `DISPOSITION.md` is the standing register of every iroh-live media module and
  its fate; no module is dropped silently. Consult it before and after any cut.
- `comparisons/` (start at `comparisons/0-index.md`) is the evidence. The
  adversarial reviews that hardened these plans are already folded in and their
  source files retired; do not re-litigate settled findings, but re-run a
  cross-check against source when a plan looks stale.

## Done-signal per task

A moq-side task is done when its `up/<name>` branch exists in the moq worktree,
its plan's acceptance checklist is ticked against real code, its tests
(including any cfg-gated hardware round-trip the plan calls for) pass where
hardware allows, and its PR description is written. Its pair-side task is done
when the iroh-live `up/<name>` branch builds against the moq branch, the cut it
carries passes the counterpart plan's proof, and the dep is expressed as a git
branch dependency ready to be re-pinned on release. Record both in `status.md`;
opening the PR is the human's separate, explicit authorization.

## Model routing for this campaign

- Cheap tier: refining a plan from the template, verifying source anchors, nit
  sweeps, mechanical `moq_net::Timestamp` and error-type adaptation, drafting PR
  descriptions.
- Mid tier: implementing a leaf against the frozen contract, porting reference
  code into moq's shape, executing a counterpart cut whose proof test exists.
- Your tier: any change to the frozen contract, adversarial review of an
  implemented leaf, cherry-picking foundational work to `up/base`, resolving
  contradictions between leaves, and surfacing the open questions listed in the
  overview to the human.

## First-run status seed

On the first run, create `status.md` with a row per plan: the base plans (B1,
B2, B3, B5, and conditional B4), every codec, audio, capture, and render leaf,
and each leaf's counterpart task where `branches.md` lists a pair. Give each row
its wave, dependencies and coordination gates from the overview, its PR target
(moq monorepo or moq-dev/vaapi), and status not-started. Create `worklog.md` and
`learnings.md`, then follow the startup protocol and begin with Wave 0.

## Blockers that need a human

Opening any PR, pushing any branch, the moq-vaapi dependency-spine decision, the
B4 Path-B decision, and the pre-encoded-source concept buy-in (libcamera) are
human decisions; each is an open question in the overview with a current
proposal. Get each task to ready, record the blocker in `status.md` and
`worklog.md`, and continue with what is not blocked.
