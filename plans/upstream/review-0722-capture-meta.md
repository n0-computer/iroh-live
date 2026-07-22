# Adversarial review 2026-07-22: capture / audio / render plans + campaign structure

Skeptical staff review, cross-checked against real source: moq `/home/bit/Code/rust/moq`
HEAD `3a3e0ea80c2103269992dd4352061b4807f6fdb7` (matches the cited `3a3e0ea8`), iroh-live
working tree. Every anchor below was opened and read, not inferred.

## Verdict 1 — capture / audio / render plans

**Sound and largely well-anchored; ship after fixing one wrong anchor.** The load-bearing
technical claims are all true against source: moq's PipeWire capture is CPU-only (shm offer,
convert-to-I420), moq's `Producer::publish(Vec<Bytes>, Timestamp)` already accepts external
Annex-B so `publish_preencoded` is genuinely additive, the moq-mux OpusHead hardcodes pre-skip
0 and gain 0, moq's `Codec` enum is `#[non_exhaustive]` single-`Opus` with no PCM, moq has no
portal camera source at all, and all five render files exist at the cited sizes. The Opus-mux
and render anchors are exact. The defects are anchor drift (mostly ±1–10 lines) plus one
substantively wrong line reference in v4l2-camera-enum. No unbuildable assumption in these
three plans.

## Verdict 2 — overall campaign structure (0-overview)

**Structurally coherent but rests on two shaky foundations that need correction before
execution.** The dependency tree is acyclic, all 19 plan files exist and the capture/audio/
render index rows match their plan headers, and no listed backend lacks a plan. But (a) the
git/PR model as written does not work for an external contribution, and (b) the tree and
coordination point 3 lean on a "shared moq-vaapi crate" that does not exist, while moq already
ships an in-tree VAAPI *encode* backend the scope section never acknowledges. Four cross-cutting
concerns (licensing, CI hardware-gating policy, semver-across-the-fan, moq-vaapi ownership/PR
target) are absent from the seven coordination points.

## Counts

- Capture/audio/render: 0 blocking, 1 substantive, 4 nit.
- Structure/meta: 1 blocking, 3 substantive, 1 nit.
- Positives verified: 12 (listed at end).

---

## Capture / audio / render findings

### C1 — v4l2-camera-enum cites the wrong moq line for YUYV conversion — SUBSTANTIVE
Location: `capture/v4l2-camera-enum.md` Evidence, "rs/moq-video/src/capture/v4l2.rs:88-96,
146-151 YUYV via `I420::from_yuyv` and MJPEG via `zune_jpeg::JpegDecoder`".
Issue: In moq's `v4l2.rs`, lines 88–96 are `Camera::open` / format negotiation, not YUYV
conversion. The YUYV `I420::from_yuyv` arm and the MJPEG `JpegDecoder` block are BOTH in
`read()` at lines 146 and 147–159. The `88-96` anchor is simply wrong.
Evidence: `sed -n '82,100p'` shows `impl Camera { fn open(...) { ... negotiate(...) ... }`;
`read()` with `Source::Yuyv => I420::from_yuyv` is at line 146.
Fix: cite `v4l2.rs:141-159` for both conversion arms; drop `88-96` or re-label it as the
format-negotiation site (`Camera::open`/`negotiate`).

### C2 — pipewire-dmabuf: iroh-live dmabuf-to-frame anchor drifts ~10 lines — NIT
Location: `capture/pipewire-dmabuf.md` Source-to-port and step 4: "`dmabuf_to_frame`
(`pipewire.rs:721-766`)" and "our NV12-only zero-copy gate (`pipewire.rs:721`)".
Issue: In iroh-live `pipewire.rs`, `fn dmabuf_to_frame` is at line 731 (with
`dmabuf_to_frame_cpu` at 782), not 721. The `spa_format_to_drm_fourcc` (114) and
`PipeWireDmaBufFrame` (145) anchors are exact; only the dmabuf_to_frame range is off.
Fix: change `721-766` to `731-780` and move the NV12-gate anchor accordingly.

