# Prose and consistency review: plans/refactor numbered docs

Scope: 0-overview, 1-code-map, 2-moq-inventory, 3-compare-codecs, 3z-compare-zerocopy,
3t-compare-traits-api, 4-compare-capture, 5-compare-pubsub, 6-compare-audio, 7-cut-plan,
8-upstream-plan, 9-room-layer, 10-summary. Reviewed against the writing rules (no em
dashes, ASCII, Oxford comma, banned words, consistent vocabulary and numbers, valid
cross-references, dev-flagging discipline, max 3 heading levels). Findings only; no
fixes applied.

Severity: substantive (wrong or contradictory content), stylistic (rule violation,
prose quality), nit (minor polish).

---

## 0-overview.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Lines 89-94 (Document index) | The "Valid maps" list names five maps plus "the current-dev maps listed above", but `moq-net-origin.md`, the single most-cited map in the series (43 references), is only implied by the checklist phrase "the main+dev net/origin layer". A reader scanning the index will not find it. | Name `moq-net-origin` (and the three current-dev media maps) explicitly in the Valid maps sentence. | nit |

Otherwise clean: no em dashes, no banned words, headings flat, dev-only framing is the
document's central section.

## 1-code-map.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Lines 169-174 (section 3, first observation) | States "moq-video capture is a single ffmpeg/libavdevice path with no trait, no backend enum, and no zero-copy frame model" and concludes "Aligning to moq there means either adopting ffmpeg-style backends and dropping the native ones, or keeping this half as the deliberate differentiator." This describes moq main only and is unflagged. It directly contradicts 2-moq-inventory (dev: six native capture backends, ffmpeg fully removed, zero-copy capture-to-encode paths) and the 3/3z/4-compare verdicts. Reads like text written before the stale-dev correction noted in 0-overview's checklist. | Rewrite the observation branch-aware: main is ffmpeg-thin, dev overlaps codec-impl and capture-backend heavily (that is the premise of the cut plan); the genuinely unmatched area is gpu-zerocopy render/import. | substantive |
| Line 121-122 (iroh-moq intro) | Actor sized at "roughly 200 lines"; 7-cut-plan's iroh-moq keep row sizes the same actor complex (dedup, fan-out, ProtocolHandler, incoming stream) at ~370 LOC. The scopes differ but the two presentations of "the actor" disagree; 200 + 120 also leaves ~250 of the 572 unaccounted. | Align the two figures or state what the remainder of lib.rs is. | nit |

## 2-moq-inventory.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 463 (enabler register, Subscription row) | "No per-viewer latency budget on the wire" uses banned word "budget". | "No per-viewer latency bound on the wire" (or "limit"). | stylistic |

Otherwise exemplary: every capability row states its branch, the register's own count
("Eleven dev-only enabler rows in total, of which ten are hard dependencies") is
internally consistent, and version/SHA facts are the canonical ones the other docs cite.

## 3-compare-codecs.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 365 (Opus table header) | "Ours (`codec/opus/`, 345 + 454 L)" sums to 799; the crate figure used everywhere else (1-code-map, 6-compare, 7-cut-plan) is 804. | Use 804 or note the mod.rs remainder. | nit |
| Line 422 (PCM verdict) | "it costs 550 lines and earns them" vs the canonical 559 used in 1-code-map, 6-compare, and 7-cut-plan. | "roughly 560 lines" or 559. | nit |

Otherwise clean: dev-only status flagged in the title, per-section, and again in a
dedicated closing section ("The dev-only dependency, stated plainly"); no em dashes; no
banned words; heading depth 3.

## 3z-compare-zerocopy.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Lines 166, 210, 264, 298 (section 2 headings) | 4 em dashes, the only ones in the whole series, all in headings of the form "### 2a. Capture to encode — verdict: ...". Violates the no-em-dash / ASCII rule. | Replace with a colon or period: "### 2a. Capture to encode: split verdict". | substantive |
| Lines 210, 294 | "crown jewel" used twice in one doc (ours in 2b, theirs in 2c); the metaphor recurs again in 6-compare and 7-cut-plan. Not banned, but a lively synonym repeated enough to become a tic. | Keep at most one; elsewhere say "strongest asset" or name the capability. | stylistic |

