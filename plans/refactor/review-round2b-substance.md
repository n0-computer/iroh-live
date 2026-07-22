# Round 2b Review: does the rewrite deliver the maintainer's three asks?

Adversarial substance review of `3u-moq-changes.md` (centerpiece), `3t-compare-traits-api.md`,
`3z-compare-zerocopy.md`, with `8-upstream-plan.md` and `10-summary.md` for integration.
Claims cross-checked against moq at HEAD `3a3e0ea8`.

## Overall verdict: YES

All three asks are delivered. (1) Render upstreaming is investigated with a recommendation
that is consistent across all four docs; capture zero-copy upstreaming is covered separately
and concretely. (2) The interface comparison (3t) is genuinely exhaustive: every codec trait
and method in side-by-side tables, a field-by-field frame-model dive, a streaming-vs-one-shot
analysis grounded in source line cites, and a complete D1–D12 decision list. (3) `3u` has all
four requested sections plus a sequenced change table, with real `file:line` targets and Rust
type sketches in moq's vocabulary (`Native` enum, `native()` accessor, public `Backend` trait,
`Registration`/`register_encoder`, PTS-through-encode). The findings below are refinements, not
missing deliverables; none rise to critical.

---

## Findings, by severity

### F1 (substantive) — 3u cites the transcode-input line wrong, undercutting its own verification boast
Location: `3u-moq-changes.md` Section 1, line ~43: "`Encoder::encode(&decode::Frame, keyframe)`
(the in-tree transcode input, `encode/encoder.rs:279-281`)". The actual method is at
`encode/encoder.rs:249-251` (verified at `3a3e0ea8`); lines 279-281 are inside the
`#[cfg(test)]` module (`software_encoder_emits_annexb`). Both sibling docs cite it correctly
(`3t` §2.1 and `3z` §1.2 both say 249). The problem is not the six-line slip itself but that
3u's preamble explicitly claims "every `rs/...` citation was read from that tree directly ...
the citation below is the value verified at `3a3e0ea8`." A wrong cite pointing into test code
directly contradicts that claim in the doc that leads on rigor.
Resolve: change `279-281` to `249-251`.

