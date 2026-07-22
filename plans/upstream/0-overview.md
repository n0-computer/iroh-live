# Upstream campaign: contribute all iroh-live codec and capture work to moq

This directory is the execution kit for upstreaming iroh-live's codec and
capture code into moq, either as new backends and crates or as improvements to
moq's existing code. The goal for this planning round is a tree of self-contained
plans that a later coordinated session can hand to many subagents, one per plan,
to produce a series of pull requests against moq: one base PR series that all the
rest depend on, then a fan of leaf PRs.

Read this overview in full before touching any plan. It defines the frozen base
API contract every leaf codes against, the dependency tree, the wave ordering,
the coordination points where agents must stop and defer to a human or to another
plan, the per-plan runbook, the plan template, and the git and PR model. moq is a
single codebase now (the `dev` branch merged into `main` on 2026-07-21, working
tree `/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`); there is no branch distinction.

## Goal and scope

Move every piece of iroh-live's owned codec and capture code that moq lacks or
implements more weakly into moq, so iroh-live can delete its parallel stack and
consume moq's native media crates. The evidence for what moq has, what we have,
and who is stronger per component is in `comparisons/` (copied from the
`plans/refactor/` analysis). The concrete moq-side API changes are designed in
`comparisons/moq-changes.md`; this campaign turns that design into ordered,
independently executable PR plans.

In scope: the base API changes, the VAAPI, V4L2, Android, and software AV1
backends, the Opus and PCM codec work, the bitstream helpers, the PipeWire,
V4L2, and libcamera capture sources, and the out-of-tree renderer crate. Out of
scope: adopting moq's existing backends (openh264, VideoToolbox encode, NVENC,
NVDEC, Media Foundation), which iroh-live consumes rather than contributes; the
room, pub/sub, and adaptive work, which is a separate effort; and the audio
device layer (the `audio_backend` echo-cancellation engine, the playback sink,
and the symphonia file source), which is neither codec nor capture and is a
separate future audio-device effort. The Opus and PCM codecs are in scope; the
audio device I/O around them is not. `comparisons/audio.md` flags the AEC engine
as a standalone-crate candidate for that later effort.

## Strategy: one base series, then a fan of leaves

The whole program rests on a small set of additive API changes to moq-video
(a public GPU-frame vocabulary, a presentation-timestamp argument on the encode
path, a decode-side handle accessor). Every zero-copy backend, the renderer, and
the pipelined encoders depend on that vocabulary and cannot be expressed without
it. So the campaign is structured as:

- A **base branch** (`moq-upstream/base`) off moq main, carrying the base plans
  B1 through B5. These are the foundation PRs. Nothing that consumes the new
  vocabulary is final until the base API shape is agreed with the maintainer.
- A **fan of leaf branches**, each off the base branch, one per module. Leaves
  are independent of each other and can be authored and reviewed in parallel,
  except at the coordination points listed below (shared candidate tables, the
  shared moq-vaapi crate, and a handful of prerequisites).

PRs land base first, then leaves rebased onto the merged result. During planning
and initial authoring, leaves target the base branch directly so they compile
against the proposed API before it merges.

## The frozen base API contract

This is the single authority every leaf codes against. It is lifted verbatim
from `comparisons/moq-changes.md` sections 1 and 2, which cite the moq source
each item changes. A leaf must treat these signatures as fixed. If a leaf finds
the contract insufficient for its module, it does not improvise a different API:
it stops and files a change against the relevant base plan (see Coordination).

**The public GPU-frame vocabulary (base plan B1, moq-changes change 1).** A new
public, closed-but-non-exhaustive enum of concrete OS handles, home a new
`moq-frame` crate (preferred) or a public module in moq-video. It names kernel or
OS objects, never a backend type, so moq keeps its "no backend types in the
public API" rule.

```rust
#[non_exhaustive]
pub enum Native {
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBuf),          // fd on demand, fourcc, modifier, planes
    #[cfg(target_os = "macos")]
    CvPixelBuffer(Surface),  // moq's existing macos::Surface, made public
    #[cfg(target_os = "windows")]
    D3d11(Texture),          // moq's existing d3d11::Texture, made public
    #[cfg(all(target_os = "linux", feature = "nvdec"))]
    Cuda(Cuda),              // moq's existing cuda::Frame, made public
    #[cfg(target_os = "android")]
    HardwareBuffer(HwBuffer),
}

pub struct DmaBuf { /* private exporter; no fd held per frame */ }
impl DmaBuf {
    pub fn fourcc(&self) -> u32;
    pub fn modifier(&self) -> u64;
    pub fn coded_size(&self) -> Size;
    pub fn display_size(&self) -> Size;
    pub fn planes(&self) -> &[Plane];   // { offset: u32, pitch: u32 }
    pub fn export(&self) -> Result<OwnedFd, Error>;   // fresh dup'd descriptor
}
```