Otherwise clean and well-flagged (pinned SHA in scope statement, "moq dev" throughout).

## 3t-compare-traits-api.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 32 | "The factory traits are load-bearing for iroh-live's publish path" uses banned word "load-bearing". | "essential to" / "what iroh-live's publish path depends on". | stylistic |

Otherwise clean: 924 lines with no em dashes, decision list D1-D12 well-structured,
"Items 1 through 3 gate everything else" is the doc's own gating claim (see cross-doc
section: two other docs misquote this as "D1 to D6").

## 4-compare-capture.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 338 | "for us they are load-bearing on macOS camera and all of Windows" — banned word. | "they are the only working path on macOS camera and Windows". | stylistic |
| Line 487 | "currently load-bearing for macOS camera and Windows" — banned word, second occurrence. | Same substitution. | stylistic |
| Line 22 | Verbatim quote of `traits.rs` doc comment contains "synthetic source". Quoted source code, so exempt from the ban, but worth knowing it is there if the ban is enforced mechanically. | None (or trim the quote). | nit |
| Lines 180-190, 421 (tables, section 4) | LOC written without thousands separators ("1655 lines", "2445 lines") while 1-code-map and 7-cut-plan write "1,655", "2,445". Same numbers, inconsistent formatting within the series. | Add separators. | nit |

## 5-compare-pubsub.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 642 | "the 1,124 LOC pipeline module" contradicts the canonical pipeline/ figure of 1,212 LOC (1-code-map section 2, 7-cut-plan ledger). Likely a transposition. | 1,212. | substantive |
| Line 215 | "exceeds the latency budget" — banned word. | "latency bound". | stylistic |
| Line 256 | "tune the skip budget continuously" — banned word. | "skip threshold". | stylistic |
| Line 485 | "the container consumer's skip budget" — banned word. | "skip threshold". | stylistic |
| Line 32 | `moq_lite` (underscore, code style) where the rest of the series writes `moq-lite` for the alias; 9-room-layer carries the terminology note. | Use `moq-lite` in prose, `moq_lite` only in code position. | nit |

Otherwise clean: branch caveat section up front, dev-only enablers flagged inline and
re-listed in the closing paragraph, "rung" reserved for the moq-transcode ladder and
"rendition" for the catalog side, consistently.

## 6-compare-audio.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 56 (decoder table, PLC row) | "no concealment is synthesized on a gap" — inflection of banned "synthetic". | "no concealment is generated on a gap". | stylistic |
| Line 145 | "(2445 + 392 lines)" — no thousands separator (see 4-compare). | "(2,445 + 392 lines)". | nit |
| Line 410 | "the crown jewel of our audio stack" — third doc reusing the metaphor. | See 3z note. | nit |

Otherwise clean and well-flagged (dev pin plus explicit `main:` prefixes on citations).

## 7-cut-plan.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 40 (P4) | "wait for the D1 through D6 decisions of 3t-compare-traits-api.md section 7" implies section 7 contains six decisions. It defines twelve (D1-D12), and this document's own ledger rows cite D7, D8, D9, and D11 as prerequisites, contradicting the P4 framing. 8-upstream-plan cites the range correctly as D1-D12. | "wait for the decision list of 3t section 7 (D1-D12), in particular D1-D3" or explicitly define D1-D6 as the gating subset if that is the intent. | substantive |
| Line 87 | "their macOS camera and Windows backends remove its load-bearing role" — banned word. | "remove its role as the only working path". | stylistic |
| Line 250 | "load-bearing for tests and diagnostics" — banned word. | "essential for tests and diagnostics". | stylistic |
| Line 246 | "the crown jewel" — fourth occurrence in the series. | See 3z note. | nit |

