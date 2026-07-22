# codec-remove

Branch: align/codec-remove          Wave: 3
Depends on: pin bump to the moq release carrying the merged media stack (Wave 0);
the frame-model collapse in `render-adopt` for every decode deletion (coordination
point 2); and, per backend, the specific upstream contribution named in the ledger
below reaching a moq release iroh-live can pin.
Kind: upstream-gated

## Goal

Delete `rusty-codecs`' codec backends, dispatch, bitstream helpers, and the codec
half of its trait and frame surface as moq gains an equivalent for each, so
iroh-live encodes and decodes through `moq-video`, `moq-audio`, and
`moq_mux::codec` rather than its own 22,310-line codec crate. Two removal kinds
run side by side. Adopt-theirs backends (openh264, VideoToolbox encode, the
bitstream front end, and the Candidate dispatch) are replaced by code already on
moq main and land with the release bump. Upstream-ours-then-remove backends
(VAAPI, V4L2, Android, and Opus) are deleted only after the matching upstream
pull request from `plans/upstream/` merges into a moq release, because deleting
them before their replacement exists would drop hardware decode on Intel and AMD
Linux, ARM SoCs, and Android, and would regress the decode-to-render zero-copy
inputs. AV1 is the exception among the once-held backends: per overview revision
4 it is not upstreamed this series (rav1e too slow, rav1d dependency too heavy),
so its module is a local rip-out deleted immediately with the independent
deletions rather than gated on an upstream contribution, and it can be re-added
later if a use case needs it. This is the largest removal in the campaign, roughly 12,000 to 15,000 LOC
of the 22,310, matching the cut-plan's Scenario B `rusty-codecs` figure of about
15,100 once the render row leaves the crate under `render-adopt`.

## Evidence

- `../cut-plan.md` section 2, the `rusty-codecs` ledger, is the source of truth
  for every verdict, LOC, prerequisite, and label cited below.
- `../../upstream/comparisons/codecs.md` carries the per-backend capability
  comparison and the verdict table (section 10), with the top upstream candidates
  in priority order.
- `../../upstream/comparisons/zerocopy.md` sections 2b and 5 establish coordination
  point 2: the decode-to-render zero-copy path survives only if `render-adopt`
  re-homes the renderer onto moq's public handles before these decoders are cut.
- Upstream contributions are the leaf plans under `../../upstream/codec/` and the
  base plans under `../../upstream/base/`.

## moq primitive adopted

- `moq-video` openh264 encode and decode backends (`moq:encode/backend/openh264.rs`,
  `moq:decode/backend/openh264.rs:44-80`), with the shared avc1 front end handling
  parameter-set injection (`moq:decode/decoder.rs:94-160`).
- `moq-video` VideoToolbox encode with H.265, High profile, and per-frame forced
  IDR (`moq:encode/backend/videotoolbox.rs`).
- `moq_mux::codec` bitstream handling: `h264::Avcc::parse`, `h265::Hvcc::parse`,
  `annexb::build_prefix`, and `annexb::from_length_prefixed` honoring the actual
  `lengthSizeMinusOne` (`moq:decode/decoder.rs:94-176`).
- `moq-video` Candidate/Kind dispatch with `Kind::{Auto, Hardware, Software,
  Named}` and tried-list errors (`moq:encode/backend/mod.rs:60-133`,
  `moq:decode/backend/mod.rs:89-145`).
- `moq-audio` Opus `Encoder`/`Decoder` (`moq:encode/encoder.rs`,
  `moq:decode/decoder.rs`) once the three upstream items land.
- The public `Native` frame vocabulary from base plan B1, consumed for the frame
  model that `format.rs` sheds (adopted concretely in `render-adopt`).
- `moq-video` VAAPI and V4L2 backends, and the registered or in-tree Android
  backend, once our own contributions land them upstream. AV1 has no adopted
  replacement this series (deferred per overview revision 4); its local backend is
  ripped out rather than swapped for a moq equivalent.

## iroh-live code changed

