# Adversarial design review: refactor plan set

Reviewer stance: staff-engineer adversarial review of the plan, not the facts.
Facts are assumed verified elsewhere; this document attacks decisions,
orderings, framings, and the arguments that carry the most load. Scope: all
numbered docs, with emphasis on 0, 10, 7, 8, 9, and 3t. Findings are ordered
by severity. Where I checked something in the moq repo myself, I say so.

---

## Critical

### F1. The dev-alignment bet has no churn hedge and no plan-freshness protocol

Attacked claim: 0-overview "The central fact" and 10-summary, which frame dev
`261c2048` as the alignment target, and 7-cut-plan R-b, which mitigates churn
with "treat all dev citations as direction, not API contract".

Counter-argument. The plan understates how unstable the target is. The 282
dev-only commits were authored between 2026-05-28 and 2026-07-17, roughly
seven weeks at 5.5 commits per day; 30 of them landed on the pin day itself,
and 40 carry breaking-change markers (checked in the moq repo). The pin is a
snapshot of a branch in mid-flight, with an unfinished wire version
(Lite06Wip) and a pre-bump rename sweep still in progress. Every dev-facing
number in these documents, including the headline 42 percent cut, is computed
against an API surface the plan itself says not to trust. That is a
contradiction the plan never resolves: the LOC arithmetic is presented with
plus or minus 15 percent confidence while its foundation is labeled
"direction, not contract". If dev churns the frame model, the candidate
tables, or the Producer/Consumer shapes before release, the stage 2 through 4
designs and most of the D1 through D12 analysis need re-derivation, and the
hundreds of pinned file:line citations rot silently.

The plan also never answers its own hardest question: what is this plan worth
if the dev release slips six months? The honest answer is: stages 0 and 1
(about 1,300 LOC of cuts, 3 percent), rooms phases 1 and 2, and the wave-1
PRs (about 350 LOC). That is genuinely useful hygiene, but it is roughly a
week or two of work, and it does not need this planning apparatus. The other
97 percent of the plan is contingent on an external event with no date. A
plan that is 97 percent contingent should say so in its first paragraph.

Missing hedges the plan should have considered: (a) a feature branch that
tracks dev as a git dependency, continuously compiling our stage-2 adoption
shims against dev HEAD, which converts silent plan rot into visible build
breaks and gives an early-warning channel on API direction; (b) a periodic
re-validation trigger, for example "re-run the enabler register against dev
HEAD every N weeks or on every 50 dev commits, and re-confirm the ledger
before any stage 2 work starts"; (c) an explicit expiry date on the plan
itself.

Severity: critical.

Resolution: add a plan-freshness section to 0-overview stating the calendar
assumption under which the plan pays off, the conditions that invalidate it,
and a concrete tracking mechanism (dev-tracking branch or scheduled
re-inventory). State the six-months-slip answer explicitly in 10-summary.

### F2. The upstream bet models our authoring effort but not their review bandwidth, and the fallback recreates the dual stack

