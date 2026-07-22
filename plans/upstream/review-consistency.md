# Consistency review: upstream plan set (19 plans + overview)

Reviewer pass over the frozen-contract adherence, dependency tree, seam
declarations, coordination points, template/anchor sanity, and overview accuracy.
Verified moq anchors against `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`.

## Verdict

**Mostly consistent — ready to hand to PR agents after one substantive fix.**

The base contract (B1 Native/DmaBuf, B2 encode(&Frame, Timestamp, keyframe) ->
Vec<Packet>, B3 native(), B4 registration) is quoted verbatim and every consumer
uses the exact signatures. Dependency headers match the overview. The three
non-trivial seams (moq-vaapi ownership, B4/Android placement, render decoder deps)
are declared correctly and without contradiction. Candidate-table additivity and
coordination-point declarations are present across all leaves. The one real problem
is a factual error in av1-software that invents a breaking-change gate that does not
exist. The rest are anchor-slop nits.

## Counts

- Blocking: 0
- Substantive: 2 (findings 1, 2)
- Nit: 4 (findings 3-6)

## Findings

### 1. av1-software falsely claims the encode `Codec` enum is not `#[non_exhaustive]` (SUBSTANTIVE)

- Location: `codec/av1-software.md` lines 74-78 ("The enum is not currently
  `#[non_exhaustive]` on the encode side, so confirm whether adding the variant is
  breaking and, if so, flag it to the B-plan owner"), and Coordination lines
  137-138 ("If adding the `Av1` variant to the encode `Codec` enum is breaking, stop
  and file the gap against the base plan").
- Issue: `rs/moq-video/src/encode/encoder.rs:22` is `#[non_exhaustive]` on `pub enum
  Codec` (verified in source; the doc at line 14 even says "so new codecs can be
  added without" a break). Adding `Av1` is purely additive and breaks nothing. The
  plan instructs the PR agent to open a spurious coordination stop / base-owner RFC
  over a non-issue, which would stall an otherwise independent leaf.
- Severity: substantive — it manufactures a false gate that will misdirect the PR
  agent. Borderline blocking because it changes the plan's execution path.
- Fix: correct the plan to state the encode `Codec` enum is already
  `#[non_exhaustive]`, so adding `Av1` is additive; drop the "if breaking, file
  against base plan" clause in both the Target-in-moq and Coordination sections.

### 2. Unresolved feature-gate name for private `Frame::DmaBuf` (SUBSTANTIVE)

- Location: `base/B1-frame-vocabulary.md` lines 85, 84 ("Gated on `vaapi` (or a new
  `dmabuf` feature)"); `capture/pipewire-dmabuf.md` lines 51, 138-140, 176;
  `codec/v4l2-decode.md` lines 49-52, 137.
- Issue: B1 gates the private `Frame::DmaBuf` variant under `cfg(all(target_os =
  "linux", feature = "vaapi"))` but hedges "or a new `dmabuf` feature". pipewire-dmabuf
  and the v4l2-decode EXPBUF follow-up are DMA-BUF *producers* that do not otherwise
  require `vaapi`; both correctly flag that they must gate on "whatever B1 chose". The
  choice is left open in B1. A PR agent building the PipeWire zero-copy path on a
  host without the `vaapi` feature cannot produce `Frame::DmaBuf` until B1 picks. All
  three plans flag it consistently, so it is not a contradiction, but it is an
  undecided design point that gates two producer leaves.
- Severity: substantive — a shared decision that should be resolved in B1 before the
  producer leaves are dispatched.
- Fix: have B1 commit to a shared `dmabuf` feature (not `vaapi`) for the private
  variant, so any producer (pipewire, v4l2, vaapi) can enable it independently, and
  update the three plans to name that feature rather than "vaapi or dmabuf".

### 3. render plan states `into_i420(&self)`; actual signature consumes `self` (NIT)

- Location: `render/moq-video-render.md` line 76 ("`decode::Frame::into_i420(&self)
  -> Result<Bytes, Error>`") and the fallback pattern `self.render_i420(frame)` over
  `&Frame` (lines 170-213, 239).
- Issue: `rs/moq-video/src/decode/mod.rs:94` is `pub fn into_i420(self)` — it consumes
  the frame (doc: "Consumes the frame"). The render crate's design borrows `&Frame`,
  reads `native()`, then wants to fall back to `into_i420()` on the same borrow, which
  the consuming signature does not allow without a clone. This is an out-of-tree crate
  concern (no moq source impact), but the stated signature is wrong and the fallback
  ergonomics need a note.
- Severity: nit (downstream crate only; zero moq-side diff).
- Fix: correct the signature to `into_i420(self)`; note that the renderer must clone
  the frame or restructure to consume it on the CPU path. If the borrow-then-fallback
  ergonomics matter, that is a candidate B3 gap to file (a `&self` I420 download), not
  a silent divergence.

### 4. v4l2-decode conflates the decode HARDWARE slice and SOFTWARE const anchor (NIT)

- Location: `codec/v4l2-decode.md` line 100 ("decode `HARDWARE` slice of
  `rs/moq-video/src/decode/backend/mod.rs:89-114`").
- Issue: in source the decode `const HARDWARE` slice is lines 89-107 and the single
  `const SOFTWARE` Candidate is 110-114 (as B4 and vaapi-decode correctly cite,
  `:89-108` / `:110-114`). v4l2-decode's `:89-114` spans both, implying the new row
  goes in a range that includes SOFTWARE. Harmless but imprecise for a PR agent.
- Severity: nit.
- Fix: change to `:89-107` (the HARDWARE slice only).

### 5. Overview plan-index "Depends on" for v4l2-camera-enum omits the qualifier (NIT)

- Location: `0-overview.md` line 412 (plan-index table row: "v4l2-camera-enum ... B1")
  vs line 233 (dependency tree: "`v4l2-camera-enum [B1 for zero-copy; else
  independent]`") and the plan header ("B1 (only if zero-copy EXPBUF is pursued);
  otherwise independent").
- Issue: the overview's own table says a flat "B1" while its tree and the plan say
  "B1 only for the optional EXPBUF follow-up, else independent." A PR agent reading
  only the table would over-constrain the default-scope leaf onto the base branch.
- Severity: nit.
- Fix: make the table row read "B1 (zero-copy only)" to match the tree and plan.

### 6. av1 Coordination describes but does not name coordination point 1 (NIT)

- Location: `codec/av1-software.md` Coordination lines 137-138.
- Issue: every other base-consumer leaf explicitly cites "coordination point 1 (base
  API freeze)" in its Coordination section; av1 describes filing against "the base
  plan" without naming the point. Cosmetic inconsistency in an otherwise uniform set.
  (Moot once finding 1 is fixed, since that clause is removed.)
- Severity: nit.
- Fix: fold into finding 1's fix or drop the clause.

## Verified clean (spot-checks that passed)

- B2 producer rewrite anchor `encode/producer.rs:386-392` matches source verbatim
  (the `Timestamp::from_micros(clock.micros())` at 386, `encoder.encode(frame,
  force_keyframe)` at 387, `producer.publish(packets, ts)` at 392).
- vaapi-encode's claim "the encode candidate table already lists a VAAPI H.264 entry,
  so no new row" is correct: `encode/backend/mod.rs` HARDWARE has a `vaapi` Candidate
  with `codecs: &[Codec::H264]`.
- vaapi-decode's claim that the decode table has no VAAPI entry (only videotoolbox,
  mediafoundation, nvdec) is correct, so it must add a row — matches source.
- Encode `Backend` trait method set (encode/finish/set_bitrate/name) and `set_bitrate`
  "no default, must return BitrateUnsupported" match source exactly; B5's honesty
  contract is faithful.
- moq-vaapi ownership seam: vaapi-decode OWNS (export+VPP+cros-codecs decode),
  vaapi-encode CONSUMES — no double-claim, no gap.
- B4/Android: android defers placement to coordination point 6 and depends on B4 for
  the external path; B4 states it is conditional on that decision and must not open as
  a PR until made. Consistent.
- All 19 plans carry the template sections (B5 legitimately substitutes its
  checklist/Error-variant sections for its reference-only nature).