Numbers audited and consistent: 41,564 denominator; scenario totals ~1,300 / ~4,800 /
~17,400 with 3% / 12% / 42% arithmetic correct; per-crate scenario columns sum to the
totals within rounding; ledger LOC figures match 1-code-map.

## 8-upstream-plan.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Lines 17-19 | "The cut plan (7-cut-plan.md, pending) will state what we delete once each contribution lands; cross-references here stay generic until it exists." 7-cut-plan.md exists in the same directory and cross-references this doc's C/R/D items extensively. Stale. | Rewrite: "The cut plan (7-cut-plan.md) states what we delete once each contribution lands." | substantive |
| Line 707 | Same staleness: "The exact deletion schedule belongs to 7-cut-plan.md; until it exists, treat ..." | "... belongs to 7-cut-plan.md; there, 'lands upstream' is the precondition for the corresponding cut." | substantive |
| Line 693 (Wave 3) vs section 4 table | Wave 3 states "~2,950 LOC across ~5 PRs", but its own components sum to ~3,250 (C6 ~750 + C5 ~1,300 + C11a ~450 + C11b ~250 + C12 ~500). 10-summary repeats ~2,950. | Recompute (state ~3,250, or ~3,000 if C11b is excluded) and mirror in 10-summary. | substantive |
| Line 682 (Wave 1) | "Total code: ~350 LOC across 4 small PRs" but the enumerated wave-1 PRs (C7.1 ~30, C7.2 ~50, C14c ~60) are 3 PRs and ~140 LOC. The gap is presumably C3 (~150, marked wave "1-2" in the table), but the text does not say so. | Name the fourth PR or restate as "~140 LOC across 3 PRs plus the RFC". | nit |

Otherwise clean: no banned words, correct D1-D12 citation, C14a-e labels match the
bullet order of section C14, wave totals for waves 2 and 4 reconcile with the table
within the stated rounding.

## 9-room-layer.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 97 | "The load-bearing observation: gossip currently does two jobs ..." — banned word. | "The key observation:". | stylistic |