Per-backend deletion ledger, each keyed to the exact upstream contribution and
moq release that must land first. LOC are verified against the working tree.

| Module | file:line | LOC | Removal kind | Waits on |
|---|---|---:|---|---|
| openh264 encode+decode | `rusty-codecs/src/codec/h264/encoder.rs`, `.../decoder.rs` | 906 | adopt-theirs | release bump; replacement is moq-video openh264, already on main (codecs.md sec 1, verdict "cut and replace with theirs") |
| annexb helpers | `rusty-codecs/src/codec/h264/annexb.rs` | 364 | adopt-theirs | release bump; replacement is `moq_mux::codec`; park `build_avcc` (codecs.md sec 7) |
| sps VUI patcher | `rusty-codecs/src/codec/h264/sps.rs` | 586 | local dead-code delete | nothing; `#[allow(dead_code)]` today, deletable now; the patcher logic is offered upstream as `../../upstream/codec/bitstream-sps-vui.md`, which does not gate this deletion (codecs.md sec 7) |
| catalog mirror | `rusty-codecs/src/config.rs` | 318 | local delete | nothing (Wave 1 / stage 1); mirror no longer compiles against hang 0.19.5; replaced by `hang::catalog` types (codecs.md final section; cut-plan D5) |
| VTB encode | `rusty-codecs/src/codec/vtb/encoder.rs` | 895 | adopt-theirs | release bump, plus atomic-macOS hold: flips only when the VTB decode swap is also ready (codecs.md sec 1; coordination point 4) |
| VTB decode | `rusty-codecs/src/codec/vtb/decoder.rs` (+`vtb.rs`) | 599 | cut-after-upstream | `../../upstream/codec/vtb-mf-decode-surface.md` (retain CVPixelBuffer, needs B1+B3) in a release, plus `render-adopt` (U1, U2; zerocopy.md sec 5) |
| VAAPI encode+decode | `rusty-codecs/src/codec/vaapi/` | 3,257 | upstream-ours-then-remove | `../../upstream/codec/vaapi-decode.md` and `../../upstream/codec/vaapi-encode.md` (grow moq-vaapi export and VPP; B1, B2, B3; U1, U2, U3) merged and released (cut-plan VAAPI row; zerocopy.md sec 5) |
| V4L2 encode+decode | `rusty-codecs/src/codec/v4l2/` | 1,856 | upstream-ours-then-remove | `../../upstream/codec/v4l2-encode.md` (needs B2 = D3, PTS through encode) and `../../upstream/codec/v4l2-decode.md` (B1, B3) merged and released (cut-plan V4L2 row) |
| Android encode+decode | `rusty-codecs/src/codec/android/` | 1,528 | upstream-ours-then-remove | `../../upstream/codec/android-mediacodec.md` (B1 HardwareBuffer variant, B2, B4 registration or in-tree per D2) merged and released (cut-plan Android row) |
| software AV1 | `rusty-codecs/src/codec/av1/` | 936 | local rip-out | nothing; AV1 is not upstreamed this series (overview revision 4: rav1e too slow, rav1d dependency too heavy), so `av1-software` is deferred and this backend is deleted locally now rather than gated on it. It can be re-added later if a use case needs it (codecs.md sec 3) |
| Opus | `rusty-codecs/src/codec/opus/` | 804 | cut-after-upstream | `../../upstream/codec/opus-improvements.md` landing runtime `set_bitrate`, lookahead pre-skip fix, and a channel-remap policy (D11) in a release, then adopt moq-audio (codecs.md sec 5) |
| dispatch | `rusty-codecs/src/codec.rs`, `.../codec/dynamic.rs` | 522 | cut-after-upstream | release bump plus every held backend admitted upstream (VAAPI, V4L2, Android), with `reset()` and `burst_size()` carried into moq's decode trait; this is the last codec cut (codecs.md sec 8) |
| codec-trait half | `rusty-codecs/src/traits.rs` | part of 410 | merge | D1-D3, D11, release; the device traits (`VideoSource`, `AudioSource`, `AudioSink`, `AudioSinkHandle`, `AudioStreamFactory`) stay local (cut-plan traits row) |
| frame-model half | `rusty-codecs/src/format.rs` | part of 1,292 | merge, cut-after-upstream | B1 (`Native`) plus `render-adopt`; `NativeFrameHandle`/`DmaBufInfo` are the U1 donors and collapse into moq's public vocabulary (cut-plan format row; zerocopy.md sec 6) |
| resample half | `rusty-codecs/src/processing/resample.rs` | 123 | merge | converges on `moq_audio::Resampler`, already on main; the remix helper stays (cut-plan processing row) |
| test shrink | `rusty-codecs/src/codec/tests/`, `test_sources.rs`, `test_util.rs` | ~1,200 | shrinks with cuts | each adopted backend's conformance vectors retire as moq-video's own tests cover them (cut-plan test row) |

