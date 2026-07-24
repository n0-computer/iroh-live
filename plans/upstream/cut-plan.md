# Cut plan: the media deletion ledger

> Campaign: upstream | Kind: reference | Read `0-overview.md` first. This is the
> deletion ledger behind the pair-side counterpart plans in `counterpart/`; the
> per-module fate register is `DISPOSITION.md`. Transport, pubsub, rooms,
> adaptive, and sync are out of scope here; a separate alignment effort covers
> them.

This document is the standing deletion ledger for iroh-live's media code: the
per-module verdicts, staging, and risk register that the counterpart plans
(`counterpart/codec-remove.md`, `counterpart/capture-remove.md`,
`counterpart/render-adopt.md`) execute on the paired iroh-live `up/<name>`
branches (`branches.md`). It stops at the media boundary: rusty-codecs,
rusty-capture, and the media modules of moq-media (the audio device layer, the
file sources, the processing helpers). Everything about transport, pubsub,
rooms, adaptive, and sync is out of scope here and belongs to a separate
alignment effort. Every replacement named here is on moq main, working tree
`/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`.

It consolidates the verdicts of the comparisons; rows cite them with the
shorthand codes: 1-code-map = `comparisons/iroh-live-code-map.md`,
2-moq-inventory = `comparisons/moq-inventory.md`, 3-compare =
`comparisons/codecs.md`, 3z = `comparisons/zerocopy.md`, 3t =
`comparisons/traits-api.md`, 3u = `comparisons/moq-changes.md`, 4-compare =
`comparisons/capture.md`, 6-compare = `comparisons/audio.md`. The comparisons'
requirement and decision codes map onto the current plans as: U1 = D1 = base
plan B1 (public frame vocabulary), D3 = base plan B2 (PTS through encode),
U2 = the decode-surface work (`codec/vtb-mf-decode-surface.md` and the export
half of `codec/vaapi-decode.md`), U3 = `capture/pipewire-dmabuf.md`, U4 =
`render/moq-video-render.md`, D2 = base plan B4. Where a row here and
`0-overview.md` differ, the overview governs.

## 1. Principles

**P1. Nothing is cut until its replacement is proven in-tree.** A module is
deleted only after a working example or e2e test passes on the new path in this
repository. The existing gates are `moq-media/tests/pipeline_integration.rs`,
the rusty-codecs conformance harness (`rusty-codecs/src/codec/tests/`), and the
hardware-gated `moq-media/tests/zero_copy_pipeline.rs`. Section 5 lists the
gaps that must be closed before specific stages.

**P2. Zero-copy capability is never regressed.** Adopting moq's codec and
capture surface as-is would destroy every iroh-live zero-copy path except macOS
capture-to-encode (3z summary and section 2). Any cut that would drop a
zero-copy path waits for the corresponding leaf to land and release: the VAAPI
pair waits for B1 plus the vaapi leaves; the VideoToolbox decoder waits for B1
plus `vtb-mf-decode-surface`; Android waits for B1 plus the B4 placement
outcome; PipeWire waits for `pipewire-dmabuf`.

**P3. One release gate, labeled on every row.** Local cuts need no moq change
and can proceed against whatever release iroh-live pins today (dead code, the
already-broken catalog mirror). Release cuts need the next moq release carrying
the merged stack, or the release carrying a named leaf, plus a version bump.
Every row carries `local`, `release`, or the leaf it waits on.

**P4. API-first.** Cuts that touch the frame model, timestamps, or dispatch
are coded against the frozen base API contract in `0-overview.md` (B1, B2,
B3); they do not run ahead of it. The `Timestamp` and catalog-mirror convergence
(stage M0) is local and runs first so adoption diffs stay clean.

## 2. The cut ledger

Verdict vocabulary, matching `DISPOSITION.md`:

- **adopt-theirs**: moq's implementation on main is the replacement; ours is
  cut on the release bump.
- **adopt-theirs (after \<leaf\>)**: moq's implementation is the replacement
  once the named upstream leaf fixes its gap and releases; then ours is cut.
- **upstream-ours**: ours is the implementation; it moves into moq via the
  named leaf, and the local copy is cut only when the release carrying it is
  pinned. If the offer is declined, the module stays local and supported.
- **keep**: stays in iroh-live; no upstream plan.
- **drop**: deleted with no replacement.
- **merge**: the module survives but sheds an identified portion.

LOC figures are from 1-code-map section 2.

