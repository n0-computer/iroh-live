# Round-2 review: fix-wave verification

Reviewed 2026-07-18 against review-accuracy.md, review-design.md, and
review-prose.md, plus the source tree where the claims are checkable
(test counts re-run against `iroh-live/tests/`). Scope: the twelve
numbered docs the fix wave touched. No fixes applied here.

## Resolutions verified

Roughly 42 accepted resolutions checked: 10 accuracy rows (3, 31, 32,
33, 34, 36, 37, 38, 39, 40), 12 design findings (F1-F12), and ~20 prose
items (9 substantive, the rest stylistic). 36 are fully applied; 6
design findings carry partial residue (findings 1-5 and 9 below); none
is missing outright.

Confirmed clean, specifically:

- Accuracy rows all fixed: 1-code-map section 3 is branch-aware;
  10-summary end-state table now reads moq-media ~10.7k/~9.6k and
  rusty-capture ~4.3k, both derivable from the 7-cut ledger (22,310 -
  ~15,100 = ~7.2k; 5,507 - ~1,250 = ~4.26k; 11,441 - ~700 = ~10.7k,
  minus ~1,100 adaptive+sync = ~9.6k; 572 - ~200 = ~372); 7-cut R-g says
  e2e.rs 4 tests and room.rs 6 tests, which matches the actual files (4
  and 6 test attributes); the iroh-moq 200/370 vs 120/200 grouping is
  reconciled by an explicit note in both 7-cut and 1-code-map;
  maps/moq-main-media.md now carries a version-note header.
- Wave totals: 8-upstream wave 1 is now "~140 LOC across 3 small PRs"
  with the C3 tail-join named, wave 3 is "~3,250 across ~5 PRs" with the
  component sum shown. No other doc quotes wave totals anymore
  (10-summary defers explicitly: "they are not repeated here"). No
  stale ~2,950 or ~350 anywhere.
- Merged dependency table exists in 7-cut section 3 and covers stages,
  waves, R1-R7, and D items; 8-upstream's cross-reference to it (line
  ~740) and its own "Section 3's velocity gate" self-reference are both
  accurate. Wave contents in the table match 8-upstream's wave lists.
- D-list: 3t section 7 defines exactly twelve decisions D1-D12 with
  "Items 1 through 3 gate everything else"; 7-cut P4 and 10-summary both
  now cite D1-D12 / D1-D3 correctly. No "D1 to D6" remains anywhere.
- Slip-floor numbers ~1,300/3%, ~4,800/12%, ~17,400/42% are identical in
  7-cut (table + R-a) and 10-summary (paragraphs 1 and 2); percentages
  check against the 41,564 denominator.
- 9-room: verdict rewritten to lead with the no-round-trip and
  relay/clustering arguments and explicitly disowns the single-protocol
  claim; the two-regressions paragraph specifies the presence beacon
  (unsigned, best-effort, must not grow metadata) and the eager-dial
  connection regression; section 5's media-mesh analogy is qualified
  accordingly; phase 2 carries the multi-room scoping open question with
  three options, an accept-and-filter recommendation, and the warn-level
  logging requirement for scope-rejected announces. Section 3.2
  cross-references the open question.
- Mechanical sweep over the twelve docs: zero em dashes and zero
  non-ASCII; zero "load-bearing"; zero "budget"; "synthetic" only inside
  the verbatim traits.rs doc-comment quote in 4-compare (exempt); no
  "synthesized"; "crown jewel" reduced to a single occurrence (3z line
  295); no doc calls another "pending"; the stale stubs
  maps/moq-dev-media.md and maps/moq-origin-hop.md are referenced only
  by 0-overview's own redirect note. Number fixes landed (5-compare
  1,212; 3-compare 804/559; thousands separators in 4- and 6-compare;
  "moq-lite" in prose).
- New sections read as integral: the 7-cut freshness protocol, churn
  note, and platform verification gate sit inside the risk register and
  ledger where they belong; 8-upstream's bandwidth assumption, velocity
  gate, pilot, RFC-first rationale, and tri-stack pricing are in the
  right places and use the documents' existing vocabulary; 10-summary's
  expectations and alternatives sections match its tone. The 7-cut R-b
  freshness protocol and 8-upstream's intro freshness paragraph overlap
  but agree; acceptable per-doc self-containment.

