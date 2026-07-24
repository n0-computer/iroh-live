# Upstream Plan: the PR program that delivers iroh-live's contributions to moq

> Campaign: upstream | Kind: analysis | Preserved context; superseded by
> ../0-overview.md where they disagree. Read ../0-overview.md first.

Status: planning artifact, rewritten 2026-07-22 after moq merged its long-lived
rewrite branch into main on 2026-07-21. There is no dev line to target: every
upstream target named here is a moq main crate, working tree
`/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`. This document is the PR program:
the waves, sizes, sequencing, velocity gate, and engagement strategy that deliver
the concrete moq-side API changes plus our backend, codec, and render
contributions. It does not re-draft those API changes. The concrete moq-side
design (the public frame vocabulary and `DmaBuf`/`HardwareBuffer` variants,
PTS-through-encode, the public and registerable `Backend` trait, native-handle
decode export, the render-crate home, and the external-versus-in-tree decision
per backend) lives in [3u-moq-changes.md](3u-moq-changes.md); this document
cross-references 3u for the API shape and focuses on the contribution catalog,
wave sequencing, size estimates, the velocity gate, and how we engage.

It builds on the verdicts in [3-compare-codecs.md](3-compare-codecs.md)
(per-codec verdicts, top upstream candidates), [3z-compare-zerocopy.md](3z-compare-zerocopy.md)
(requirements U1-U4), [3t-compare-traits-api.md](3t-compare-traits-api.md)
(decisions D1-D12), [4-compare-capture.md](4-compare-capture.md),
[5-compare-pubsub.md](5-compare-pubsub.md), [6-compare-audio.md](6-compare-audio.md),
and [9-room-layer.md](9-room-layer.md). All LOC figures for our code are from
[1-code-map.md](1-code-map.md); all moq citations are `3a3e0ea8`.

Working assumption: we have a good relationship with the moq maintainers, and
work that arrives in their shape has a realistic path to acceptance. A second,
equally binding assumption is maintainer bandwidth: this plan proposes roughly
10,000 LOC over about 20 PRs into a project that vendors everything, reshapes
contributions in review, and has just absorbed a large rewrite into main. Our
authoring capacity is not the constraint; their review capacity is, and no
merged PR from us yet evidences either assumption. Section 3's velocity gate
exists for exactly this reason. The cut plan ([7-cut-plan.md](7-cut-plan.md))
states what we delete once each contribution lands; its stage 2 keys on wave 2,
its stage 3 held cuts on waves 2-3, and its stage 4 on wave 4. Because the
enabler stack is now on moq main, the cuts our contributions unlock ride the
same single release bump the whole cut plan keys on, not a separate branch
release.

Plan freshness: the analysis is pinned to moq main `3a3e0ea8` (2026-07-21).
Post-merge main is still settling, so signature-level drift by release is
expected and structural drift less so. Before any wave starts, re-diff the pin
against the then-current main or release, re-validate the enabler register
(2-moq-inventory summary table 2, now a bump checklist rather than a branch
gamble), and re-confirm the per-item design against 3u. 7-cut-plan.md risk R-a
states the reduced slip floor and the main-tracking hedge branch.

---

## 1. Upstreaming ground rules

Derived from the observed moq style at `3a3e0ea8` and the commit log. Our
contributions must arrive in their shape, not ours. The evidence for each rule
and the per-backend in-tree-versus-external recommendation are worked out in
3u sections 1 through 4; this is the summary the PR program keys on.

1. **In-tree vendored backends behind default-on, opt-out features.** They
   vendored the NVENC bindings as `moq-nvenc`, trimmed cros-libva into the
   external-but-theirs `moq-vaapi` crate, and build openh264 from source. A new
   backend PR ships in-tree (or into moq-vaapi), feature-gated, default-on where
   the dependency cost allows, and never as an out-of-tree plugin, unless the
   backend is one moq cannot test (Android), which is the one case for the
   registration path of 3u section 2. Their `Backend` traits are deliberately
   `pub(crate)` (`rs/moq-video/src/encode/backend/mod.rs:37`); we ask to open
   them only for the external-backend path (3u section 4, Path B).
2. **dlopen for system libraries, link nothing that can fail to load.** Any
   Linux backend we contribute must resolve libva, libdrm, and V4L2 entry points
   at runtime or degrade cleanly, and must build on hosts without the hardware,
   the same posture as moq-nvenc's compile-everywhere stub (3u section 4).
3. **Minimal dependencies, crates.io only.** release-plz owns their versions;
   there are no git dependencies in `rs/`. Our rav1d git-fork pin and our cpal
   git pin are unacceptable upstream as-is (see C6). Heavy crates need
   justification per dependency.