### rusty-codecs (22,310 LOC)

| Module | LOC | Verdict | Replacement / leaf | Gate | Evidence |
|---|---:|---|---|---|---|
| codec/h264/sps.rs | 586 | drop (dead code) | none; the SPS VUI patcher is offered separately (`codec/bitstream-sps-vui.md`) | local | 3-compare sec 7 |
| config.rs | 318 | adopt-theirs | `hang::catalog` types directly; the mirror no longer compiles against hang 0.19.5 | local | 3t D5, sec 4.1 |
| codec/h264/ encoder+decoder (openh264) | ~906 | adopt-theirs | moq-video openh264 encode/decode backends | release | 3-compare sec 1 |
| codec/h264/annexb.rs | 364 | adopt-theirs | `moq_mux::codec` (Avcc parse, annexb, param-set injection); `build_avcc` parked in `capture/parity-ports.md` | release | 3-compare sec 7 |
| codec/vtb/encoder.rs | 895 | adopt-theirs | moq-video videotoolbox encode (H.265, High profile, per-frame IDR) | release | 3-compare sec 1 |
| codec/vtb/decoder.rs (+mod) | ~599 | adopt-theirs (after vtb-mf-decode-surface) | moq-video VT decode retaining the CVPixelBuffer | B1, the leaf, release | 3-compare sec 1; 3z sec 5 |
| codec/vaapi/ | 3,257 | upstream-ours | `codec/vaapi-decode.md` + `codec/vaapi-encode.md` (DMA-BUF import, VPP, PRIME export into moq-video/moq-vaapi) | B1, B2, both leaves, release | 3-compare sec 1; 3z sec 5; 3u sec 1 |
| codec/v4l2/ | 1,856 | upstream-ours | `codec/v4l2-encode.md` + `codec/v4l2-decode.md` | B1, B2, both leaves, release | 3-compare sec 1; 3u sec 2 |
| codec/android/ | 1,528 | upstream-ours | `codec/android-mediacodec.md` (MediaCodec backends plus HardwareBuffer) | B1, B2, B4 placement, the leaf, release | 3-compare sec 1; 3u sec 4 |
| codec/av1/ | 936 | drop (local rip-out) | none this series; the upstream plan (`codec/av1-software.md`) is deferred, and the local backend is dropped, re-added later only if a use case needs it | local; proof before deletion | 3-compare sec 3; 0-overview |
| codec/opus/ | 804 | adopt-theirs (after opus-improvements) | moq-audio Encoder/Decoder once runtime `set_bitrate`, the pre-skip fix, and a channel-remap policy land (`audio/opus-improvements.md`) | the leaf, release | 3-compare sec 5; 6-compare sec 1.5, 7 |
| codec/pcm/ | 559 | upstream-ours | `audio/pcm.md` (`Codec::Pcm` plus the hang catalog variant, one branch); local codec stays until that release is pinned | the leaf, release | 3-compare sec 6; 6-compare 1.4 |
| codec.rs + codec/dynamic.rs | 522 | adopt-theirs (after the backend leaves) | moq-video Candidate/Kind dispatch, once every upstream-ours backend is admitted; `reset()`/`burst_size()` carried into their decode trait via `codec/vaapi-decode.md` | all backend leaves, release | 3-compare sec 8 |
| traits.rs | 410 | merge | codec traits fall away with adoption; the device traits (`AudioSink`, `AudioSinkHandle`, `AudioStreamFactory`, `AudioSource`, `VideoSource`) stay local and shrink as `audio/audio-device-unify.md` lands | B1, B2, release | 3t sec 1 |
| format.rs | 1,292 | merge | the public frame model collapses onto B1 `Native`; `NativeFrameHandle`/`DmaBufInfo` are the B1 donors; the CPU half stays | B1 | 3t sec 3; 3z sec 1; 3u sec 1 |
| processing/ | 1,086 | merge | resample.rs converges on `moq_audio::Resampler` plus our remix helper; scale.rs and convert.rs stay serving capture and render | release for the resampler half | 6-compare sec 2 |
| render.rs + render/ | 3,463 | upstream-ours | `render/moq-video-render.md` (in-tree moq workspace crate, heavy deps behind non-default features, wgpu and GLES both feature-flagged); executed by `counterpart/render-adopt.md` | B1, B3, vtb-mf-decode-surface, the leaf, release | 3z sec 4, 5; 0-overview |
| test sources + conformance harness | 2,880 | keep, shrinks with cuts | adopted backends are covered by moq-video's own tests | tracks stages M1, M2 | 1-code-map sec 2 |
| lib.rs | 8 | keep | | | |