The private `crate::frame::Frame` enum (`rs/moq-video/src/frame.rs:23-36`) grows
two cfg-gated variants that feed the public vocabulary: `DmaBuf(dmabuf::Frame)`
under `cfg(all(target_os = "linux", feature = "dmabuf"))` and
`HardwareBuffer(android::HwBuffer)` under `cfg(target_os = "android")`. Both are
additive; `width`/`height`/`to_i420` gain arms, with `to_i420` a CPU download
fallback so software paths keep working. B1 adds a shared `dmabuf` Cargo feature
that `vaapi`, `pipewire`, and `v4l2` each enable, so DMA-BUF producers (PipeWire
capture, V4L2) pull in the variant without depending on `vaapi`.

**PTS through the encode path (base plan B2, moq-changes change 2).** The encode
`Backend` trait takes the presentation timestamp and returns per-packet
timestamps, so pipelined backends (V4L2 M2M, Android MediaCodec) can drain a
frame several inputs old without mis-stamping it.

```rust
fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool)
    -> Result<Vec<Packet>, Error>;

#[non_exhaustive]
pub struct Packet { pub payload: Bytes, pub timestamp: Timestamp }
```

`Timestamp` is `moq_net::Timestamp`. This replaces
`encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>`
(`encode/backend/mod.rs:40`). It is additive in behavior (each existing backend
echoes the current frame's timestamp) and makes encode symmetric with the decode
side, which already carries `Decoded { timestamp, frame }`.

**The decode handle accessor (base plan B3, moq-changes change 3).** A public
accessor on `decode::Frame` beside `into_i420()` (`decode/mod.rs:94-101`):

```rust
impl decode::Frame {
    /// The platform GPU handle when the frame is GPU-resident; `None` for CPU.
    pub fn native(&self) -> Option<Native>;
}
```

`into_i420()` stays as the universal CPU fallback.

**The public registerable Backend trait (base plan B4, moq-changes change 7,
BREAKING, Path B only).** Only pursued if a backend (Android) stays out of moq's
tree. Publishes the `pub(crate)` `Backend` trait as additive-sealed and adds
registration so an external crate can contribute a candidate:

```rust
pub trait Backend: Send {
    fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool)
        -> Result<Vec<Packet>, Error>;
    fn finish(&mut self) -> Result<Vec<Packet>, Error>;
    fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;
    fn name(&self) -> &str;
}

#[non_exhaustive]
pub struct Registration {
    pub name: &'static str,
    pub codecs: &'static [Codec],
    pub open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
}
pub fn register_encoder(reg: Registration);

// Decode is NOT a mirror: the decode Candidate carries a `supports` predicate
// and its opener takes the concrete codec, so one backend serves several codecs.
#[non_exhaustive]
pub struct DecodeRegistration {
    pub name: &'static str,
    pub supports: fn(Codec) -> bool,
    pub open: fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>,
}
pub fn register_decoder(reg: DecodeRegistration);
```

The built-in candidate tables become a `OnceLock<Vec<Candidate>>` seeded from the
built-ins plus the registered slice; `Kind::{Auto,Hardware,Named}` selection
chains it. Note an asymmetry B4 must normalize: on the encode side the built-ins
are two `&[Candidate]` slices (`HARDWARE`/`SOFTWARE`, `encode/backend/mod.rs:68-102`),
but on the decode side `SOFTWARE` is a single `const Candidate`
(`decode/backend/mod.rs:110-114`) chained via `std::iter::once`, so the decode
seeding folds that single const into the `Vec`. No change to the public `Kind`
enum. This is the only breaking item in the whole program and the only one
exclusive to Path B.

**In-tree backends do not need B4.** An in-tree VAAPI, V4L2, or AV1 backend just
adds a `const Candidate` to the existing tables; it needs only B1, B2, and B3.
B4 exists solely for the external-backend path (Android with its NDK
dependencies moq cannot test in CI). Treat B4 as conditional on the Android
placement decision.

## Adaptation conventions (base plan B5, moq house style)

Every leaf follows these, so contributions arrive in moq's shape, not ours. The
evidence is `comparisons/moq-changes.md` sections 3 and the ground rules in
`comparisons/moq-changes.md`.

- No ffmpeg anywhere, including tests. The merge removed it; nothing reintroduces
  it.
- dlopen system libraries (libva, libdrm, V4L2, NDK); link nothing that can fail
  to load. Backends must build on hosts without the hardware and degrade cleanly,
  matching moq-nvenc's compile-everywhere stub.
- Minimal dependencies, crates.io only. release-plz owns versions; no git
  dependencies (our rav1d fork pin and cpal git pin are unacceptable as-is and
  are prerequisites to resolve).
- Timestamps are `moq_net::Timestamp` at boundaries, never `Duration`. Configs
  come from hang's catalog types, not our `config.rs` mirror (which no longer
  compiles against hang 0.19.5). Errors adopt moq's `Error` with additive
  variants.