### C3 — pipewire-dmabuf: moq re-pace-timer anchor points at the quit handler — NIT
Location: `capture/pipewire-dmabuf.md` Target-in-moq: "The static-screen re-pacing timer
(`pipewire.rs:462-475`) keeps working because it re-emits `state.last`".
Issue: In moq's `pipewire.rs` the re-pace timer (`add_timer`, `chan.push(Frame::I420(last))`)
is at lines 440–457; lines 460–467 are the `_quit` handler, not the timer. Also `convert()` is
at 473–482, not the cited 474–485 (474–485 spills into `format_offer`).
Fix: cite `440-457` for the re-pace timer and `473-482` for `convert()`.

### C4 — opus-improvements: cluster of ±1–2 line anchor drifts — NIT
Location: `codec/opus-improvements.md` Evidence/Target.
Issue: moq `opus.rs` `RATES` is line 12 (cited `:13`), `FRAME_DURATIONS` line 15 (cited `:16`),
`pick_rate` 19–21 (cited `:20-22`); moq-audio `encoder.rs` construction bitrate block is 182–191
(cited `:180-188`) and `catalog()` is at line 262 (cited `:263`). `validate_channels` (33-40),
`frame_size` (43-51), the mux pre-skip site, and `producer.rs:221` "Opus PLC handles dropped
groups" are all exact. Low impact; tighten for the PR author.
Fix: shift the five drifted anchors by 1–2 lines.

### C5 — opus-improvements: VOIP anchor is the import, not the apply site — NIT
Location: `codec/opus-improvements.md` step 6: "Ours defaults to `OPUS_APPLICATION_VOIP`
(`opus/encoder.rs:5`)".
Issue: iroh-live `opus/encoder.rs:5` is the `use unsafe_libopus::{... OPUS_APPLICATION_VOIP ...}`
import line. The application-mode is *selected* at `encoder.rs:58` (per `comparisons/audio.md:37`).
Fix: cite `:58` (or `:5` for the import AND `:58` for the choice) so the reader finds the knob.

---

## Structure / meta findings

### M1 — the "shared moq-vaapi crate" does not exist; scope hides an existing VAAPI encoder — SUBSTANTIVE (blocks the vaapi sub-tree)
Location: `0-overview.md` dependency tree (lines 227-228), coordination point 3 (283-290),
plan-index (425-426), and the Wave 1 text (256-258).
Issue: `ls rs/` in moq shows NO `moq-vaapi` crate. moq's VAAPI lives in-tree at
`rs/moq-video/src/encode/backend/vaapi.rs` (encode). There is no decode VAAPI backend. The
overview repeatedly treats "the shared moq-vaapi crate" as an existing thing that vaapi-decode
"owns" and "grows." Either the campaign means to *create* a new `moq-vaapi` crate — a
significant, unannounced architectural decision with no ownership, PR-target, or
workspace-placement plan and no coordination point — or the "shared crate" framing is simply
wrong and it is an in-tree module. An agent handed the tree will assume a crate exists.
Compounding this: the overview's "Out of scope: adopting moq's existing backends (openh264,
VideoToolbox encode, NVENC, NVDEC, Media Foundation)" list OMITS moq's existing in-tree VAAPI
encode. So vaapi-encode is a *replacement of existing moq code*, not an additive backend — a
materially more contentious proposition than the additive framing implies, and one that is not
surfaced as a decision/coordination gate.
Evidence: `rs/moq-video/src/encode/backend/vaapi.rs` present; `rs/moq-vaapi` absent; no
`vaapi` file under `decode/backend/`.
Fix: pick one — (i) declare moq-vaapi a NEW crate explicitly, add a coordination point for its
creation/ownership/workspace placement and PR target, and say vaapi-encode *replaces*
`encode/backend/vaapi.rs`; or (ii) drop the crate framing and treat both as in-tree modules.
Either way, add moq's existing VAAPI encode to the scope discussion.

