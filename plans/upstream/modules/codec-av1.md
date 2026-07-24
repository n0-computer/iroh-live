# Software AV1 codec (rav1e encode, rav1d decode)

VERDICT: remove from iroh-live for now

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path. Software AV1 decodes to CPU I420 and feeds
> no zero-copy render input, so its removal regresses no held frame model. See
> ../zerocopy.md, section "Decode to render: verdict ours".

## What it is

iroh-live has the only software AV1 path on either side: a rav1e 0.8 encoder and a
rav1d decoder. moq has AV1 decode only through NVDEC, gated to 8-bit 4:2:0
non-monochrome, and no AV1 encode anywhere. The capability is genuinely
ours-only, but it is deferred upstream and dropped locally this series: rav1e is
too slow to carry as a live encoder now, and the rav1d decode dependency is a
git-fork pin moq's crates.io-only rule forbids. The local backend is ripped out
now with no moq replacement; the upstream contribution stays a parked plan for a
later round.

## iroh-live vs moq

Ours: `Av1Encoder` on rav1e 0.8 with a live-streaming configuration (speed preset
10, `low_latency = true`, `error_resilient = true`, bitrate rate control with a
quantizer floor, `codec/av1/encoder.rs:46-59`), a timestamp map surviving rav1e's
lookahead reordering (`:29,239-241`), and a full ISOBMFF codec-string parameter
set matched to our BT.601 conversion pipeline (`:148-178`, `config.rs:90-116`).
`Av1VideoDecoder` wraps rav1d through a safe shim (`codec/av1/rav1d_safe.rs`, 196
L) with `max_frame_delay = 1` for latency (`codec/av1/decoder.rs:46-48`) and
stride-checked plane conversion (`:95-98`). No `set_bitrate` (rav1e cannot retune
a live context cheaply).

Theirs: decode only, NVDEC only (#2178), gated to 8-bit 4:2:0 non-monochrome
(`is_supported_av1`, `moq:decode/decoder.rs:187-189`), and no AV1 encode anywhere.
Their public encode `Codec` enum is `{H264, H265}` only
(`moq:encode/encoder.rs:21-40`), and their own test pins that software AV1 decode
fails to open (`av1_is_supported_by_hardware_only`). So moq has no software AV1
either, encode or decode; our removal leaves both sides without a software AV1
path this series.

## What to do

Remove locally now (local rip-out, not gated on any upstream contribution).

- Removal: delete `rusty-codecs/src/codec/av1/` (936 LOC:
  `encoder.rs` ~369, `decoder.rs` ~365, `rav1d_safe.rs` 196). This is the one
  once-held backend that is deleted immediately with the independent local
  deletions rather than gated on an upstream release, because AV1 is not offered
  upstream this series.
- Sequencing (the codec removal sequencing, step 7): AV1 decodes to CPU I420 and
  feeds no zero-copy render input, so it needs no platform-atomic hold. The
  proof-before-deletion rule (the proof-before-deletion rule) still applies: a local end-to-end test
  must pass without the AV1 backend before the module is deleted.
- Nothing to port upstream this series. The upstream leaf
  (`../codec/av1-software.md` in the old tree) stays deferred, retained as the
  reference for a later round, not deleted. It carries the B2-signature encoder,
  the lookahead timestamp map, the stride-checked I420 extraction, and the
  `hang::catalog::VideoConfig` output, ready for re-entry.

Why deferred:

- rav1e is roughly 4x slower per resolution step at speed 10, usable at
  conference resolutions but not at 1080p60 on small cores.
- The rav1d dependency is unresolved. Our decode is a git-fork pin on
  `memorysafety/rav1d` with `bitdepth_8/16` and `asm` (`rusty-codecs/Cargo.toml`),
  which moq forbids (crates.io only, release-plz). The published crates.io
  alternative `dav1d-rs` links a C libdav1d system dependency, breaking the "no
  system library, builds everywhere" spirit. rav1e 0.8 itself is crates.io and
  unproblematic; the decoder is the blocker.

Re-entry condition: a use case needs AV1 and the rav1d dependency becomes
resolvable, that is a crates.io `rav1d` release, an accepted git exception, or an
accepted `dav1d-rs` C dependency. AV1 can be re-added later on those terms.

## Tests

The proof-before-deletion rule (the proof-before-deletion rule) requires a local end-to-end test to
pass without the AV1 backend before the module is deleted. No hardware gate and no
platform-atomic hold apply, since AV1 is software and off every zero-copy path.
The deferred upstream leaf carries a software round-trip test for the eventual
re-entry (encode through rav1e, decode through rav1d, assert monotonic
timestamps), which runs unconditionally in CI when revived; that test is not part
of this series.

## Evidence

- ../comparison/codecs.md, section 3 (AV1), including the git-fork dependency risk
  and the encoder CPU-cost caveat at speed 10.
- ../comparison/codecs.md, section 10 (verdict table, AV1 encode/decode rows) and
  the "top upstream candidates" note deferring AV1 on the rav1d pin.
- ../comparison/maps/rusty-codecs.md, section 2 (av1 row) and section 5 (the
  rav1d git memorysafety fork).

## Coordination

- No release gate and no upstream leaf this series: this is a local drop, deleted
  with the independent local deletions.
- Proof-before-deletion (the proof-before-deletion rule) governs the local deletion.
- The deferred upstream leaf, if revived, needs B2 (PTS through encode,
  ../base/B2-pts-through-encode.md) for the rav1e lookahead timestamp map, and
  coordination point 4 (rav1d fork resolution) is a hard gating prerequisite
  before any AV1 PR opens.
</content>