- Public configs are `#[non_exhaustive]`; audio formats mirror WebCodecs
  `AudioData.format`; no backend type appears in any public API.
- Honest capability contracts: `set_bitrate` succeeds or returns
  `Error::BitrateUnsupported`, never a silent no-op; every encoder supports
  per-frame forced IDR; every backend that can be tested on real hardware ships a
  hardware round-trip test in the style of moq's VideoToolbox and NVENC tests.
- Conventional commits with crate scope and `!` for breaking changes.

## Dependency tree

```
moq main (3a3e0ea8)
|
+-- base branch  moq-upstream/base
|   +-- B1  frame vocabulary (Native enum + Frame variants + moq-frame crate)  [KEYSTONE]
|   +-- B2  PTS-through-encode (Backend::encode timestamp + Packet)
|   +-- B3  decode Frame::native() accessor                        [needs B1]
|   +-- B4  public registerable Backend trait + registration       [needs B1,B2; BREAKING; Path B only]
|   +-- B5  adaptation conventions + moq Error variants            [shared reference]
|
+-- codec leaves (each branches off base)
|   +-- vaapi-decode          [B1,B3; grows moq-vaapi export+VPP]   *shares moq-vaapi*
|   +-- vaapi-encode          [B1,B2; grows moq-vaapi VPP]          *shares moq-vaapi*
|   +-- vtb-mf-decode-surface [B1,B3]
|   +-- v4l2-encode           [B2]
|   +-- v4l2-decode           [B1,B3]
|   +-- android-mediacodec    [B1,B2,B4]                            *Path B decision*
|   +-- av1-software          [B2; rav1d fork resolution]           *prereq*
|   +-- opus-improvements     [independent of base]
|   +-- pcm                   [independent; low value]
|   +-- bitstream-sps-vui     [independent; optional]
|
+-- capture leaves
|   +-- pipewire-dmabuf       [B1]
|   +-- v4l2-camera-enum      [B1 for zero-copy; else independent]
|   +-- libcamera-preencoded  [pre-encoded-source concept buy-in]   *concept gate*
|
+-- render leaf (out-of-tree crate, not a moq source PR)
    +-- moq-video-render      [B1,B3,vtb-mf-decode-surface]
```

## Wave ordering

The tree says what depends on what; the waves say what to attempt in what order,
balancing value against the coordination gates.

- **Wave 0, base.** The RFC for B1, B2, B3, and B5 (one design conversation with
  the maintainer, led by the VAAPI decode motivation and the AV1 offer), then
  land B1, B2, B3, B5. Defer B4 until the Android decision forces it.
- **Wave 1, the high-value zero-copy series.** vaapi-decode and vaapi-encode
  (the largest value and the moq-vaapi growth), vtb-mf-decode-surface, and
  pipewire-dmabuf. All rest on B1 and B3.
- **Wave 2, the remaining backends and capture.** v4l2-encode, v4l2-decode,
  av1-software, v4l2-camera-enum, opus-improvements. opus is independent and can
  slot earlier as relationship-building.
- **Wave 3, the conditional and opportunistic items.** android-mediacodec (after
  the B4 decision), libcamera-preencoded (after the concept buy-in), the
  moq-video-render crate, pcm, bitstream-sps-vui.