Explicitly not deleted by this task: the PCM codec
(`rusty-codecs/src/codec/pcm/`, 559 LOC), kept because iroh-live requires the
uncompressed PCM path (overview revision 3). Do not delete `rusty-codecs/pcm`
unless moq accepts the `Codec::Pcm` offer in `../../upstream/codec/pcm.md` and
releases it, at which point iroh-live can adopt moq-audio's PCM; if moq declines,
iroh-live keeps its own PCM codec so the capability is never lost (codecs.md sec 6);
`processing/scale.rs` (360) and `processing/convert.rs` (598), which stay serving
capture and render (cut-plan processing row); the device traits in `traits.rs`;
and the whole `render.rs` plus `render/` tree, which leaves the crate under
`render-adopt`, not here. Any contribution upstream declines (a plausible outcome
for Android per cut-plan R-c) keeps its module in-tree indefinitely under
keep-and-upstream-copy. AV1 is handled differently: it is not offered upstream
this series (overview revision 4), so rather than being kept pending a decline it
is ripped out locally now, and re-added later only if a use case needs it.

## Steps

Ordered so adoption always precedes deletion, and each platform with a held
backend flips atomically per coordination point 4.

1. Land the Wave 0 pin bump and the Wave 1 local cuts first, so `config.rs` and
   `sps.rs` are gone and the `Timestamp` and catalog-type convergence is done
   before any codec diff has to carry a conversion shim.
2. Adopt moq-video openh264 encode and decode and the `moq_mux::codec` bitstream
   front end behind a `moq-native-codecs` cargo feature. Run both paths through
   the conformance harness, flip the default, then delete
   `codec/h264/encoder.rs`, `codec/h264/decoder.rs`, and `codec/h264/annexb.rs`
   in a deletion-only commit. This is the software fallback path on every
   platform and regresses no zero-copy path (openh264 output is CPU I420).
3. Windows adopts immediately and atomically: it has no held backend and no
   working local codec today, so adopting moq-video NVENC, Media Foundation, and
   H.265 is pure gain with nothing to bridge (cut-plan stage 2 bridge-cost
   paragraph).
4. Hold macOS entirely on the local stack until `vtb-mf-decode-surface` has
   released and `render-adopt` consumes moq's `decode::Frame::native()`. Then
   flip macOS atomically: adopt the VideoToolbox encoder and the surface-retaining
   decoder together, and delete `codec/vtb/encoder.rs` and `codec/vtb/decoder.rs`
   in one platform-scoped, revertible commit, so the CVPixelBuffer-to-render path
   never breaks (coordination point 2; zerocopy.md sec 2b).
5. Hold Linux non-NVIDIA entirely until the VAAPI and V4L2 upstream series have
   merged and released. Then flip Linux atomically: pin the moq release carrying
   the upstreamed VAAPI (encode and decode over the grown moq-vaapi) and V4L2
   backends, and delete `codec/vaapi/` and `codec/v4l2/` together with the version
   bump in the same commit (cut-plan commit strategy: the deletion and the bump
   travel together). The DMA-BUF-to-render path must already run through
   `render-adopt` before this deletion.