Otherwise exemplary: the MAIN/DEV split is structural (sections 2.1 vs 2.2 with "Every
item in this list is dev-only" stated first), phases labeled main-compatible vs
dev-dependent, figures consistent with 7-cut-plan and 10-summary.

## 10-summary.md

| Location | Issue | Suggested fix | Severity |
|---|---|---|---|
| Line 50 | "Six decisions (D1 to D6) gate everything" misstates 3t, which defines twelve decisions and says items 1 through 3 gate everything else. Same error family as 7-cut-plan P4. | "Twelve decisions (D1 to D12) are on the table; D1 through D3 gate everything, and the two that must land first are D1 ... and D3." | substantive |
| Line 100 | "Ten of eleven dev-only enablers require the next breaking moq release." Per 2-moq-inventory's register, all eleven rows imply waiting for the release; ten are hard dependencies (7-cut-plan R-a states it correctly). | "Ten of the eleven dev-only enablers are hard dependencies on the next breaking moq release." | substantive |
| Line 61 (end-state table, moq-media row) | "11,441 -> ~8-9k" is not derivable from the cut ledger: 7-cut-plan scenario (iii) removes ~700 from moq-media, leaving ~10,700; even with the parenthetical adaptive+sync shrink (~1,100) the floor is ~9,600. Every other row in this table reconciles with 7-cut-plan. | Either revise to ~9.5-10.5k, or state the extra assumed shrink (wave-4 wrappers, pipeline internals) and its size. | substantive |

Otherwise clean; wave sizes, LOC scenarios, SHAs, and the ~20 PRs / five waves figure
match 8-upstream-plan.

---

## Cross-doc consistency

**Recurring numbers (canonical value, status).**

| Number | Canonical | Status |
|---|---|---|
| 41,564 LOC core total | 1-code-map sec 1/3 | Consistent (1-code-map, 7-cut-plan, 10-summary). |
| Cut scenarios ~1,300 / ~4,800 / ~17,400 | 7-cut-plan sec 2 | Consistent (7-cut-plan, 10-summary); percentages check out. |
| Wave sizes ~350 / ~4,600 / ~2,950 / ~1,300 | 8-upstream sec 3 | Consistent between 8 and 10, but wave 3's ~2,950 contradicts its own component table (~3,250), and wave 1's "4 PRs" contradicts its enumerated 3. |
| 11-row enabler register | 2-moq-inventory table 2 | Register itself consistent (11 rows, 10 hard); 7-cut-plan quotes it correctly; 10-summary flattens it to "ten of eleven ... require the release". |
| SHAs 2be3a55f / 261c2048 / b0a8c834, 3/282 | 0-overview | Consistent everywhere (0, 2, 5, 10; spacing varies trivially between "3 / 282" and "3/282"). |
| pipeline/ LOC 1,212 | 1-code-map | 5-compare line 642 says 1,124. Diverges. |
| iroh-moq after ~350-400 | 9-room-layer phase 1 | Consistent (9, 10, 7-cut arithmetic); but 1-code-map's "actor roughly 200 lines" does not reconcile with 7-cut-plan's ~370 keep. |
| codec/opus 804, codec/pcm 559 | 1-code-map | 3-compare drifts ("345 + 454", "550 lines"). |

**Cross-references.** No broken references. Every numbered doc promised by 0-overview's
checklist exists; every "see X.md" and every maps/ citation resolves (all 11 map files
present, including the two declared stale stubs); plans/old/review-moq-usage.md,
rooms-overhaul.md, and adaptive-track-refactor.md all exist. The one reference-level
defect is 8-upstream-plan calling 7-cut-plan.md "pending" twice (flagged above).
Section-number references spot-checked (3-compare sec 1/5/6/7/8/10, 3z sec 4 R1-R7,
3t sec 7 D-list, 9-room sec 3/5/6) all point at real sections.

**Decision-count contradiction (the largest cross-doc issue).** 3t defines D1-D12 and
gates on D1-D3; 8-upstream cites D1-D12 correctly; 7-cut-plan P4 and 10-summary both
say "D1 through D6", while 7-cut-plan's own ledger cites D7-D11. One canonical framing
is needed.

**Dev-branch flagging discipline.** Strong across the series: 0, 2, 3, 3z, 3t, 4, 5, 6,
8, 9, 10 all pin the SHA and flag dev-only capabilities at or before first mention (2
and 9 are the model). The one failure is 1-code-map section 3, which presents moq
main's ffmpeg-era capture as "moq's own stack" unflagged and draws a conclusion the
rest of the series refutes (substantive finding above).

**Vocabulary.** "rendition" (catalog/subscriber side) vs "rung" (moq-transcode ladder)
is used consistently per side; no stray "quality" as a noun-of-art. Branch naming
varies mildly: "moq dev", "the dev branch", "the dev line", plus "the next breaking
release" vs "the next breaking bump". All comprehensible, but standardizing on "moq
dev" for the code state and "the next breaking release" for the event would remove the
wobble (stylistic). "moq-lite"/"moq_lite" alias styling varies (5-compare); 9-room
carries the terminology note.

**Banned words, series totals.** load-bearing: 6 (3t x1, 4-compare x2, 7-cut x2,
9-room x1). budget: 4 (2-inventory x1, 5-compare x3). synthetic: 1 (4-compare, inside
a verbatim source quote; exempt). synthesized: 1 (6-compare, inflected form). No
clobber, reap, wedge, or infix anywhere.

**Em dashes / non-ASCII.** 4 em dashes total, all in 3z headings; every other doc is
0 and fully ASCII.

**Headings.** All 13 docs stay within 3 levels; no over-nesting found.

**Style rules otherwise.** Scope-statement openings ("This document compares ...") are
present in 1, 2, 5 and are legitimate scope statements per the rules. Oxford comma
discipline is good; a scan for "x, y and z" triads found no true violations. No
reflexive triadic filler lists stood out; enumerations are factual.
