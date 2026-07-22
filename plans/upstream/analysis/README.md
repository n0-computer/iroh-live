# analysis: broader refactor context

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
- `iroh-live-cut-plan.md`: what iroh-live deletes once moq carries the upstreamed
  code. This is the counterpart to the upstream campaign: the campaign moves code
  into moq, this plan removes the local copies once each contribution lands and
  releases.
- `room-layer.md`: the room-layer redesign onto moq's origin, announce, and token
  primitives. A separate effort from this campaign, preserved for when it starts.
- `refactor-upstream-plan.md`: the earlier PR-program write-up. Superseded as the
  actionable plan by `../0-overview.md` plus the `../base`, `../codec`,
  `../capture`, and `../render` plans, but kept for its engagement strategy, the
  velocity gate, and the per-contribution size catalog.

For the codec and capture upstream campaign itself, start at `../0-overview.md`
and `../comparisons/0-index.md`. The full history of the refactor analysis rounds,
including the superseded review reports and the stale pre-merge maps, remains in
git history under the retired `plans/refactor/` path.