## Coordination points (read before authoring any leaf)

Most work is autonomous. These are the only places an agent must stop and defer.

1. **Base API freeze.** No leaf finalizes against a base API that is not yet
   agreed. The signatures in "The frozen base API contract" above are the
   contract; author leaves against them. If a leaf discovers the contract cannot
   express its module (a missing `Native` variant, a needed accessor), it stops
   and files the gap against the base plan (B1/B2/B3/B4), and the base plan owner
   reconciles. Leaves never diverge from the contract unilaterally.
2. **Shared candidate tables.** Every in-tree backend adds a `const Candidate` to
   `encode/backend/mod.rs` or `decode/backend/mod.rs`. These edits are additive
   but touch the same lines, so two leaf PRs will conflict there. Rule: each leaf
   adds only its own candidate; PRs land one at a time and later leaves rebase.
   Do not refactor the tables from a leaf.
3. **The shared moq-vaapi crate (see coordination point 11 for the reality).**
   moq-vaapi already ships an encoder, surface export, and a VPP wrapper; what it
   lacks is a decode stack. vaapi-decode owns the decode contribution to the
   moq-vaapi repo (the largest single piece, a re-vendor against moq-vaapi's
   diverged bindgen types, not a drop-in of our cros-codecs decoder), and
   vaapi-encode contributes the validation and hardware-correctness fixes to
   moq-vaapi's existing encode path. Both target the external moq-vaapi repo for
   the VA-layer work and the monorepo for the moq-video backend wiring. If the two
   are authored in parallel, they coordinate on the shared moq-vaapi types rather
   than one growing export or VPP the other duplicates.
4. **rav1d fork resolution.** av1-software is blocked until the rav1d git-fork pin
   is resolved (published to crates.io, moved to a released rav1d, or vendored).
   This is a prerequisite, not something the leaf agent decides alone; flag it and
   proceed only once resolved.
5. **The pre-encoded-source concept.** libcamera-preencoded needs a new moq
   `publish_preencoded` concept (a capture source that emits an already-encoded
   bitstream plus a catalog config). This needs maintainer buy-in before
   implementation; it is a design conversation, not a silent addition.
6. **The B4 breaking change.** Publishing the `Backend` trait is the only breaking
   change and is worth it only if moq wants out-of-tree backends. It gates on the
   Android placement decision (in-tree with its NDK build cost, or external over
   the registration API). Do not open B4 as a PR until that decision is made with
   the maintainer.
7. **Per-segment transcoding and FETCH.** The maintainer's stated moq codec
   direction is per-group (per-segment) transcoding with FETCH support: a FETCH
   for group 45 of a lower rendition triggers transcoding that one group from the
   source (held in relay memory, possibly disk) down to, for example, 360p, with
   custom per-GOP rate control. moq-transcode already owns this. Our contributions
   plug into it with no integration work, because moq-transcode drives encoders
   only through the public `encode::{Kind, Config, Encoder}` front end: it selects
   by `Kind`, sets a per-rung CBR target through `Config.bitrate` at construction,
   forces an IDR per group, and builds a fresh encoder per fetched group; it never
   uses `rate::Control`. Our zero-copy VAAPI decode into VPP scale into VAAPI
   encode is the Intel and AMD analog of moq-transcode's NVDEC to NVENC path, and
   rav1e with rav1d is the software fallback, so both slot in by `Kind` selection.
   The RULE this imposes on every encoder contribution: expose per-segment
   rate-control primitives (an honest `set_bitrate` with no forced-IDR side effect,
   a per-encode target-bitrate or QP knob, forced IDR per GOP, and cheap
   reconfigure or session reuse between groups) and defer the rate-control POLICY
   to moq-transcode. Never embed a streaming rate controller in a backend. Note
   the per-group re-open cost, which the encode plans must address: rav1e is cheap,
   VAAPI opens a VA context, and V4L2 is expensive (full device open plus REQBUFS
   plus STREAMON), so the VAAPI and V4L2 encode plans add a session-reuse path
   rather than constructing fresh per group.
8. **Licensing and provenance of ported FFI.** The VAAPI, V4L2, Vulkan, and Metal
   code carries libva, DRM, and graphics-API bindings, each with its own license,
   and some is ported from cros-libva and cros-codecs. Every contribution states
   the provenance and license of what it vendors or binds, and matches moq's
   existing posture (moq-vaapi already ships `LICENSE.libva` and
   `LICENSE.cros-codecs`). Do not introduce a license-incompatible dependency.