## Findings

1. **7-cut-plan.md R-c (line ~373) vs 8-upstream-plan.md section 5
   (lines ~848-855).** R-c still closes with "scenario (ii) is the
   floor, not a failure", the exact framing F2's accepted resolution
   removed; 8-upstream now prices the same keep-local world as "worse
   than today, not a neutral floor ... three states instead of one".
   Cross-file seam between the two fix agents: the two docs now state
   opposite characterizations of the same end state. Severity:
   substantive.

2. **10-summary.md expectations paragraph (lines ~25-27) vs 7-cut stage
   2 atomic recommendation.** 10-summary says "~4,800 (12%) lands when
   the dev line ships", but 7-cut's new bridge-cost paragraph recommends
   atomic per-platform switchover, under which the bulk of scenario
   (ii)'s rusty-codecs cuts (openh264, VTB encoder, annexb; ~2,100+ LOC)
   defer past dev release until the wave-2/R4 merges. 7-cut says so
   ("parts of scenario (ii) shift later in time; the end totals are
   unchanged"); 10-summary was not updated to carry the qualifier.
   Severity: substantive.

3. **F4's final clause not applied.** Neither 9-room phase 2 nor
   10-summary states phase 2's standalone payoff as preparatory for the
   dev-gated phase 3; 10-summary (gating reality 1) retains the "only
   work safe to start immediately" framing that F4 attacked as
   conflating "possible on main" with "pays off on main". The rest of
   F4 (verdict, beacon, eager dial) is fully applied. Severity:
   substantive.

4. **0-overview.md Goal (F6, half-applied).** The goal still opens
   "Massively reduce the code iroh-live owns"; the fix added only a
   pointer to 10-summary's scenario expectations rather than the
   restatement F6's resolution asked for. The substance (floor-first
   expectations, burden-weighted caveat) did land in 10-summary, one hop
   away. Severity: nit.

5. **8-upstream C13 (F10, third clause).** The reference-consumer
   argument ("a third-party renderer working purely over the public
   handle vocabulary demonstrates that C2's API is sufficient") still
   does not state that this proof exists only after C2 lands and we port
   the render stack to it; F10 asked for that honesty explicitly.
   Severity: nit.

6. **7-cut churn note (line ~166): "stage 5 phase 1" mislabel.** The
   two-phase-accept rework item belongs to stage 0 (which contains
   9-room phase 1); stage 5 is defined as 9-room phases 2 and 3. Should
   read "stage 0 (9-room-layer phase 1)". Severity: nit.

7. **7-cut stage 2 internal tension.** The stage's Content sentence
   still specifies the interleaved ordering (adopt openh264, VTB
   encoder, front end now; hold the rest), which the appended
   bridge-cost paragraph then supersedes ("This plan recommends the
   atomic variant"). The paragraph flags it ("as written"), but the
   stage reads as plan-then-retraction; the Content line was not
   rewritten to the recommendation. Severity: nit.

8. **9-room 3.1 third bullet vs the phase-2 recommendation.** 3.1 states
   sessions are wired with "a scoped `with_consume` (see 3.2)"
   unqualified, while phase 2 now recommends accept-and-filter (broad
   scope, enforcement in the actor) with per-session scope as the
   single-room special case. 3.2 carries the cross-reference to the open
   question; 3.1 does not. Severity: nit.

9. **F1 location deviation.** The stated resolution named 0-overview as
   home of the plan-freshness section; it landed in 7-cut R-a/R-b (slip
   floor, hedge branch, re-check protocol) and 8-upstream's intro
   instead, with 0-overview carrying nothing but the Goal pointer. The
   substance is complete and consistent; only the promised location
   differs. Severity: nit.

Out of scope but noted: 2-moq-inventory.md (not in the fix wave's file
list) no longer contains "budget" either, so the series-wide ban holds.

## Verdict

The fix wave landed cleanly: all accuracy corrections applied and
re-verified against source, all prose substantives applied, mechanical
rules now pass across the twelve docs, and the new sections are
coherent. The residue is three substantive seams, all of the
cross-file kind the disjoint ownership predicted (findings 1-3), and
six nits. Nothing blocking.