### M2 — the base-branch/leaf-PR git model does not work for an external contribution — BLOCKING
Location: `0-overview.md` "Strategy" (49-58), "Git and PR model" (394-407), runbook step 7.
Issue: "leaves target the base branch directly so they compile against the proposed API before
it merges" and "PRs target the base branch until base merges to moq main." For a contribution
worked from a fork (which the overview itself calls "a contribution to an external project"),
a PR opened against upstream moq must set its base to a branch that lives IN the upstream repo.
You cannot target your own fork's `moq-upstream/base` as the base of an upstream PR. So either
the maintainer must first create `moq-upstream/base` inside the upstream repo (an ask never
surfaced), or every leaf PR opened before base merges will render as the UNION diff
(B1–B5 + leaf) against moq main and cannot be reviewed as an isolated leaf. The realistic model
for an external contribution is: land the base series to moq main first, fully, then open leaves
against main — which serializes all of Wave 0 ahead of any Wave 1 review and removes the
"author leaves in parallel against the base branch" parallelism the plan is built around.
Fix: rewrite the git model for the fork reality: state that leaves are *authored* against a
local base branch for compilation but are only *opened as PRs* after base merges to moq main;
or negotiate an upstream integration branch with the maintainer as an explicit prerequisite.

### M3 — four cross-cutting concerns missing from the coordination points — SUBSTANTIVE
Location: `0-overview.md` "Coordination points" (265-320).
Issue: the seven points cover API freeze, candidate tables, moq-vaapi, rav1d, the pre-encoded
concept, B4, and transcode. Absent:
  - **Licensing / provenance.** The render crate ports raw libva FFI hand-transcribed from
    `va_vpp.h` (`VaProcPipelineParameterBuffer`, `dmabuf_import.rs:1026`) plus Vulkan/EGL/Metal
    glue; the codec leaves port cros-codecs-derived VAAPI. No statement on license compatibility
    with moq's license or on the provenance of manually-copied headers. (The render crate is
    out-of-tree so lower risk, but the in-tree VAAPI/V4L2 ports land in moq's tree.)
  - **CI hardware-gating as a cross-cutting policy.** Every plan says "`#[ignore]` + confirm on
    named hardware," but there is no overview-level agreement that moq maintainers accept a
    growing body of tests they cannot run, nor a shared convention for marking/tracking them.
  - **semver across the fan.** Additive public API (Native, Packet, `native()`) is a minor bump;
    B4 is major/breaking. release-plz interaction across many concurrent PRs is only lightly
    touched (line 200-201).
  - **moq-vaapi ownership / PR target** (ties to M1).
Fix: add these as explicit cross-cutting concerns or coordination points.

### M4 — missing API/negotiation details the leaves should specify — SUBSTANTIVE
Location: `capture/libcamera-preencoded.md` and `capture/pipewire-dmabuf.md`.
Issue:
  - **`publish_preencoded` exact shape.** The plan sketches the `PreEncoded` trait well but
    never nails the entry-point signature. moq's sibling is
    `async fn publish_capture(broadcast, catalog, capture::Config, encode::Options, clock)
    -> Result<(), Error>` (verified `producer.rs:183-189`). The plan should state the concrete
    mirror, e.g. `async fn publish_preencoded(broadcast, catalog, source: impl PreEncoded,
    clock) -> Result<(), Error>`, and resolve whether `encode::Options` still applies when there
    is no encoder (bitrate/codec now come from `source.config()`).
  - **Portal-camera negotiation.** The pipewire-dmabuf "Scope" note folds the portal camera in
    "reusing the same SPA negotiation," but `org.freedesktop.portal.Camera` differs from
    `ScreenCast`: there is no picker-provided node id; the Camera portal hands back a PipeWire
    remote fd and the client enumerates camera nodes on it. The note should call that out rather
    than imply the ScreenCast flow transfers unchanged.
Fix: add the concrete `publish_preencoded` signature and Options resolution; add the Camera
portal fd/enumeration specifics to the scope note.