9. **CI hardware gating.** Most of this cannot run in moq CI: there is no Intel or
   AMD GPU, no Raspberry Pi, and no Android device on the runners. Every hardware
   path ships a cfg-gated round-trip test modeled on moq's own `round_trip` helper
   (`decode/backend/nvdec.rs:513`, which is cfg-gated rather than `#[ignore]`),
   plus a reproducible host-validation script, and each plan states plainly what
   CI can and cannot verify. A backend that only compiles in CI is explicitly
   marked unvalidated, as moq's own VAAPI encoder is today.
10. **Semver across the fan.** B1 through B4 change moq-video's public surface, and
    a string of leaf PRs follow. Agree the versioning expectation with the
    maintainer up front (one base bump, then additive leaves) so the fan of PRs
    does not thrash the crate version.
11. **The moq-vaapi PR target.** `moq-vaapi` is a separate external crate
    (crates.io 0.0.2, the maintainer's own org, github.com/moq-dev/vaapi), not a
    crate in the moq monorepo. It already ships an encoder, `vaExportSurfaceHandle`
    surface export, and a VPP `VAProcPipelineParameterBuffer` wrapper, but no
    decode stack, and its types are a diverged bindgen trim of cros-libva and
    cros-codecs that does not use the `cros-codecs` crate our decoder is written
    against. The VAAPI VA-layer work (a decode stack, plus any export or VPP
    additions) is a PR to that repo, and the dependency-spine decision (re-vendor
    cros-codecs decode into moq-vaapi's style, add a `cros-codecs` dependency, or
    another route) is a maintainer conversation, not a leaf-agent decision. The
    moq-video backend wiring that consumes it is a separate monorepo PR.

## How to work a plan autonomously (agent runbook)

Each plan file is written so a capable but non-expert model can execute it
end to end. The steps:

1. Read this overview in full, especially the frozen base API contract, the
   adaptation conventions, and the coordination points.
2. Read your assigned plan file.
3. Read the comparison sections and maps it references (in `comparisons/`). These
   carry the file:line evidence for both our code and moq's.
4. Create your branch off the base branch: `moq-upstream/<plan-name>`.
5. Implement the plan's steps in order, coding against the frozen base API and
   following the adaptation conventions. Port the referenced iroh-live source,
   adapting it to moq's vocabulary; do not copy our trait glue or our config
   mirror.
6. Write tests in moq's style, including a hardware-gated round-trip test where
   the plan calls for one.
7. Run the plan's acceptance checklist. Open the PR against moq with the plan's
   PR description, targeting the base branch until base has merged, then main.

You may do autonomously: the implementation, the vocabulary adaptation, the
tests, and the PR. You must stop and flag (a coordination point above) if: the
base API cannot express your module; you would need to change a shared file
(a candidate table, moq-vaapi) beyond adding your own additive entry; you need a
public moq API not in the contract; a prerequisite (rav1d, the pre-encoded
concept, the B4 decision) is unresolved; or a hardware test needs hardware you
do not have (say so in the PR and mark the test `#[ignore]` with the reason).

## Plan template

Every base and leaf plan uses this structure, so any agent knows where to look.

```
# <id>. <title>

Branch: moq-upstream/<name>          PR target: base branch, then moq main
Depends on: <base plans / other leaves / external prerequisites>
Path: A (in-tree) | B (external) | independent
Size: S | M | L | XL (moq-side authoring estimate)

## Goal
One paragraph: what this PR delivers to moq.

## Evidence
Links into comparisons/ and maps/ for our code and moq's, with file:line.

## moq API consumed
Which frozen-contract types/traits this uses (Native, Packet, native(), etc.).

## Source to port
The iroh-live files (path + LOC), and what carries over vs what is dropped.

## Target in moq
The moq files and crates this adds or changes, with file:line anchors.

## Implementation steps
Ordered, each small enough to review, each a what-and-why.

## Tests
The moq-style tests, including any hardware-gated round trip.

## Adaptation notes
Our-vocabulary to moq-vocabulary specifics (timestamp, error, no ffmpeg, dlopen,
feature gate) beyond the shared conventions.

## Coordination
Any point where this plan must defer (from the list above).

## Acceptance checklist
The gate for calling the PR done.
```

## Git and PR model

This is a contribution to an external project, so the PR model is not the private
base-branch-then-leaf-branch model a single worktree would use. A pull request to
upstream moq targets moq `main`, not a branch on our fork, and a leaf opened
before the base API merges would render as the union of the base diff plus the
leaf diff, which cannot be reviewed in isolation. The model is therefore:

- The base API (B1, B2, B3, B5) lands on moq `main` first, as its own PR or a
  short dependency-ordered series, B1 first. This is a hard serializing gate:
  Wave 0 must merge upstream before any leaf PR can be reviewed. B4 is a later,
  separate PR, pursued only on the Path B decision.
- Each leaf is then a normal PR against moq `main`, rebased onto the merged base.
  Leaves do not depend on each other, subject to the shared-file coordination
  points (the candidate tables and the moq-vaapi work). The moq-vaapi VA-layer
  work is a separate PR to the `moq-dev/vaapi` repo, not the monorepo (see
  coordination point 11).
- Locally, a base integration branch off moq main is still useful so leaves can
  compile against the proposed API before base merges, but that is a development
  convenience, not the PR path. Land order follows the waves.
- Opening PRs, and any push to moq or moq-vaapi, happens only with explicit human
  authorization; the plans produce the branches and PR descriptions, they do not
  self-publish.

## Plan index

Base (in `base/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| B1 frame-vocabulary | public `Native` enum + `Frame` variants + `moq-frame` crate | none (keystone) | Both | M-L |
| B2 pts-through-encode | timestamp arg + `Packet` on `Backend::encode` | none | Both | S-M |
| B3 decode-native-accessor | `decode::Frame::native()` | B1 | Both | S |
| B4 backend-trait-registration | public `Backend` + `register_encoder/decoder` | B1, B2 | B only (breaking) | M |
| B5 adaptation-conventions | moq `Error` variants + the house-style checklist | none | shared | S |

Codec leaves (in `codec/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| vaapi-decode | VAAPI H.264/H.265 decode exporting DMA-BUF; grows moq-vaapi | B1, B3 | A | L-XL |
| vaapi-encode | VAAPI encode replacement, DMA-BUF input, VPP, honest set_bitrate | B1, B2 | A | L |
| vtb-mf-decode-surface | VideoToolbox and Media Foundation decode retain surface | B1, B3 | A | S-M |
| v4l2-encode | V4L2 M2M encode (Pi and embedded), stride handling | B2 | A | L |
| v4l2-decode | V4L2 M2M decode | B1, B3 | A | M-L |
| android-mediacodec | Android MediaCodec encode and decode, HardwareBuffer | B1, B2, B4 | B | L |
| av1-software | rav1e encode and rav1d decode | B2, rav1d pin | A | M-L |
| opus-improvements | runtime set_bitrate, pre-skip, FEC/PLC groundwork, remix | none | independent | S-M |
| pcm | `Codec::Pcm` offer | none | independent | S |
| bitstream-sps-vui | SPS VUI low-latency patcher as an optional pass | none | independent | S |

Capture leaves (in `capture/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| pipewire-dmabuf | PipeWire DMA-BUF zero-copy capture delivery | B1 | A | M |
| v4l2-camera-enum | V4L2 camera capture and device enumeration | B1 for EXPBUF zero-copy, else independent | A | M |
| libcamera-preencoded | libcamera raw and on-device H.264 pre-encoded source | pre-encoded concept | independent | M |

Render leaf (in `render/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| moq-video-render | out-of-tree renderer crate over public handles | B1, B3, vtb-mf-decode-surface | Both (out-of-tree) | 0 upstream |

## Provenance

`comparisons/` holds the evidence base: the codec, capture, audio, pub/sub,
zero-copy, and trait comparisons, the moq-side change design (`moq-changes.md`),
the iroh-live code map, the moq inventory, and the `maps/` of both codebases.
Start at `comparisons/0-index.md`, which carries the consolidated capability
matrix with inline links into the detailed sections and the plan each verdict
feeds. Read a comparison for the reasoning and evidence behind a plan; read a
plan for what to build. The broader refactor analysis that these were drawn from
(the iroh-live cut plan, the room-layer redesign, and the whole-refactor summary,
which reach beyond this codec and capture campaign) is preserved under
`analysis/`.
