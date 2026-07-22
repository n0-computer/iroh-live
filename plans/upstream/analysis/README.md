# analysis: broader refactor context

> Campaign: upstream | Kind: analysis | Preserved context; superseded by
> ../0-overview.md where they disagree. Read ../0-overview.md first.

These documents are the wider iroh-live-to-moq refactor analysis that the codec
and capture upstream campaign was drawn from. They reach beyond this campaign's
scope (which is upstreaming codec and capture code) into the pub/sub layer, the
room layer, and the iroh-live-side deletions, so they live here as context rather
than as campaign plans. They were produced in the refactor analysis rounds and
moved here on 2026-07-22 when the `plans/refactor/` directory was retired; the
codec and capture evidence from that analysis lives in `../comparisons/`.

- `refactor-overview.md`: the whole-refactor framing and document index, including
  the moq main topology and the prior-work incorporated. Superseded for campaign
  structure by `../0-overview.md`; kept for the broader framing.
- `refactor-summary.md`: the executive summary of the whole refactor (codec,
  capture, pub/sub, rooms, and the iroh-live cuts), with the scenario expectations
  and the priced alternatives.
- `refactor-upstream-plan.md`: the earlier PR-program write-up. Superseded as the
  actionable plan by `../0-overview.md` plus the `../base`, `../codec`,
  `../capture`, and `../render` plans, but kept for its engagement strategy, the
  velocity gate, and the per-contribution size catalog.

The iroh-live-side alignment work (the room-layer redesign and the iroh-live cut
plan, which are about shrinking and improving iroh-live rather than contributing
to moq) moved to `plans/align-to-moq/` on 2026-07-22; see that directory's
overview.

For the codec and capture upstream campaign itself, start at `../0-overview.md`
and `../comparisons/0-index.md`. The full history of the refactor analysis rounds,
including the superseded review reports and the stale pre-merge maps, remains in
git history under the retired `plans/refactor/` path.