Attacked claim: 8-upstream-plan working assumption ("we have a good
relationship with the moq maintainers, and work that arrives in their shape
has a realistic path to acceptance"), the wave structure, and the per-item
fallbacks in section 5.

Counter-argument. Every size estimate in the plan measures our cost to write
the PRs. Nothing measures the binding constraint, which is maintainer review
bandwidth for roughly 10,000 LOC over 20 PRs into a project that vendors
everything, rewrites contributions in review, and is simultaneously executing
its own 282-commit rewrite. Wave 2 alone is about 4,600 LOC including a
1,000 to 1,400 LOC FFI growth of moq-vaapi, described by the plan itself as
"the largest single piece of upstream work and the one most likely to meet
resistance". A series like that plausibly takes months to land even with a
willing maintainer, and the cut plan serializes stages 2 through 4 behind it
("lands upstream" is the precondition for every corresponding cut). The
"good relationship" assumption is asserted, not evidenced: no merged PR from
us, no issue thread, no maintainer statement is cited anywhere in the plan
set. For a bet this large, that is a hole.

The fallback analysis makes the problem worse, not better. Every fallback is
"keep local", and the plan calls scenario (ii) "the floor, not a failure".
But examine what that floor actually is: we have adopted their openh264, VTB
encode, and NVENC paths, while carrying our VAAPI, V4L2, Android, VTB decode,
and Opus paths locally, forever, behind our own traits, against their frame
model where C2 landed or ours where it did not. That is the current
dual-stack world with an extra stack in it. The refactor will have bought
~4,800 LOC of deletions at the price of a permanently mixed codec layer and
20 PRs worth of upstream engagement. The plan never asks whether that trade
is positive, because it never prices the mixed-stack carrying cost (see F3).

There is a smaller-first strategy the plan does not evaluate: treat wave 1
plus a single pilot (C2 minimal variant plus C1b VAAPI decode, the highest
value, least contested item) as an acceptance-velocity experiment, and gate
the remaining ~15 PRs on its measured outcome. That gets most of the
strategic information for perhaps 1,500 LOC of exposure, and 80 percent of
the Linux-story value if it lands. The plan jumps straight to the 20-PR
program.

Severity: critical.

Resolution: add an explicit decision gate after the first wave-2 PR with
measured criteria (review latency, reshape depth, maintainer engagement)
that select between continuing, shrinking to C1+C2+C3 only, or abandoning
the upstream track. Add evidence for the relationship assumption or mark it
as an unvalidated premise. Price the scenario-(ii) end state honestly as a
tri-stack, not a floor.

### F3. Stage 2 creates a mixed codec stack whose bridging cost appears nowhere in the ledger, and an atomic-per-platform alternative is never examined

Attacked claim: 7-cut-plan stage 2 ordering (adopt openh264, VTB encode,
annexb front end, NVENC/MF/H.265 now; hold VAAPI, V4L2, Android, VTB decode,
Opus until R2/R3/R4/R7/D3 land), and the ledger totals it feeds.

Counter-argument. For the entire middle of the migration, iroh-live runs two
frame models (their private `Frame` reachable only through their concrete
`Encoder`/`Decoder`, and our `VideoFrame`), two config vocabularies, two
error models, and two dispatch mechanisms (their candidate table for adopted
backends, our `DynamicVideoDecoder` cascade for held ones). moq-media's
pipelines must arbitrate per platform and per codec which stack a frame
enters, and convert at every crossing. The dispatch layer the ledger marks
"cut-after-upstream" (codec.rs plus dynamic.rs, 522 LOC) must first grow to
span both stacks before it can shrink. The commit strategy compounds this:
feature-flagged dual paths with tests running against both doubles the test
matrix for the duration. None of this interim code, none of the conversion
shims, and none of the doubled test cost appears in any table; the ledger
counts only deletions. The 42 percent headline is a gross number presented
as a net one.

The unexamined alternative is atomic-per-platform switching: hold each
platform entirely on our stack until every replacement for that platform has
landed and released, then switch the platform in one stage (macOS switches
when R1+R4 land, Linux non-NVIDIA when the VAAPI/V4L2 series lands, and so
on). This is slower in calendar time to first adoption but keeps exactly one
stack per platform at all times, eliminates the bridge code, and makes each
switch independently revertible. Given that stage 2's early adoptions mostly
buy us things we do not ship on today (NVENC, Media Foundation, H.265 on
Windows hardware we have no CI for, see F8), the calendar argument for the
mixed ordering is weak. The plan chose the interleaved ordering without
stating the comparison.

Severity: critical.

Resolution: add a bridging-cost estimate (LOC written and later deleted,
test-matrix growth, duration of the mixed state) to stage 2, and a explicit
comparison against atomic-per-platform switching. If the mixed ordering
survives that comparison, the ledger should carry the bridge as a cost row.

---

## Substantive

### F4. The rooms migration's headline wins are overstated, and phase 2's standalone payoff is thin

Attacked claim: 9-room-layer section 6 verdict: "it removes a whole protocol
(gossip KV) from the announcement path, it improves leave liveness from a
2-minute horizon to session-close latency".

Counter-argument. Under Variant A, which is the recommendation, gossip stays.
The system count does not drop from two to one; it goes from gossip-plus-KV
plus-moq to gossip-plus-moq. "Removes a whole protocol" is true only of the
smol-kv layer, while the gossip dependency, its wire protocol, and its
overlay maintenance all remain. The liveness win is real but oversold as a
migration justification: a fraction of it is available inside the current
design by shortening the KV expiry horizon and leaning on the
broadcast-close inference that rooms.rs already implements. The strongest
honest arguments for the migration are different ones, and the doc makes
them but buries them: the announce stream hands you the `BroadcastConsumer`
directly instead of a dial-then-request round trip, and the path namespace
is the same primitive moq-relay clustering uses, so relay-hosted rooms need
no iroh-live-specific room code. The verdict paragraph should lead with
those, because they survive scrutiny and "single protocol" does not.

Two costs are waved off. First, the migration replaces signed, attributable
KV state with unsigned gossip presence beacons; the doc analyzes the loss of
signed state for announces but never specifies what the presence beacon
looks like or whether it is signed, which is a regression against the
current signed `PeerState` and deserves its own paragraph. Second, discovery
changes from subscribe-driven dialing (today you dial a peer when you want
its broadcast) to eager dialing of every discovered peer (announce
presupposes a session), so connection count in a room becomes N-1 per peer
regardless of subscription interest. Section 5 justifies O(N^2) announce
traffic by analogy to the media mesh, but the media mesh is
subscription-driven and this is not. For viewer-heavy rooms the analogy
fails.

Finally, phase 2's standalone accounting is thin: the ledger nets about 125
LOC of deletions in iroh-live for a wire-visible protocol change that adds a
throwaway debounce, a roster-completion heuristic (main has no AnnounceOk),
and new failure modes. Phase 2's real value is preparing for the dev-gated
phase 3 payoffs (migration, relay rooms). It is therefore also a bet on the
dev timeline, and labeling it "the only work safe to start immediately"
(10-summary) conflates "possible on main" with "pays off on main".

Severity: substantive.

Resolution: rewrite the verdict to lead with the round-trip and relay
arguments, drop or qualify the single-protocol claim, specify the presence
beacon format and its signing story, address the eager-dial connection
regression for large rooms, and state phase 2's payoff honestly as
preparatory.

### F5. Phase 2's authorship enforcement exists as a primitive but its wiring conflicts with connection dedup and dynamic room membership

Attacked claim: 9-room-layer 3.2, "Authorship enforcement per session. On
accept, the consume-side producer handed to the session is scoped to
`<room>/<remote-endpoint-id>/`, so a peer can only announce under its own
id. This is precisely what `OriginProducer::scope`/`with_root` exist for."

Counter-argument. I verified the mechanism side: `scope` and `with_root`
exist on moq-net main (`origin.rs:775`, `origin.rs:818`, consumer variants
at 993 and 1007), scoped `publish_broadcast` returns `false` for
unauthorized paths, and the lite session takes an `OriginProducer` for the
consume direction. So the spoofing answer for a single-room full mesh is
real, existing mechanism plus room-layer glue we must write. The plan is
not wrong on that narrow point, but phase 2 still has an unsolved design
problem the doc never mentions: the scope is fixed when the session is
wired, while sessions are deduped one-per-EndpointId across the whole node
(iroh-moq actor, preserved by design in 3.1) and room membership is
dynamic. When we share rooms R1 and R2 with the same peer, the consume
scope must be the union of `<R1>/<peer>/` and `<R2>/<peer>/`, and it must
change when either side joins or leaves a room mid-session. Nothing cited
in moq-net supports rescoping a live session's wired producer. The
workarounds all have costs the plan has not chosen between: per-room
sessions (breaks dedup and multiplies connections), session teardown and
re-establishment on membership change (visible interruption, interacts
badly with the debounce), or wiring a broad scope and filtering announces
in the actor (reopens the spoofing question at a layer the doc has not
analyzed). Additionally, unauthorized announces are dropped silently
(a `bool` return), which turns future authorization bugs and version skew
into invisible missing-peer symptoms; the plan should decide where that is
logged.

Severity: substantive.

Resolution: add a multi-room scoping design to 9-room-layer (chosen
workaround, its costs, and whether an upstream ask for live rescoping is
warranted), and an observability note for scope-rejected announces. Until
that exists, phase 2 should be marked as having an open design question,
not just open security analysis for phase 3.

### F6. "Massively reduce owned code" is not what the numbers deliver, and the plan reports the best case as the headline

Attacked claim: 0-overview goal ("Massively reduce the code iroh-live
owns") and the 10-summary one-paragraph answer leading with ~17,400 LOC
(42 percent).

Counter-argument. The 42 percent figure is the full-success scenario: dev
released, all 20 PRs accepted including the three the plan itself rates as
acceptance risks (moq-vaapi growth, in-tree Pi/Android backends, the frame
vocabulary's home), and all local stages executed. The plan's own floor is
12 percent. A probability-weighted expectation is somewhere in the 15 to 30
percent range over a 12-plus-month horizon, and zero to 3 percent in the
first months. Against the 53,000 LOC workspace (the denominator a reader
will naturally reach for), even the best case is 33 percent. Meanwhile the
24,000 LOC that stays is by the plan's own account the highest-expertise
code we own (render and zero-copy import, audio engine with AEC, Linux
capture, adaptive, sync), while much of what gets cut (openh264 wrapper,
VTB encoder, config mirror, stubs) is the cheapest code to maintain. LOC
removed is therefore a poor proxy for maintenance burden removed, and the
plan never provides the burden-weighted view. "Massively reduce" is the
wrong framing for "delete the redundant half of the codec layer, keep every
differentiator, and add upstream engagement as a standing obligation".
Note also that maintaining our backends inside moq's tree after acceptance
is not zero cost; keep-and-upstream-copy converts local maintenance into
upstream maintenance under their review process, it does not eliminate it.

Severity: substantive.

Resolution: restate the goal in 0-overview to match the actual outcome;
lead 10-summary with the floor and the expected case, with the 42 percent
labeled as full-success; add one paragraph on burden-weighted rather than
LOC-weighted savings. Tell the user to expect roughly 3 percent now, 12
percent at dev release, and 40 percent only if the full upstream program
succeeds over a year or more.

### F7. The rejected alternatives are never priced, so the chosen path cannot be shown to dominate

Attacked claim: the plan set as a whole; no document contains an
alternatives section.

Counter-argument. Two serious alternatives are absent. First, the null
hypothesis: stay on our stack, upstream nothing beyond goodwill fixes, and
track moq-net/hang/moq-mux for transport and catalog only. Its cost is
continued ownership of code that already works and that we already know how
to maintain; its benefit is zero coordination risk, zero dev-timeline
exposure, and zero mixed-stack interim. Given the plan's own floor is 12
percent, the delta between the null and the floor is modest, and the null
should have been priced as the baseline every scenario is measured against.
Second, the fork: fork moq-video (or vendor its useful backends), merge our
backends into the fork, and publish as our own crates. That achieves
single-stack coherence, NVENC/MF/H.265 gains, and full iteration speed
without upstream review latency, at the cost of a permanent fork burden.
The plan's own fallback world (multiple keep-local outcomes) is a de facto
scattered fork with none of a real fork's coherence, so the comparison is
directly relevant, not hypothetical. A plan recommending a high-coordination
path is only credible if it shows the low-coordination paths losing on
stated criteria.

Severity: substantive.

Resolution: add an alternatives section (one page) to 10-summary or
0-overview pricing the null and the fork against the recommended path on
maintenance burden, calendar risk, and capability outcome.

### F8. The verification principle P1 is unenforceable on exactly the platforms stages 2 and 3 switch

Attacked claim: 7-cut-plan P1 ("Nothing is cut until its replacement is
proven in-tree", enforced by named test gates) together with stage 2 and 3
content, and the upstream plan's offers of hardware validation.

Counter-argument. R-g concedes there is no macOS or Windows CI, and that
the zero-copy e2e runs by hand on one Intel Linux machine. Stage 2 adopts
the VTB encoder and gains NVENC, Media Foundation, and H.265; stage 3
adopts Apple and Windows capture; none of these has an automated gate, and
NVENC/MF/H.265 have no local hardware at all in evidence. So the plan's
strongest safety claim, "nothing is cut until its replacement passes an e2e
test on the new path", cannot actually be executed for the majority of
stage 2 and all of stage 3; "add macOS CI (or a manual hardware checklist)"
is mentioned but neither is specified, owned, or scheduled. The same gap
undermines the upstream plan's credibility offers: C1 promises
"CI-adjacent hardware validation on MTL" and every backend "a hardware
round-trip test in the style of their VideoToolbox and NVENC tests", which
we cannot run in CI either. A reviewer at moq will notice. The stage-4
adaptive-switching integration test is likewise named as a prerequisite
with no owner or design.

Severity: substantive.

Resolution: before stage 2 is scheduled, either stand up the macOS runner
and define the Windows story, or downgrade P1 honestly to "proven on
platforms we can test, checklist-verified elsewhere" and accept the risk in
writing. Name owners for the missing test prerequisites in section 5.

### F9. The accounting is one-sided: deletions are counted, forced churn and throwaway code are not

Attacked claim: the scenario table in 7-cut-plan and the per-stage content.

Counter-argument. Several costs the plan itself describes never appear as
numbers. The dev bump, when it comes, renames every moq_lite path and
touches every consumer ("dominated by the upstream bump churn", 9-room-layer
phase 3); that migration cost is incurred even in a world where we adopt
nothing new, and it belongs in the ledger as a negative. The stage 2/3
bridge and flag machinery (F3) is written to be deleted. The phase 2
unannounce debounce is explicitly throwaway. Phase 1 is built on moq-native
main's two-phase iroh accept, which dev has already collapsed to one phase,
so part of phase 1 is known rework at bump time and is still counted as
pure win. Stage 1 adopts `moq_net::Timestamp` end to end before the wire
version stabilizes (Lite06Wip), accepting a risk of converting twice.
Individually small, together these mean the net LOC and effort picture is
meaningfully worse than the table implies, in the same direction as F3 and
F6.

Severity: substantive.

Resolution: add a "costs" column or companion table: bump migration
estimate, bridge code, throwaway code, known rework. Net numbers, not gross.

### F10. Sequencing chains the whole middle of the plan behind the least-controlled event, and the cross-plan dependency graph is never drawn

Attacked claim: 7-cut-plan section 3 dependency summary and 8-upstream-plan
section 3.

Counter-argument. Stage 2's entry condition is the breaking release AND
D1 through D3 settled upstream; D1/D3 land, if at all, on dev; the release
date is unknown. Stage 2 gates stages 3 and 4. So four of six stages are
serialized behind an unbounded external dependency, and the plan's own
"safe to start immediately" set is the small tranche of F1. That structure
is defensible only if stated plainly, and it is not; the stages read as a
schedule. Additionally, the cut plan and upstream plan express dependencies
in two different vocabularies (stages and R/D identifiers versus waves and
C identifiers) and no single diagram joins them; the wave-2-unlocks-stage-2
cuts, wave-4-unlocks-stage-4 relationships are stated in prose in two
places and would not survive a re-plan without error. There is also a soft
circularity worth surfacing: the strongest argument for upstream accepting
C2 is the render stack as reference consumer (C13), but the render stack
only becomes that consumer after C2 lands and we port to it; the RFC should
not imply the proof exists today.

Severity: substantive.

Resolution: one merged dependency graph covering stages, waves, R and D
items; an explicit statement that stages 2 through 4 are contingency-
scheduled, not calendar-scheduled; and a restated C2 argument that is
honest about when the reference consumer exists.

---

## Stylistic

### F11. Scenario (iii) is fuzzily defined and its total appears to undercount its own definition

Attacked claim: 7-cut-plan scenario table, moq-media row (~700 in scenario
iii) versus 10-summary ("adaptive and sync shrink to wrappers if wave 4
lands").

Counter-argument. Scenario (iii) is "dev + our upstreams accepted", which
by the upstream plan includes wave 4 (C8 adaptive, C9 playout clock). If
wave 4 lands, adaptive.rs and sync.rs (about 1,130 LOC) shrink to thin
wrappers, but the moq-media row credits only ~700 total including unrelated
items, so either the scenario excludes wave 4 (then say so) or the total
undercounts (then the 42 percent is conservative, which is worth claiming
explicitly since it strengthens the plan's honesty). Either way the
scenario definitions should state exactly which waves each column assumes.

Severity: stylistic.

Resolution: define scenarios by wave list; reconcile the moq-media row.

### F12. The engagement strategy front-loads the biggest ask and misplaces its own trust-builder

Attacked claim: 8-upstream-plan section 5, "How to open": an RFC proposing
D1 and D3 as the opening move, with goodwill PRs in parallel; and section 3
placing C6 AV1 in wave 3 while section 5 calls it "the first medium-sized
PR to establish trust".

Counter-argument. The opening package asks the maintainers, in their first
substantial interaction with us, to accept a public frame vocabulary (their
single biggest stability concession, against their stated no-public-backend
posture) accompanied by a report that their VAAPI backend is unvalidated.
However well written, that can read as outsiders arriving with API demands
and criticism before any code of ours has been through their review. The
plan's own logic says calibration should come first: small PRs merged, then
the medium AV1 decode PR that touches no contested decision, then the RFC
with merged-code credibility behind it. The current text has the trust
sequencing argument in section 5 and contradicts it in the wave assignment.

Severity: stylistic.

Resolution: re-order the engagement narrative: goodwill PRs and the
validation report (framed as an offer of test hardware, not a defect
report), then C6 decode if the rav1d pin allows, then the RFC. Move the
RFC trigger from "now" to "after first merges".