4. **No ffmpeg.** The merge removed it entirely; nothing we send may reintroduce
   it, including in tests (3u section 4).
5. **WebCodecs-aligned vocabulary and semver discipline.** Public configs are
   `#[non_exhaustive]`, no backend type appears in a public API, audio formats
   mirror `AudioData.format`, timestamps are `moq_net::Timestamp` (never
   `Duration` at boundaries), catalog types come from hang. Our contributions
   adopt their error types and their tabs (3u section 3).
6. **Honest capability contracts.** `set_bitrate` must succeed or return
   `Error::BitrateUnsupported`; a silent no-op is a rejected pattern. Every
   backend arrives with per-frame forced-IDR support and, where hardware exists
   to test on, a hardware round-trip test in the style of their VideoToolbox and
   NVENC tests.
7. **Breaking-change discipline.** Conventional commits with crate scope and `!`
   markers. Now that the rewrite is merged, all of this targets moq main, and
   our contributions land for us with the next ordinary release (2-moq-inventory
   summary table 2). The only genuinely breaking item in the program is the
   public `Backend` trait (C5's Path B, 3u section 2, change 7); everything else
   is additive.

---

## 2. The contribution catalog

Priority-ordered. Sizes are estimated adapted diff LOC (our implementation size
minus our trait glue, plus their idioms and tests), with S under ~150, M
~150-600, L ~600-1,500, XL above that. Each item names its 3u change and its 3z
requirement rather than restating the API; 3u carries the sketches.

### C1. VAAPI decode backend and VAAPI encode replacement

**What it is.** Our full cros-codecs VAAPI stack, hardware-validated on Intel
MTL: a 1,188-line stateless H.264 decoder producing GPU-resident DMA-BUF frames
with cached PRIME export, a 1,533-line encoder with zero-copy DMA-BUF surface
input, VPP hardware scaling, and VPP color conversion, 3,257 LOC total across 4
files (`rusty-codecs/src/codec/vaapi/`).

**Why moq wants it.** This is the most lopsided comparison in the codec study,
in our favor (3-compare-codecs section 1). Their VAAPI encoder is a 111-line
CPU-NV12-only adapter marked "NOT YET VALIDATED ON HARDWARE", cannot retune, and
hard-links libva; they have no VAAPI decode at all, so every Intel and AMD Linux
machine decodes H.264 in software. Upstreaming ours fills moq's largest platform
gap and replaces their least-loved backend with a validated one.

**Target and API.** moq-vaapi grows surface export, VPP, decode FFI, and dlopen
(closing #1837); moq-video gains a `decode/backend/vaapi.rs` and a rewritten
`encode/backend/vaapi.rs`. This is 3u change 4 (decode export) and change 5
(encoder GPU input plus honest `set_bitrate`), realizing 3z requirements U2 and
U3; the encoder gains the honest bitrate contract and per-frame forced IDR of
3u section 3. Recommendation: in-tree, Path A (3u section 4); the dependency
growth lands in their moq-vaapi vendor, matching their posture. Offer the
vendor-merge and the cros-codecs-dependency alternative and let them choose.

**Prerequisites.** C2 (the `Native`/`DmaBuf` vocabulary, 3u change 1) for the
GPU output and input paths; C3 (PTS through encode) should land first so all
backends share one signature. moq-vaapi growth gates both backends and is the
largest single piece of upstream work (3z section 5, ordering note).

**Estimated PR size.** Three PRs. moq-vaapi growth: L-XL (~1,000-1,400).
Decode backend: L (~900 adapted from 1,188). Encode replacement: L (~800 adapted
from 1,533). Total ~2,700-3,100.

### C2. Public platform-handle frame vocabulary (U1 / D1 / 3u change 1)

**What it is.** A public equivalent of our `NativeFrameHandle`/`DmaBufInfo`: fd,
DRM format, modifier, coded and display dimensions, and per-plane offset and
pitch for DMA-BUF; a retained CVPixelBuffer accessor on macOS; AHardwareBuffer on
Android. Small in LOC, keystone in effect. Every other GPU contribution (C1, C5,
C11, C13) depends on it.

**Why moq wants it, target, and API.** Their `frame::Frame` is `pub(crate)` with
no public GPU escape except `into_i420()`, so their own GPU work is invisible to
any renderer and no zero-copy decode output can reach an application (3z sections
1 and 2b). The full design, including the closed `#[non_exhaustive]` `Native`
enum, the on-demand `DmaBuf::export()`, the `decode::Frame::native()` accessor,
and the `moq-frame` crate versus module question, is 3u section 1 and change 1
plus change 3. This is D1 with recommendation (iii) falling back to (ii)
(3t section 8).

**Prerequisites.** None. This goes first, or in the same PR series as the VAAPI
decode backend that motivates it.

**Estimated PR size.** M-L. Minimal variant ~300 LOC; full moq-frame crate
~700-1,000, mostly mechanical (3u change 1).

### C3. PTS threading through Backend::encode (D3 / 3u change 2)

**What it is.** One signature change on their private encode trait, adding a
`timestamp: Timestamp` argument and returning `Vec<Packet>` where `Packet {
payload, timestamp }`, with `Producer::publish` using the per-packet timestamps
instead of the call-site PTS. The full before/after is 3u section 2 change 1 and
3t section 6.

**Why moq wants it.** Their current shape is correct only for zero-frame-delay
encoders; pipelined hardware queues (V4L2 M2M, Android MediaCodec) cannot
honestly implement it. The change mirrors what their decode side already does
and changes no existing backend's observable behavior (3t section 6).

**Target, prerequisites, size.** moq-video, private trait plus the backends plus
Producer; not a breaking change for downstreams. No prerequisites; precondition
for C4 and C5. S-M (~150 LOC across 7 files, mechanical).

### C4. V4L2 M2M encode and decode backend

**What it is.** Our Linux ARM SoC H.264 pair: encoder over raw V4L2 M2M ioctls
with resolution-derived level selection, SPS/PPS repeat on IDR, and the
driver-stride and 16-aligned-height handling debugged on real hardware
(commit 625c16f); decoder via v4l2r. 1,856 LOC across 3 files.

**Why moq wants it and target.** moq has no V4L2 codec backend; this is the
entire Raspberry Pi and ARM SoC class (3-compare-codecs section 1). In-tree,
Path A, behind a `v4l2` feature (3u section 4). The encoder implements honest
`set_bitrate` and per-frame IDR; the M2M queue model is the motivating case for
C3, and PTS rides the V4L2 buffer timestamp (3u section 3).

**Prerequisites.** C3 (hard requirement). Decoder wants C2 only if we later add
DMA-BUF output; the initial PR can be CPU-out.

**Estimated PR size.** L (~1,400 adapted from 1,856), plausibly split into
encode and decode PRs. CI story: no Pi in their runners, answered with a
compile-only gate plus our hardware validation.

### C5. Android MediaCodec backend and AHardwareBuffer handling

**What it is.** Our MediaCodec encoder plus two decoders (ByteBuffer CPU path
and zero-copy ImageReader HardwareBuffer path), with codec-reset error recovery
(1,528 LOC across 6 files) and the `HardwareBufferInfo` handle.

**Why moq wants it and target.** moq has zero Android support; moq-ffi exists
precisely to serve mobile embedders. Android is the strongest case for the
external-backend path, because moq cannot test it in CI and it drags in an ndk
dependency surface foreign to their focus (3u section 4). Propose in-tree first
(a whole platform contributed complete); fall back to the public registerable
`Backend` trait and `register_decoder` of 3u section 2 (change 7) with Android
as the named external backend if they decline to carry code they cannot test.
This is the one item whose fallback needs the only breaking moq change in the
program, and the D2 decision (3t section 8).

**Prerequisites.** C2 (HardwareBuffer variant), C3 (dequeue-based codec), and
the D2 decision if in-tree is declined.

**Estimated PR size.** L (~1,300 adapted from 1,528 plus the frame variant). If
the fallback is taken, add the registration API at M (~250, both sides, 3u
change 7).

### C6. Software AV1: rav1e encode and rav1d decode

**What it is.** Our `Av1Encoder` (rav1e 0.8, live-streaming tuning, timestamp
map surviving lookahead reordering) and `Av1VideoDecoder` (rav1d via a 196-line
safe shim), 936 LOC across 4 files.

**Why moq wants it and target.** Their AV1 story is NVDEC-decode-only and their
public encode `Codec` enum is `{H264, H265}`; ours is the only AV1 encode in
either stack and the software decode completes their hardware-then-software
story. In-tree, Path A, once the dependency pin resolves (3u section 4):
`Codec::Av1` in the public enum (non-breaking), software candidates behind an
`av1` feature.

**Prerequisites.** The rav1d git-fork pin must resolve first (ground rule 3): a
released crates.io rav1d, a scoped-name publish of the fork, or a vendored safe
wrapper. rav1e 0.8 is crates.io and unproblematic. No dependency on C1-C3.

**Estimated PR size.** M-L (~750 adapted from 936). The honest cost belongs in
the PR: rav1e at speed 10 is usable at conference resolutions, not 1080p60 on
small cores.

### C7. Opus improvements

**What it is.** Four small items (6-compare-audio sections 1.1, 1.5, 7; 3u
section 3 item 5): runtime `set_bitrate` on their encoder (also the precondition
for extending their rate control to audio); lookahead-derived pre-skip (their
OpusHead hardcodes pre-skip 0, a genuine correctness fix touching moq-mux and
moq-audio); a channel-remix policy for their decode Consumer; and an FEC/PLC
`decode_lost(duration)` API reservation for phase 3c.

**Target, prerequisites, size.** moq-audio (items 1, 3, 4) and moq-mux (item 2).
None. S each; total ~200 across 3-4 small PRs. These are relationship-building
PRs that can go first.

### C8. Subscriber-side adaptive rendition selection

**What it is.** Our pure ABR policy (`rank_renditions`, `AdaptiveConfig`,
`AdaptationTimers`, `evaluate`, `should_abort_probe`, ~340 LOC plus tests) plus
the switching driver from `adaptation_task_v2` (parallel staging consumer,
decoder swap on first frame, probe lifecycle, failure cooldown).

**Why moq wants it and target.** moq Rust has no subscriber-side rendition
selection: libmoq subscribes by catalog index with a TODO, moq-cli takes static
flags, and the only ABR anywhere is the JS heuristic (5-compare-pubsub section
5). moq-transcode mints the ladder; nothing in Rust picks the rung. Policy home:
moq-mux catalog module next to `Select`, or a small dedicated crate; driver:
moq-video `decode` module as a switcher over `decode::Consumer`.

**Build on what already landed.** The publish side of this loop is now on moq
main: moq-mux's per-rendition `Metrics` measures actual jitter and bitrate and
writes them into the catalog fields the ABR selects on
(`rs/moq-mux/src/container/jitter.rs`, `record_frame`/`finish_group`; 5-compare
sections 2 and 5). So C8 does not add rendition metrics from scratch; it selects
over the bitrate and jitter that `Metrics` already populates, rather than the
static preset values our current broadcasts assert. Bandwidth comes from
`moq_net::bandwidth::Consumer` via `recv_bandwidth`, also on main; loss stays an
optional injected signal since moq-net exposes none (5-compare section 5). With
moq-transcode's 1:1 group mirroring plus `fetch_group`, the upstream switcher can
backfill the current group on the target rung, which our local switcher cannot.

**Prerequisites.** The merged-stack release (decode::Consumer, recv_bandwidth,
Metrics). C10 `set_latency` for the companion latency policy, not for switching.
C2/C3 unrelated.

**Estimated PR size.** M for the policy (~400 with tests, near-verbatim), M-L for
the driver (~300-450 rebuilt over decode::Consumer). Two PRs, policy first.

### C9. Playout clock and A/V sync

**What it is.** Our `Sync` playout clock (`moq-media/src/sync.rs`, 420 LOC), a
Rust port of their own JS `sync.ts`: tightening-only reference offset,
`latency = max(audio, video) + jitter`, blocking `wait(pts)` with re-evaluation
on retune, audio as pacing master; plus the `PlaybackPolicy` surface (92 LOC).

**Why moq wants it and target.** No playout clock exists in moq Rust; the only
sync implementation upstream is the JS one we ported (5-compare-pubsub section
6). moq-audio's own docs defer `latency_min` jitter padding as acknowledged
missing. Home: moq-mux beside `container::Consumer` (the clock is codec-free and
shared across audio and video); decode consumers optionally gate on it.

**Build on what already landed.** The producing half of this loop is on moq
main: the catalog `jitter` field is populated per rendition by moq-mux `Metrics`
(5-compare sections 4 and 6), and JS `sync.ts` already reads it. What is missing
is a Rust consumer that reads it. So C9 is not "add measured jitter", it is
"port the clock and have it read the catalog `jitter` that `Metrics` now writes",
which is strictly less than the round-1 framing assumed. Runtime latency retune
wants C10.

**Prerequisites, size.** The merged-stack release; independent of C8. M (~450
adapted from 512, with the moq-js provenance cited).

### C10. Runtime Consumer::set_latency

**Verified: already on moq main.** `container::Consumer::set_latency` exists at
`consumer.rs:478-480` (5-compare-pubsub section 3). This is "adopt, not
contribute", with one residual S-size ask: surface `set_latency` through
`moq_video::decode::Consumer` and `moq_audio::decode::Consumer`, whose
`Config.latency_max` is currently forwarded only at construction. ~30 LOC. That
surfacing is what our `PlayoutMode::Auto` retuning (and C9) need to avoid
rebuild-on-change.

### C11. PipeWire DMA-BUF capture delivery and Linux device enumeration

**What it is.** Two capture items (4-compare-capture sections 2 and 5; 3z
requirement U3). DMA-BUF delivery in PipeWire capture: our backend negotiates
`SPA_DATA_DmaBuf` and delivers GPU frames with fd, DRM fourcc, modifier, and
per-plane layout; theirs negotiates shm only. Linux enumeration: their
`cameras()`/`displays()` return `Unsupported` off macOS; our V4L2 scan
enumerates `/dev/video0..63` with per-device FourCC lists.

**Why moq wants it and target.** With C1's VAAPI encoder accepting
`Frame::DmaBuf`, DMA-BUF capture completes a zero-copy Linux capture-to-encode
pipeline neither part of moq has (3z section 2a). Extend moq-video
`capture/pipewire.rs` (their restore-token replay, static-screen re-pacing, and
open-per-demand lifecycle stay); enumeration on `capture/mod.rs` and
`capture/v4l2.rs`. This is part of 3u change 5 (encoder input) plus the capture
half of U3.

**Prerequisites, size.** C2 for delivery; enumeration none. M (~450) for DMA-BUF
delivery; S-M (~250) for enumeration. Separate PRs.

### C12. Pre-encoded source concept (libcamera H.264)

**What it is.** Our `LibcameraH264Source`, the only backend on either side that
publishes device-encoded H.264 (`rpicam-vid` with the Pi ISP-to-encoder DMABUF
path internal to the device), parsing Annex-B into access units with avcC
extraction (522 LOC).

**Why moq wants it and target.** On a Pi Zero 2 this is the difference between
working and not; moq's encoder-always-runs model has no answer for that hardware
class. Their `Producer::publish(Vec<Bytes>, Timestamp)` already accepts external
Annex-B without an `Encoder` (3u section 3 item 7), so the concept fits: a
`publish_preencoded` sibling of `publish_capture` whose source yields packets
instead of frames.

**Prerequisites, size.** Conceptual buy-in that a source may bypass the encoder.
M (~500 adapted). The Pi backend may stay in our tree as the first out-of-tree
user of the concept; offer both.

### C13. Render integration as an out-of-tree crate (U4 / 3u section 1b, Option B)

**What it is.** Our importers and renderer: `WgpuVideoRenderer` with per-frame
path selection and failure fallback (799 LOC), Vulkan DMA-BUF import including
the `VppRetiler` Y_TILED-to-CCS re-tile (1,452 LOC), GLES and EGLImage paths
(938 LOC), and Metal CVPixelBuffer import (274 LOC); ~3,500 LOC total.

**A first-class contribution, not a non-deliverable.** The maintainer intends to
upstream the render stack rather than tolerate it living downstream (3z section
4). The decision, worked out in 3z section 4 and 3u section 1b, is Option B: a
published, out-of-tree `moq-video-render` crate under our maintenance, consuming
moq's public `decode::Frame::native()` handles, selecting a path per frame, and
falling back to `into_i420()` when a zero-copy path fails, exactly as our
renderer does today over our own handle. moq-video stays render-free, the heavy
graphics dependencies (wgpu, ash, glow, objc2-metal) and the Intel-specific VPP
re-tile stay out of moq's workspace and CI, and the crate is the working proof
that the public handle vocabulary of C2 is sufficient for a third party, which
is the strongest argument for C2 in the first place.

**Prerequisite: the public frame vocabulary.** C13 depends entirely on C2 (U1)
plus the per-decoder export of C1 and 3u change 6, so that `native()` has
something to return on Linux and Apple. Until C2 lands the crate has nothing to
import; the render-crate deliverable is therefore scheduled after C2, and the
render argument is what the C2 RFC leads with.

**Upstream diff: zero, but a real downstream deliverable.** No moq source
changes beyond the public `native()` accessor of C2 and 3u change 3; the crate
itself is ours to publish (~3,500 LOC ported from `render.rs`,
`render/dmabuf_import.rs`, `render/gles*.rs`, and `render/metal_import.rs`). It
is one paragraph in the C2 RFC and a published crate in the moq ecosystem, not a
PR into moq's tree.

### C14. Smaller items

- **SPS VUI patcher.** Our exp-golomb rewriter stripping DPB reordering latency
  (`codec/h264/sps.rs`, 586 LOC, currently dead code). Offer as an optional pass
  in `moq_mux::codec::h264`; do not push. S-M (~300 adapted) if wanted.
- **PCM codec offer.** `Codec::Pcm` slots into moq-audio's `#[non_exhaustive]`
  enum in ~100 lines plus our impl (~250 adapted), but the hang catalog has no
  PCM codec variant, so interop value is nil (3u section 3 item 6). Mention it,
  expect and accept a decline; keep local for tests.
- **Bounded-buffer capture fix.** Their mic path forwards realtime callback
  buffers over an unbounded tokio mpsc with a per-callback allocation on the
  realtime thread (6-compare-audio 3.3). A bounded channel plus preallocated
  buffer PR is S (~60) and a clear win. Good early goodwill PR.
- **Dynamic-handler race report.** Our workaround registers `producer.dynamic()`
  synchronously before spawning the task because `subscribe_track` otherwise
  returns NotFound until the task runs (5-compare-pubsub section 2). File as a
  moq-net issue with the repro; no code required.
- **moq-vaapi hardware validation report.** We have run VAAPI encode and decode
  paths on Intel MTL hardware, including the exact failure modes their
  unvalidated backend would hit (Y_TILED modifier incompatibility, VPP re-tile
  requirement, `vaSyncSurface`-before-export). Write up as an issue against
  moq-vaapi/#1837 with measurements. Immediate, zero-code value and the natural
  opener for the C1 conversation. Since neither side runs this hardware in CI,
  the report must carry reproducible scripts and exact environment versions so
  the results can be re-run without us; the same applies to C1 and C4.

---

## 3. Sequencing and dependency graph

Dependencies, stated as edges (C = catalog item here, U = 3z requirement, the
number in parentheses = 3u change):

```
C2/U1 (frame vocabulary, 3u#1) --> C1 (VAAPI dec/enc GPU paths, 3u#4,#5)
                              --> C5 (Android HardwareBuffer)
                              --> C11 (PipeWire DMA-BUF delivery)
                              --> C13 (render crate, out-of-tree, 3u#1b)
C3 (PTS in encode, 3u#2)      --> C4 (V4L2)  --and--> C5 (Android)
rav1d pin resolution          --> C6 (AV1)
merged-stack release + bump   --> C8 (adaptive driver), C9 (clock), C10 (adopt)
C10 surfacing                 --> C9 runtime retune (soft)
moq-vaapi growth              --> C1 backends
```

C6, C7, C12, and C14 have no dependency on the API decisions and can proceed any
time; C8's policy half is also independent (sans-IO), only its driver needs the
release. C8 and C9 build on the moq-mux `Metrics` bitrate/jitter and the
`recv_bandwidth` estimate that are already on main, so those are inputs they
consume, not work they add.

**Wave 1 (now; discussion plus small PRs).** The D1-D3 RFC issue proposing C2
(3u change 1) and C3 (3u change 2) with the 3u sketches, framed with the render
argument (C13) as the motivating consumer; the moq-vaapi hardware validation
report (C14) attached as evidence; the dynamic-handler race report (C14); the
goodwill fixes: Opus pre-skip (C7.2), Opus set_bitrate (C7.1), and the bounded
capture channel (C14). Total code: ~140 LOC across 3 small PRs, plus two issues
and one RFC; C3 (~150 LOC) joins the tail of this wave if the RFC converges
quickly.

**Velocity gate between waves 1 and 2.** Wave 2 starts only after the wave-1 PRs
have demonstrated actual review turnaround (merged, or reviewed with a
predictable cadence). If wave-1 review stalls, do not open the full wave-2
series; fall back to a smaller-first pilot: C2 in its minimal variant plus the
C1b VAAPI decode backend only (with the decode-side slice of C1a it needs),
roughly 1,200-1,500 LOC of exposure that carries most of the strategic
information and the bulk of the Linux-story value. The remaining PRs are
committed only on the pilot's measured outcome (review latency, reshape depth,
maintainer engagement).

**Wave 2 (after D1/D3 land).** C2 implementation (if we are asked to carry it),
C3, then the C1 series: moq-vaapi growth, VAAPI decode, VAAPI encode
replacement; C4 V4L2 in parallel once C3 merges. Total: ~4,600 LOC across ~6 PRs.
This wave delivers moq's Linux story and is the center of gravity of the plan.

**Wave 3.** C6 AV1 (once the rav1d pin resolves; can start earlier), C5 Android
(or the registration fallback), C11 PipeWire DMA-BUF plus Linux enumeration, C12
pre-encoded source. Total: ~3,250 LOC across ~5 PRs (C6 ~750, C5 ~1,300, C11a
~450, C11b ~250, C12 ~500).

**Wave 4 (after the merged-stack release and bump).** C8 adaptive (policy PR,
then driver PR, building on the on-main `Metrics` and `recv_bandwidth`), C9
playout clock (reading the on-main catalog `jitter`), C10 set_latency surfacing.
Total: ~1,300 LOC across 4 PRs. These are the "moq gains a Rust client story"
wave and pair naturally with their JS parity interests.

**Wave 5 (nice-to-haves, opportunistic).** C14 SPS VUI patcher if wanted, PCM
offer, C7.3 remix policy, C7.4 PLC API reservation follow-through, and the C13
`moq-video-render` crate publication once C2 has landed and the render stack is
ported onto the public handles. C13 is a downstream deliverable rather than a
moq PR, so it rides this wave without competing for moq review bandwidth.

Tie-in to the cut plan: each wave unlocks deletions on our side (wave 2:
rusty-codecs VAAPI and V4L2 modules once we consume moq-video; wave 3: AV1,
Android, and parts of rusty-capture; wave 4: adaptive.rs and sync.rs shrink to
thin consumers; wave 5: render.rs and render/ leave the core crate for the
out-of-tree crate). The exact deletion schedule is 7-cut-plan.md section 3, whose
merged dependency table joins these waves with the cut stages, U1-U4, and the D
decisions in one vocabulary; there, "lands upstream" is the precondition for the
corresponding cut, never the reverse.

---

## 4. Size summary table

| # | Contribution | Target crate | 3u change / 3z req | Est. PR size (LOC, files) | Prerequisites | Wave |
|---|---|---|---|---|---|---|
| C14e | moq-vaapi validation report | issue (moq-vaapi) | - | 0 (issue) | none | 1 |
| C14d | dynamic-handler race report | issue (moq-net) | - | 0 (issue) | none | 1 |
| C2 | Frame vocabulary RFC + impl | moq-frame or moq-video | 3u#1, #3 / U1 | 300 minimal, 700-1,000 crate; 5-15 files | none (RFC first) | 1-2 |
| C3 | PTS through encode | moq-video | 3u#2 | ~150, 7 files | none | 1-2 |
| C7.1 | Opus runtime set_bitrate | moq-audio | 3u#10 | ~30, 1 file | none | 1 |
| C7.2 | Opus pre-skip fix | moq-mux + moq-audio | 3u#10 | ~50, 2 files | none | 1 |
| C14c | Bounded capture channel | moq-audio | - | ~60, 1 file | none | 1 |
| C1a | moq-vaapi export + VPP + decode FFI, dlopen | moq-vaapi | 3u#4, #5 / U2, U3 | 1,000-1,400 | direction agreed | 2 |
| C1b | VAAPI decode backend | moq-video | 3u#4 / U2 | ~900, 3-4 files | C1a, C2 | 2 |
| C1c | VAAPI encode replacement | moq-video | 3u#5 / U3 | ~800, 2-3 files | C1a, C2 | 2 |
| C4 | V4L2 M2M enc + dec | moq-video | 3u#2 | ~1,400, 4-6 files (2 PRs) | C3 | 2 |
| C6 | Software AV1 (rav1e + rav1d) | moq-video | 3u#4 (in-tree) | ~750, 4-5 files | rav1d pin resolved | 3 |
| C5 | Android MediaCodec + AHB | moq-video | 3u#7 (fallback) | ~1,300, 6-8 files (+~250 if registration) | C2, C3, D2 decision | 3 |
| C11a | PipeWire DMA-BUF delivery | moq-video | 3u#5 / U3 | ~450, 2 files | C2 | 3 |
| C11b | Linux device enumeration | moq-video | - | ~250, 2 files | none | 3 |
| C12 | Pre-encoded source concept | moq-video | 3u#12 | ~500, 3 files | concept buy-in | 3 |
| C8a | ABR policy | moq-mux (or new crate) | - | ~400, 2 files | release; on-main Metrics | 4 |
| C8b | Switching driver | moq-video | - | ~300-450, 2-3 files | C8a, release | 4 |
| C9 | Playout clock | moq-mux | - | ~450, 2-3 files | release; on-main catalog jitter | 4 |
| C10 | set_latency surfacing | moq-video, moq-audio | - | ~30, 2 files | release (core on main) | 4 |
| C7.3/4 | Opus remix, PLC reservation | moq-audio | 3u#10 | ~120, 2 files | none | 5 |
| C14a | SPS VUI patcher | moq-mux | - | ~300, 1 file | if wanted | 5 |
| C14b | PCM codec offer | moq-audio | 3u#11 | ~250, 3 files | catalog question | 5 |
| C13 | Render integration (out-of-tree crate) | moq-video-render (ours) | 3u#1b, #8 / U4 | 0 upstream, ~3,500 downstream | C2 | 5 |

Total upstream diff across all waves: roughly 9,500-10,500 LOC over about 20 PRs,
of which the VAAPI series plus V4L2 (wave 2) is nearly half; this figure is
essentially unchanged by the main merge, because C10 was already done and C8/C9
build on rather than add the metrics inputs. Separately, C13 is a ~3,500 LOC
downstream deliverable (the `moq-video-render` crate) with zero moq diff, moved
in this rewrite from a non-deliverable to a first-class published contribution
gated on C2. These are gross authoring estimates: they exclude review reshaping,
and the deletions they unlock on our side are themselves gross of the bridge and
rework accounting in 7-cut-plan.md section 2.

---

## 5. Engagement strategy

**How to open.** One RFC-style issue, not a code PR: present D1 (public frame
vocabulary, 3u change 1) and D3 (PTS through encode, 3u change 2) with the
concrete sketches from 3u, framed as "here is what we have running in production
on the platforms you do not cover, here is the minimal API it needs from you, and
here is the list of backends and the render crate we will contribute once it
exists". The render crate (C13) is the lead argument for D1: a third-party
renderer working purely over the public handles is the proof the vocabulary is
sufficient. Attach the moq-vaapi hardware validation findings (C14e) as
immediate, zero-ask value. File the race report separately. Land the wave-1
goodwill PRs (pre-skip, set_bitrate, bounded channel) while the RFC is under
discussion; they are small, obviously correct, and calibrate both sides on
review style before anything large is in flight.

**Why RFC-first rather than code-first.** D1 and D3 gate every backend PR
downstream, so delaying the RFC delays the whole program; opening it now is the
critical-path move. Wave 1 deliberately pairs the RFC with small, high-value
goodwill fixes so that merged code, not the RFC, is the maintainers' first review
experience of us. The C6 AV1 offer is cited in the RFC as motivation, evidence
that we bring complete capabilities rather than API asks, without shipping its
code first.

**Sequence code after decisions.** No backend PR goes up before D1 and D3 are
decided; a 900-line VAAPI decode PR that forces the frame-model discussion in its
review thread is the failure mode to avoid. The one exception is C6 AV1 decode,
which touches neither decision and can serve as the first medium-sized PR to
establish trust.

**What we accept in return.** Their vocabulary everywhere (`moq_net::Timestamp`,
hang catalog types, their Error enums, tabs); in-tree vendoring of what we
contribute, including giving up our own release cadence on that code; their
review pace, which means our cut plan keys on upstream merges rather than
calendar dates; and their right to reshape our code in review (the contract we
care about is capability, not form). The one place we hold our own cadence is the
render crate (C13), which stays ours out-of-tree by the Option B decision, and
which therefore does not compete for their review or CI.

**Fallback per item if declined.**

- C2 declined entirely: everything GPU-shaped stays local. We keep our
  `NativeFrameHandle` model and our backends behind our own traits, and the
  alignment is limited to the CPU-path adoptions from the cut plan. This is the
  worst case and costs moq more than us.
- C2 lands but a given backend is declined (Android is the plausible case): the
  backend stays in our tree consuming the public frame vocabulary, and we raise
  the registration API (3u section 2, change 7) with that backend as the
  motivating out-of-tree user.
- C1 vendor-merge declined but cros-codecs dependency accepted: fine, smaller for
  us.
- C4/C12 declined on maintainability grounds (untestable hardware): keep local
  behind our traits; offer a compile-only stub plus our hardware CI results.
- C6 blocked on rav1d: ship encode-only (rav1e is crates.io), keep decode local.
- C8/C9 declined or deferred: keep adaptive.rs and sync.rs local; they already
  work against the consumer APIs and the on-main `Metrics` and `recv_bandwidth`,
  and need only C10's `set_latency` surfacing, small enough to carry as a local
  patch if declined.
- C13 render crate: it is already out-of-tree by design, so "declined" only means
  moq does not link it from their docs; the crate ships regardless once C2 lands.
- Anything else declined: the uniform fallback is "keep local behind our own
  seam, over the public frame vocabulary if at least C2 lands, fully local
  otherwise". Nothing in this plan deletes capability before its replacement or
  its upstream home is merged.

Stated plainly: the keep-local fallback world is worse than today, not a neutral
floor. If we adopt their backends where accepted and keep ours where declined,
our tree carries three states instead of one: their code for the adopted paths,
ours for the held ones, and the bridge between the two frame models. The section
3 velocity gate and the smaller-first pilot bound that exposure: if C2 plus VAAPI
decode do not land at acceptable cost, the remaining PRs are never opened and the
cut plan stays on its Scenario A shape (a plain version bump, no mixed stack)
without any of Scenario B's upstream dependence.