6. Hold Android until `android-mediacodec` is accepted (in-tree or registered per
   the D2 decision), then delete `codec/android/`, preserving the HardwareBuffer
   decode-to-render input via `render-adopt` (zerocopy.md sec 2b).
7. Delete `codec/av1/` as a local rip-out with the Wave 1 independent deletions,
   not gated on any upstream contribution, since AV1 is not upstreamed this series
   (overview revision 4). Software AV1 decodes to CPU I420 and feeds no zero-copy
   render input, so its removal regresses no held frame model and needs no
   platform-atomic hold. The proof-before-deletion rule still applies: a local
   end-to-end test must pass without the AV1 backend before the module is deleted.
   AV1 can be re-added later if a use case needs it.
8. Adopt moq-audio Opus once its three upstream items release, then delete
   `codec/opus/`.
9. Only after every held backend above is cut, replace the local dispatch with
   moq-video's Candidate/Kind model and delete `codec.rs` and `codec/dynamic.rs`.
   Dispatch is last because it must not name a backend that still exists locally.
10. With every codec backend gone, shed the codec half of `traits.rs` and the
    frame-model half of `format.rs` (coordinated with `render-adopt`), converge
    `resample.rs` on `moq_audio::Resampler`, and retire the conformance vectors
    for adopted backends.

## Proof before deletion

Mandatory, per coordination point 1: no module is deleted until the new path
passes an end-to-end test in this repository.

- The rusty-codecs conformance harness (`rusty-codecs/src/codec/tests/`) and
  `moq-media/tests/pipeline_integration.rs` pass with the adopted decoders in
  place, and the latency tests do not regress, before any adopt-theirs deletion.
- For every held backend, the hardware-gated `moq-media/tests/zero_copy_pipeline.rs`
  passes on the target hardware (Intel or AMD GPU for VAAPI, an ARM board for
  V4L2, an Android device for MediaCodec) against the upstreamed backend before
  its local module is deleted.
- macOS and Windows carry the platform verification gate R-g: CI or checked-in
  scripted on-hardware runs with recorded results, since P1 is unenforceable on a
  platform we cannot test (cut-plan R-g). No macOS or Windows deletion lands
  without it.

## Coordination

- Coordination point 1 (proof before deletion) governs every row: adoption behind
  a feature flag, both paths tested, default flipped, then a deletion-only commit.
- Coordination point 2 (no zero-copy regression): every decode deletion (VTB,
  VAAPI, Android) waits for `render-adopt` and the B1 frame vocabulary so the
  decode-to-render path survives. `render-adopt` must land before or with these
  deletions.
- Coordination point 3 (upstream gating): the upstream-ours-then-remove rows are
  blocked on their named leaf plan reaching a pinned moq release; the deletion
  commit and the version bump are a single commit.
- Coordination point 4 (atomic per platform): Linux, macOS, and Android flip
  whole; the repository never holds two frame models within one platform at once.
  Windows adopts immediately because it has no held backend.

## Acceptance checklist

- Every adopt-theirs module (openh264, annexb, VTB encode, dispatch) is deleted,
  with its replacement passing the harness and pipeline tests.
- Every upstream-ours-then-remove module (VAAPI, V4L2, Android) is deleted
  only against a pinned moq release containing the merged contribution, verified on
  hardware, with the deletion and bump in one revertible commit.
- The AV1 module (`codec/av1/`) is ripped out locally with the independent
  deletions, not gated on an upstream release (overview revision 4), after a local
  end-to-end test passes without it.
- Opus is deleted only after `opus-improvements` releases the three required items.
- The codec half of `traits.rs` and the frame-model half of `format.rs` are gone;
  the device traits, `scale.rs`, `convert.rs`, and PCM remain.
- No zero-copy path regressed: `moq-media/tests/zero_copy_pipeline.rs` passes on
  each supported platform through `render-adopt`.
- `cargo make check-all` is green at every commit, and each deletion commit
  contains nothing but the deletion so a revert restores the old path cleanly.
</content>
</invoke>