### rusty-capture (5,507 LOC)

| Module | LOC | Verdict | Replacement / leaf | Gate | Evidence |
|---|---:|---|---|---|---|
| platform/apple/screen.rs | 394 | adopt-theirs | moq-video screencapture (app capture, NV12 surfaces, fail-fast TCC) | release | 4-compare sec 2, 5 |
| platform/apple/camera.rs | 81 | adopt-theirs | moq-video avfoundation (working, zero-copy, TCC flow); ours is a stub | release | 4-compare sec 2, 5 |
| windows stubs (part of 134) | ~100 | adopt-theirs | moq-video mediafoundation + desktopduplication; ours are stubs | release | 4-compare sec 2, 5 |
| platform/nokhwa_impl.rs | 246 | drop | superseded once the adopted macOS camera and Windows backends are proven; Linux cameras are covered by V4L2, PipeWire, and libcamera | release; stage M2 | 4-compare sec 2, 5 |
| platform/xcap_impl.rs | 175 | drop | same gate; X11 remains the portal-less Linux screen fallback | release; stage M2 | 4-compare sec 2, 5 |
| lib.rs + types.rs | 1,107 | merge | selection cascade and backend enum shrink as the Apple and Windows arms migrate (estimate ~250 removed) | stage M2 | 4-compare sec 5 |
| platform/linux/pipewire.rs | 1,655 | upstream-ours | `capture/pipewire-dmabuf.md` (DMA-BUF delivery into moq's CPU-only backend); moq's token replay, static-screen re-pacing, and open-per-demand lifecycle are ported into ours meanwhile | B1, the leaf, release | 4-compare sec 2, 5; 3z sec 5 |
| platform/linux/v4l2.rs | 552 | upstream-ours | `capture/v4l2-camera-enum.md` (enumeration and negotiation fill moq's macOS-only `cameras()`); adopts moq's zune-jpeg MJPEG shortcut in the port | the leaf, release | 4-compare sec 2, 5 |
| platform/linux/libcamera_h264.rs | 522 | upstream-ours | `capture/libcamera-preencoded.md` (the pre-encoded source, required) | the leaf, release | 4-compare sec 2, 5; 3u sec 3 |
| platform/linux/libcamera.rs | 268 | upstream-ours | travels with `capture/libcamera-preencoded.md` as the raw companion | the leaf, release | 4-compare sec 2, 5 |
| platform/linux/x11.rs | 373 | upstream-ours (leaf pending) | portal-less X11 screen capture, which moq lacks entirely (their story is `Unsupported`); needs a small follow-up leaf after the Linux capture series | a leaf to be written | 4-compare sec 2 |
| android stub (~34) | 34 | keep (stub) | nothing to move yet on either side; Android capture is future work beside `codec/android-mediacodec.md` | n/a | 4-compare sec 2 |

### moq-media, media modules only (the alignment modules are out of scope here)

| Module | LOC | Verdict | Replacement / leaf | Gate | Evidence |
|---|---:|---|---|---|---|
| audio_backend.rs + aec.rs | 2,837 | upstream-ours | `audio/audio-device-unify.md` (playback sink and AEC into moq-audio behind features); moq's capture surface (system audio, TCC, `format()` without open) is adopted onto our bounded buffers meanwhile | the leaf, release | 6-compare sec 3, 7 |
| audio_file_source.rs + audio_file_symphonia.rs | 472 | keep | app-level decoded-PCM sources; open question on a moq-audio home is discussed in `audio/audio-device-unify.md`, current proposal: stays here | n/a | 6-compare sec 4 |
| processing.rs + mjpg | 87 | keep | | | 1-code-map |
| capture.rs + util + test_util | 551 | keep | | | 1-code-map |

### Expected LOC removed (media ledger only)

Sums are ledger-row estimates, cumulative, rounded; treat as +/-15%. Scenario A
is adopting the next moq release as-is; Scenario B adds our upstream leaves
being accepted and released. Combined with the separate alignment effort's cuts, the two together
remove about 4,800 LOC (12% of the 41,564 core) on the release bump alone and
about 17,400 (42%) with the upstream leaves accepted.

| Crate | Scenario A | Scenario B |
|---|---:|---:|
| rusty-codecs | 3,069 (sps.rs, config.rs, openh264, annexb, VTB encoder) | ~15,100 (+VAAPI, V4L2, Android, AV1, Opus, VTB decoder, dispatch, trait and frame-model halves, ~1,200 test shrink) |
| rusty-capture | ~1,250 (Apple screen+camera, Windows stubs, nokhwa, xcap, cascade shrink) | ~1,250 now; the Linux column follows its leaves |
| moq-media (media rows) | 0 | ~2,900 (audio_backend + aec after audio-device-unify) |

Scenario A is the ordinary outcome of a version bump: every replacement it
counts exists today on moq main at `3a3e0ea8` and ships in the next release
iroh-live can pin. Scenario B's additional cuts each key on a named leaf
releasing; if a leaf is declined, its module stays local under upstream-ours
and nothing is lost. The sums count deletions only; the bump itself forces a
migration across our consumers (module renames and the `#[non_exhaustive]`
sweep), and any bridge code from stage M1's ordering is written to be deleted,
so the net saving is somewhat below the gross figures.

## 3. Ordering

Stages are sequential per platform; the separate alignment effort's stages
interleave with these where its pubsub work depends on the codec adoption.

**Stage M0: type convergence (local).**
Adopt `moq_net::Timestamp` in place of `Duration` through format.rs and the
pipelines, and delete the broken catalog mirror `config.rs` in favor of direct
`hang::catalog` types. Also the local drops: `sps.rs` dead code, the misleading
V4L2 EXPBUF doc claim, and the AV1 rip-out (proof before deletion). Entry
condition: none. Doing M0 after M1 would force every adoption diff to carry
conversion shims, so it goes first.

**Stage M1: codec adoption, atomic per platform (release-gated).**
Adopt moq-video openh264 encode/decode, the VideoToolbox encoder, and the
bitstream front end, gaining NVENC/NVDEC, Media Foundation, H.265, and
`rate::Control` outright. Hold VAAPI, V4L2, Android, the VTB decoder, and Opus
on our implementations until their leaves land and release (P2). Platforms
flip whole: Windows adopts immediately (pure gain, nothing held); macOS flips
when the VTB pair including the decode-surface leaf is ready; Linux non-NVIDIA
flips when the VAAPI and V4L2 series land. Atomic switchover avoids mixing two
frame models within one platform: the mixed-stack bridge would cost 300 to 600
LOC of temporary conversion plus a doubled test matrix, and buys little,
because the early adoptions mostly add capability we do not ship today. Entry
condition: the release bump, stage M0, and the platform verification gate R-g
for every platform being switched. Gate: the conformance harness and
`pipeline_integration.rs` pass with the adopted decoders; latency tests do not
regress.

**Stage M2: capture adoption (release-gated).**
Adopt moq-video capture for macOS camera, macOS screen, and both Windows
backends; drop nokhwa, xcap, and the Windows stubs afterward; the Linux column
follows its own leaves rather than being adopted. Adopt the moq-audio capture
surface (system audio, TCC flow, `format()` without open, demand gating) onto
bounded buffers, explicitly not importing their unbounded realtime channel
(6-compare sec 3.3; the bounded-channel fix upstreams via
`capture/parity-ports.md`). Entry condition: stage M1 on the same platform,
because their capture emits their frame model into their encoders, plus R-g for
macOS and Windows. macOS camera currently works only through nokhwa, so
removing it before the AVFoundation backend is proven leaves macOS camera dead.

**Upstream-gated cuts.** Each upstream-ours row is cut on its paired
`up/<name>` branch per its counterpart plan, only once the release carrying
the leaf is pinned: the VAAPI, V4L2, and Android backends and the dispatch
collapse (`counterpart/codec-remove.md`), the Linux capture column
(`counterpart/capture-remove.md`), the render tree
(`counterpart/render-adopt.md`), and the audio device layer (the
`up/audio-device` pair). Dependency spine: B1 gates the decode-surface,
vaapi, pipewire, and render leaves; B2 gates the v4l2 and android encoders;
those leaves gate the corresponding local cuts.

## 4. What stays local

The permanent keeps of the media ledger, each with its reason.

| Kept | Why |
|---|---|
| test sources + conformance harness (2,880, shrinking) | our gate for every adoption and cut; adopted backends shed their vectors |
| device traits in traits.rs | iroh-live's app-facing device surface; shrinks as audio-device-unify lands but the seam stays |
| format.rs CPU half | the I420/CPU frame plumbing every fallback path uses |
| processing scale.rs + convert.rs | serve capture and render locally; no moq counterpart |
| audio_file_* (472) | app-level sources; open question on a moq home discussed in `audio/audio-device-unify.md`, current proposal stays |
| moq-media capture.rs + util + test_util (638) | local glue and test scaffolding |
| android capture stub (34) | placeholder until Android capture exists anywhere |

Everything else media-shaped either adopts moq's implementation or moves
upstream through a leaf and is then cut. The transport, pubsub, rooms,
adaptive, sync, stats, and chat keeps are the alignment ledger's section 4.

## 5. Risk register

**R-a. Release timing.** The whole adopt-theirs stack is on moq main at
`3a3e0ea8`; adoption waits only for an ordinary release plus a version bump.
Mitigation: stage M0 is local and proceeds regardless. If the release slips,
the plan still yields M0 and the leaf authoring, which targets moq main
directly.

**R-b. moq API churn between `3a3e0ea8` and the release.** Post-merge main is
still settling (module boundaries, `#[non_exhaustive]` sweep, wire-version
constants). Mitigation: do not start M1 against a git pin; treat citations
pinned to `3a3e0ea8` as direction, not API contract. Before any stage or wave
starts, re-diff `3a3e0ea8` against the then-current main, re-validate the
enabler register (2-moq-inventory summary table 2), and re-confirm the
affected rows.

**R-c. Upstream acceptance.** B1 is the keystone; the moq-vaapi growth is the
largest single piece and the one most likely to meet resistance; Android is a
plausible decline that pushes it onto the B4 registration path. Mitigation:
every upstream-ours module stays local and supported until its leaf releases,
and stays permanently if declined. Scenario A is an acceptable waypoint on its
own.

**R-d. The rav1d git-fork pin.** moq accepts crates.io dependencies only, so
any AV1 upstream is gated on a released rav1d or vendoring. AV1 is deferred
and the local backend dropped; resolving the pin is a precondition for the
deferred `codec/av1-software.md`, not for any local deletion.

**R-e. The cpal git pin.** audio_backend uses a git-pinned cpal while
moq-audio uses crates.io cpal; `audio/audio-device-unify.md` treats resolving
the pin as a hard prerequisite, since two cpal versions cannot share one
dependency graph.

**R-f. Behavioral differences to verify at each gate.** Their openh264 output
is Annex-B avc3 only, no avcC (3-compare sec 1); their Opus uses
`OPUS_APPLICATION_AUDIO` versus our VOIP and a zero pre-skip OpusHead
(6-compare sec 1.1); their VT encoder uses High profile versus our Baseline;
their video rendition registers in the catalog only after the first SPS versus
our register-up-front (3t sec 1); their capture drops oldest at depth 4 versus
our backpressure at depth 2 (4-compare sec 1); their mic capture uses an
unbounded channel we must not inherit (6-compare sec 3.3, fixed upstream by
`capture/parity-ports.md`).

**R-g. Platform verification gate.** P1 is unenforceable on a platform we
cannot test: there is no macOS or Windows CI today, and the zero-copy e2e runs
only on Intel Linux hardware by hand. macOS and Windows CI, or at minimum
scripted on-hardware verification runs with recorded results, is an explicit
prerequisite for every stage that switches those platforms (M1 for the VTB
swap and Windows, M2 for camera and screen smoke tests per adopted backend).
Every validation report sent upstream carries reproducible scripts and exact
environment versions so results can be re-run without us.

## 6. Commit strategy

Per the workspace workflow (conventional prefixes, `cargo make check-all`
before every commit, no doc-only commits, no push without an explicit ask):

- Each stage is a series of small compiling commits on its branch, one concern
  per commit (`refactor:` for type convergence, `feat:` for adopted
  capability, `chore:` for deletions), with the stage's gate tests green at
  every commit.
- Where old and new paths must coexist, the transition is feature-flagged: the
  new path lands first, tests run against both, the default flips in its own
  commit, and the old path is deleted in a final commit only after P1 passes
  on the new default. Deletion commits contain nothing else, so a revert
  restores the old path cleanly.
- Upstream-ours modules never get a local deletion commit until the release
  containing them is pinned in `Cargo.toml`; the deletion commit and the
  version bump travel together on the pair branch.
- Doc updates ride the code commits that touch the same area, never
  standalone.