### M5 — "leaves are independent of each other" is contradicted by the tree — NIT
Location: `0-overview.md` lines 51-52 and 399-400 vs coordination point 3 and the render dep.
Issue: the tree has leaf→leaf edges — vaapi-encode branches off vaapi-decode (coord pt 3), and
moq-video-render depends on vtb-mf-decode-surface. Acyclic, yes, but the blanket "leaves are
independent" / "leaves do not branch off each other" overstates it and could mislead an agent.
Fix: soften to "independent except where a coordination point says otherwise."

---

## Positives verified against source (do not regress these)

1. moq PipeWire capture is CPU-only: shm offer, no dmabuf modifiers, `convert()` to I420
   (`pipewire.rs:383,404-406,423-436,473-482`). `Frame::I420` push + `state.last` re-pace present.
2. moq `FrameStream` is `pub(crate)` push-model (comparison-verified; `capture/mod.rs`).
3. `cameras()` is macOS-only, returns `Error::Unsupported` off macOS (`capture/mod.rs:365-374`);
   `Camera { id, name }` at 116-123. moq has a working `v4l2` capture module using the `v4l`
   crate + `zune-jpeg` (`v4l2.rs:1-7,146-159`); no `cameras()` there yet.
4. `Producer::publish(Vec<Bytes>, Timestamp)` exists and runs external Annex-B through
   split/import (`producer.rs:85-104`); `publish_capture` at `producer.rs:183-215`;
   `Producer::new`/`demand()` at 51-83. The `publish_preencoded` concept is genuinely additive.
5. moq-mux OpusHead hardcodes pre-skip 0 (`opus/mod.rs:86`) and gain 0 (`:88`); `parse()`
   `advance(2)` at `:57`; `Config { sample_rate, channel_count }` at 35-37;
   `encode_rejects_multichannel` at `:175`. moq encoder does NOT query `OPUS_GET_LOOKAHEAD`
   (grep empty) — the "pre-skip 0" premise is real.
6. moq-audio `Codec` is `#[non_exhaustive]` single-`Opus` (`encode/encoder.rs:24-42`),
   construction-only bitrate (`:182-191`), `OPUS_APPLICATION_AUDIO` (`:14`); no `Pcm`. PCM
   plan's additive framing holds.
7. moq-audio decoder rejects channel remap ("since remapping isn't implemented",
   `decode/decoder.rs`); `opus.rs` `pick_rate`/`validate_channels`/`frame_size` present.
8. moq-audio `producer.rs:221` carries the unbacked "Opus PLC handles dropped groups" comment;
   `keyframe: true` at `:225`. The plan's correction is warranted.
9. All five render files exist at cited sizes: `render.rs` 799, `dmabuf_import.rs` 1452,
   `gles.rs` 536, `gles_dmabuf.rs` 402, `metal_import.rs` 274 (sum 3463). `WgpuVideoRenderer`
   @46, `RenderPath` @72, `render()` @267 with DmaBuf branch @287 and MetalZeroCopy @325,
   `render_cached` @576. Render plan's "0 upstream diff beyond one RFC paragraph" is accurate;
   it correctly notes the Linux DMA-BUF path additionally needs vaapi-decode.
10. moq has NO portal camera source (no libcamera camera, `cameras()` macOS-only, v4l2 the only
    Linux camera). iroh-live `PipeWireCameraCapturer` exists (`pipewire.rs:1513`). The scope
    note's "carry it as a camera sibling" gap-closure is factually grounded.
11. iroh-live anchors confirmed: `spa_format_to_drm_fourcc` @114, `PipeWireDmaBufFrame` @145;
    `PreEncodedVideoSource` @traits.rs:268; opus `set_bitrate` @206, FEC @80, DTX @85,
    `OPUS_GET_LOOKAHEAD` @97, `build_opus_head` @227, pre-skip assertion @323;
    `convert_channels_into` @decoder.rs:136; v4l2 `cameras()` @47, dead EXPBUF field @161.
12. Dependency tree is acyclic; all 19 plan files exist; capture/audio/render index rows'
    "Depends on" columns match their plan headers; no listed backend lacks a plan.