### F2 (substantive) — 3u drops the maintainer's explicitly-named third render option
Location: `3u-moq-changes.md` §1b "The moq changes for problem two": "Two options, and the
recommendation is unambiguous" — then lists only Option A (in-tree `moq-video-render`) and
Option B (out-of-tree crate). The maintainer's ask named three placements: upstream in-tree,
out-of-tree crate, or "keeping it in-repo fully aligned to moq's model." `3z` §4 correctly
enumerates all three (A/B/C) and recommends B; `10-summary` also names the keep-in-repo-aligned
fallback. So 3u, the centerpiece, is narrower than both the ask and its own sibling on the
option set it presents. The recommendation (B) is still consistent everywhere, so this is a
completeness/coherence gap, not a contradiction.
Resolve: add Option C (keep-in-repo, aligned to moq's frame model) to 3u §1b as the minimal
fallback, matching 3z §4.

### F3 (substantive) — Section 4's per-backend external-vs-in-tree list is video-only
Location: `3u-moq-changes.md` §4 "Recommendation per backend" covers VAAPI, V4L2, AV1, Android,
and the renderer, but omits the audio codecs. PCM and Opus never get a Path-A/Path-B verdict in
the section that is supposed to answer "would moq accept external codecs at all" per backend.
PCM's posture ("offer, expect declined, keep local") lives only in §3 item 6 and `3t` D2; the
maintainer's question is answered for video and silently narrowed for audio.
Resolve: add a one-line PCM/Opus row to §4 (in-tree offer, low interop value, expect PCM
declined) so the per-backend answer is complete.

### F4 (substantive) — 3u's `register_decoder` is called a "mirror" but the decode seam is not symmetric
Location: `3u-moq-changes.md` §2 point 2: "The decode side mirrors it with `register_decoder`."
The encode `Registration` sketch is `{ name, codecs: &[Codec], open: fn(&Config) -> ... }`. The
decode candidate table is not shaped the same: it carries `supports: fn(Codec) -> bool` (not a
`codecs` slice) and `open: fn(Codec, &Config) -> ...` (extra `Codec` arg), per
`decode/backend/mod.rs:78-85`. So a literal "mirror" would not compile; the decode
`Registration` needs a different field set. 3u glosses this. Given the doc's stated goal of
"concrete ... in moq's own vocabulary," the decode registration deserves its own sketch rather
than "mirrors it."
Resolve: sketch the decode `Registration` explicitly with `supports` and the `fn(Codec, &Config)`
opener.

### F5 (stylistic) — 3t tabulates codec traits exhaustively but handles two surfaces in prose only
Location: `3t-compare-traits-api.md`. The ask was "every trait and method ... in side-by-side
tables." 3t meets this for all codec-role traits, but two enumerated surfaces get prose instead
of tables: (a) the device-layer traits and the `Decoders` associated-type trait
(traits.rs:14-19) are listed in §1.1/§2.8 but never method-compared — justified, since moq has
no counterpart (§2.8 says so); (b) moq's track-facing `encode::Producer` / `decode::Consumer`
public methods are compared in §7 prose with line cites, not a signature table. Both are
defensible, but they are the only spots where 3t summarizes where the ask said enumerate.
Resolve: optional — a short signature table for Producer/Consumer in §7 would close the last gap.

### F6 (stylistic) — minor line-range imprecision in 3u
Location: `3u-moq-changes.md` §1 point 1 cites "`width`/`height`/`to_i420` (`frame.rs:38-74`)":
the `impl Frame` block is 38-75 but `to_i420` itself is 63-74; §2 cites the decode `supports`
field as part of `decode/backend/mod.rs:80-114` where the `Candidate` struct is 81-85. These are
loose ranges, not wrong facts, but in a doc that trades on exact cites they read as imprecise.
Resolve: tighten to the specific member lines.

---

## What was checked and found sound (no action)

- The public frame vocabulary is precise across 3u/3t/3z: the `Native` `#[non_exhaustive]` enum,
  the `DmaBuf`/`HardwareBuffer` additions to the `pub(crate)` `frame::Frame`, the on-demand
  `DmaBuf::export() -> OwnedFd` (correctly identified as strictly better than moq's
  store-the-resource enum), and `decode::Frame::native() -> Option<Native>` beside the verified
  `into_i420()` at `decode/mod.rs:94-101`. Variant sets agree between docs.
- Section 2's `Backend` trait draft is real, not hand-wavy: public trait with
  `encode(&Frame, timestamp, keyframe) -> Result<Vec<Packet>, Error>`, `Packet` type,
  `Registration` + `register_encoder`, and the correct observation that `Kind::Named(String)`
  needs no change (verified against `open()` at `encode/backend/mod.rs:106-134`). PTS-through-
  encode is correctly called out as the separate, unconditional change, symmetric with the
  decode side's already-present `Decoded { timestamp, frame }` (verified at `decode/backend/mod.rs:54-62`).
- Section 4 genuinely investigates moq's posture — vendored NVENC, openh264-from-source, trimmed
  cros-libva, ffmpeg removed, `pub(crate)` `Backend` (verified line 37 encode / 67 decode),
  `const` `Candidate` tables (verified 60-102) — and concludes there is no external seam today.
  The per-backend split (VAAPI/V4L2/AV1 in-tree needing only R1+PTS; Android as the Path-B case;
  renderer out-of-tree) is defensible and does not dodge, save the audio omission in F3.
- Render recommendation (Option B, out-of-tree crate) is consistent and identically reasoned
  across `3z` §4, `3u` §1b, `8-upstream` C13, and `10-summary`; reasoning on dependency weight,
  testability (per-vendor GPU CI), ownership, and the existence-proof argument is sound.
- The streaming-vs-one-shot analysis in `3t` §6 is well-evidenced (V4L2 M2M `encoder.rs` line
  cites, the "all five moq backends drain per call" claim matches the five encode candidates).
