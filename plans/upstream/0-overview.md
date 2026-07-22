# Upstream campaign: contribute iroh-live's media stack to moq

> Campaign: upstream | Kind: overview | Read this in full before touching any
> plan. The campaign prompt is `prompt.md`; the branch registry is
> `../branches.md`; the evidence is `comparisons/` (start at
> `comparisons/0-index.md`).

This directory is the execution kit for upstreaming iroh-live's codec, capture,
audio-device, and render code into moq, either as new backends and crates or as
improvements to moq's existing code. The output is a series of pull requests
against moq (and moq-vaapi): one base PR series that everything depends on, then
a fan of leaf PRs, each with a paired iroh-live counterpart branch that adopts
the contribution and cuts the local code it replaces (see `counterpart/` and the
paired-branch model in `../prompt-base.md`). moq is a single codebase
(`/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`); the VA layer lives in the external
`moq-dev/vaapi` repo.

## Goal and scope

Move every piece of iroh-live's owned media code that moq lacks or implements
more weakly into moq, so iroh-live can delete its parallel stack and consume
moq's native media crates. `DISPOSITION.md` is the standing register giving
every iroh-live media module a fate (upstream-ours, adopt-theirs, keep, defer,
or drop); nothing is dropped silently, and any module without a plan here has an
explicit row there.

In scope:

- The base API changes (`base/`), the VAAPI, V4L2, Android, and bitstream codec
  work (`codec/`), the Opus, PCM, and audio-device work (`audio/`), the
  PipeWire, V4L2, and libcamera capture sources (`capture/`), and the render
  crates (`render/`).
- The audio device layer is part of this campaign: the `audio_backend` AEC
  engine and the playback sink unify into moq-audio behind features
  (`audio/audio-device-unify.md`). Audio and video device I/O solve the same
  problem for moq and are not treated as a separate effort.
- The iroh-live pair side of every leaf: the counterpart branches that switch
  the dependency and delete the replaced code (`counterpart/`, ledger in
  `cut-plan.md`).
- For the backends where moq's implementation wins and iroh-live adopts theirs
  (openh264, VideoToolbox encode, NVENC, NVDEC, Media Foundation, macOS and
  Windows capture), porting our fixes and improvements upstream before the local
  code is cut (`capture/parity-ports.md` and the per-module DISPOSITION rows).

Out of scope: the room, pub/sub, and adaptive alignment work, which is the
`plans/align-to-moq/` campaign. Deferred, not dropped: AV1 (rav1e is too slow
and the rav1d git-fork dependency too heavy to carry now; `codec/av1-software.md`
stays as a deferred plan and iroh-live rips out its own AV1 backend meanwhile).

A first-class constraint throughout: contributions that add heavy dependencies
to moq land feature-gated so moq's default and relay builds stay light. Heavy
importer and GPU dependencies sit behind non-default features; the crate itself
can still be a normal workspace member.

## Strategy: one base series, then a fan of leaves

The whole program rests on a small set of additive API changes to moq-video
(a public GPU-frame vocabulary, a presentation-timestamp argument on the encode
path, a decode-side handle accessor). Every zero-copy backend, the renderer, and
the pipelined encoders depend on that vocabulary. So:

- A **base branch** (`up/base` in the moq worktree) off moq main, carrying the
  base plans B1 through B5. Nothing that consumes the new vocabulary is final
  until the base API shape is agreed upstream.
- A **fan of leaf branches** (`up/<name>`), each cut from the base branch, one
  per plan. Leaves are independent of each other and can be authored and
  reviewed in parallel, except at the coordination points below.
- A **paired iroh-live branch** per leaf that has a cut side (`../branches.md`
  lists the pairs): same name, path dep on the moq branch during development,
  git branch dep at handoff, carrying the counterpart deletion.

PRs land base first, then leaves rebased onto the merged result.

## The frozen base API contract

This is the single authority every leaf codes against. It is lifted from
`comparisons/moq-changes.md` sections 1 and 2, which cite the moq source each
item changes. A leaf must treat these signatures as fixed. If a leaf finds the
contract insufficient for its module, it does not improvise a different API: it
stops and files the gap against the relevant base plan (see Coordination).

**The public GPU-frame vocabulary (base plan B1, moq-changes change 1).** A new
public, closed-but-non-exhaustive enum of concrete OS handles, home a public
module in moq-video (a separate `moq-frame` crate would create a dependency
cycle). It names kernel or OS objects, never a backend type, so moq keeps its
"no backend types in the public API" rule.

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

The built-in candidate tables become a `Mutex<Vec<Candidate>>` seeded from the
built-ins plus the registered slice; `Kind::{Auto,Hardware,Named}` selection
chains it. Note an asymmetry B4 must normalize: on the encode side the built-ins
are two `&[Candidate]` slices (`HARDWARE`/`SOFTWARE`, `encode/backend/mod.rs:68-102`),
but on the decode side `SOFTWARE` is a single `const Candidate`
(`decode/backend/mod.rs:110-114`) chained via `std::iter::once`, so the decode
seeding folds that single const into the `Vec`. No change to the public `Kind`
enum. This is the only breaking item in the whole program and the only one
exclusive to Path B.

**In-tree backends do not need B4.** An in-tree VAAPI or V4L2 backend just adds
a `const Candidate` to the existing tables; it needs only B1, B2, and B3. B4
exists solely for the external-backend path (Android with its NDK dependencies
moq cannot test in CI). Treat B4 as conditional on the Android placement
decision.

## Adaptation conventions (base plan B5, moq house style)

Every leaf follows these, so contributions arrive in moq's shape, not ours. The
evidence is `comparisons/moq-changes.md` section 3 and its ground rules.

- No ffmpeg anywhere, including tests.
- dlopen system libraries (libva, libdrm, V4L2, NDK); link nothing that can fail
  to load. Backends must build on hosts without the hardware and degrade
  cleanly, matching moq-nvenc's compile-everywhere stub.
- Minimal dependencies, crates.io only. release-plz owns versions; no git
  dependencies (our rav1d fork pin and cpal git pin are unacceptable as-is).
- Timestamps are `moq_net::Timestamp` at boundaries, never `Duration`. Configs
  come from hang's catalog types, not our `config.rs` mirror. Errors adopt moq's
  `Error` with additive variants.
- Public configs are `#[non_exhaustive]`; audio formats mirror WebCodecs
  `AudioData.format`; no backend type appears in any public API.
- Honest capability contracts: `set_bitrate` succeeds or returns
  `Error::BitrateUnsupported`, never a silent no-op; every encoder supports
  per-frame forced IDR; every backend that can be tested on real hardware ships
  a hardware round-trip test in the style of moq's VideoToolbox and NVENC tests.
- Heavy dependencies sit behind non-default features (the dependency-weight
  constraint from Goal and scope).
- Conventional commits with crate scope and `!` for breaking changes.

## Dependency tree

```
moq main (3a3e0ea8)
|
+-- base branch  up/base
|   +-- B1  frame vocabulary (public Native enum + Frame variants)  [KEYSTONE]
|   +-- B2  PTS-through-encode (Backend::encode timestamp + Packet)
|   +-- B3  decode Frame::native() accessor                        [needs B1]
|   +-- B4  public registerable Backend trait + registration       [needs B1,B2; BREAKING; Path B only]
|   +-- B5  adaptation conventions + moq Error variants            [shared reference]
|
+-- codec leaves (each cut from up/base)
|   +-- vaapi-decode          [B1,B3; grows moq-vaapi decode]       *shares moq-vaapi*
|   +-- vaapi-encode          [B1,B2; validates moq-vaapi encode]   *shares moq-vaapi*
|   +-- vtb-mf-decode-surface [B1,B3]
|   +-- v4l2-encode           [B2]
|   +-- v4l2-decode           [B1,B3]
|   +-- android-mediacodec    [B1,B2,B4]                            *Path B decision*
|   +-- bitstream-sps-vui     [independent]
|   +-- av1-software          DEFERRED
|
+-- audio leaves
|   +-- opus-improvements     [independent of base]
|   +-- pcm                   [independent; required; hang catalog variant same branch]
|   +-- audio-device-unify    [independent; playback sink + AEC into moq-audio behind features]
|
+-- capture leaves
|   +-- pipewire-dmabuf       [B1]
|   +-- v4l2-camera-enum      [B1 for zero-copy; else independent]
|   +-- libcamera-preencoded  REQUIRED [open question: publish_preencoded shape]
|   +-- parity-ports          ports of our fixes into moq's adopted backends
|
+-- render leaves
    +-- moq-video-render      normal workspace member; wgpu AND gles behind features [B1,B3,vtb-mf-decode-surface]
    +-- moq-egui              moq-media-egui moves to moq as moq-egui [moq-video-render]
```

Each leaf with a cut side has a paired iroh-live `up/<name>` branch; the pair
table is `../branches.md` and the cut guidance is `counterpart/` plus
`cut-plan.md`.

## Wave ordering

- **Wave 0, base.** The RFC for B1, B2, B3, and B5 (one design conversation
  upstream, led by the VAAPI decode motivation and the embedded and Raspberry Pi
  story), then land B1, B2, B3, B5. Defer B4 until the Android decision forces
  it.
- **Wave 1, the high-value zero-copy series.** vaapi-decode and vaapi-encode
  (the largest value and the moq-vaapi growth), vtb-mf-decode-surface, and
  pipewire-dmabuf. All rest on B1 and B3.
- **Wave 2, the remaining backends, capture, and audio.** v4l2-encode,
  v4l2-decode, v4l2-camera-enum, opus-improvements, pcm, audio-device-unify,
  and parity-ports. opus is independent and can slot earlier as
  relationship-building.
- **Wave 3, the larger and conditional items.** moq-video-render, moq-egui,
  libcamera-preencoded, android-mediacodec (after the B4 decision), and
  bitstream-sps-vui.
- **Deferred.** av1-software: revisit when a use case needs it and the rav1d
  dependency is resolvable.

## Coordination points (read before authoring any leaf)

Most work is autonomous. These are the only places an agent must stop and defer.

1. **Base API freeze.** No leaf finalizes against a base API that is not yet
   agreed. The signatures above are the contract; author leaves against them. If
   a leaf discovers the contract cannot express its module, it stops and files
   the gap against the base plan (B1/B2/B3/B4), and the coordinator reconciles.
   Leaves never diverge from the contract unilaterally.
2. **Shared candidate tables.** Every in-tree backend adds a `const Candidate`
   to `encode/backend/mod.rs` or `decode/backend/mod.rs`. These edits are
   additive but touch the same lines. Rule: each leaf adds only its own
   candidate; PRs land one at a time and later leaves rebase. Do not refactor
   the tables from a leaf.
3. **The shared moq-vaapi crate (see point 11 for the repo reality).** moq-vaapi
   already ships an encoder, surface export, and a VPP wrapper; what it lacks is
   a decode stack. vaapi-decode owns the decode contribution to the moq-vaapi
   repo (a re-vendor against moq-vaapi's diverged bindgen types, not a drop-in
   of our cros-codecs decoder), and vaapi-encode contributes the validation and
   hardware-correctness fixes to moq-vaapi's existing encode path. Both target
   the external moq-vaapi repo for the VA-layer work and the monorepo for the
   moq-video backend wiring. If authored in parallel, they coordinate on the
   shared moq-vaapi types rather than one growing export or VPP the other
   duplicates.
4. **AV1 is deferred**, so the rav1d fork resolution is not a gate for this
   series. Do not spend the series on it.
5. **Pre-encoded capture is required; the API shape is an open question.**
   Perfect libcamera on-device H.264 support is a committed outcome of this
   series, not contingent on the API being accepted exactly as sketched. Open
   question: the exact `publish_preencoded` shape, discussed in
   `capture/libcamera-preencoded.md`; current proposal: mirror `publish_capture`
   minus `encode::Options`, taking a bitstream source plus a catalog config.
   Bring it upstream as a design conversation.
6. **The B4 breaking change.** Publishing the `Backend` trait is the only
   breaking change and is worth it only if moq wants out-of-tree backends. Open
   question: the Android placement (in-tree with its NDK build cost, or external
   over the registration API), discussed in `codec/android-mediacodec.md`;
   current proposal: external (Path B), which is what B4 exists for. Do not open
   B4 as a PR until the placement is settled upstream.
7. **Per-segment transcoding and FETCH.** moq's stated codec direction is
   per-group (per-segment) transcoding with FETCH support: a FETCH for group 45
   of a lower rendition transcodes that one group from the source (relay memory,
   possibly disk) down to, for example, 360p, with custom per-GOP rate control.
   moq-transcode owns this policy and drives encoders only through the public
   `encode::{Kind, Config, Encoder}` front end: it selects by `Kind`, sets a
   per-rung CBR target through `Config.bitrate` at construction, forces an IDR
   per group, and builds a fresh encoder per fetched group; it never uses
   `rate::Control`. The rule this imposes on every encoder contribution: expose
   per-segment rate-control primitives (an honest `set_bitrate` with no
   forced-IDR side effect, a per-encode target-bitrate or QP knob, forced IDR
   per GOP, and cheap reconfigure or session reuse between groups) and defer the
   rate-control policy to moq-transcode. Never embed a streaming rate controller
   in a backend. Per-group re-open cost matters: rav1e is cheap, VAAPI opens a
   VA context, and V4L2 is expensive (full device open plus REQBUFS plus
   STREAMON), so the VAAPI and V4L2 encode plans add a session-reuse path.
8. **Licensing and provenance of ported FFI.** The VAAPI, V4L2, Vulkan, and
   Metal code carries libva, DRM, and graphics-API bindings, and some is ported
   from cros-libva and cros-codecs. Every contribution states the provenance and
   license of what it vendors or binds, matching moq's existing posture
   (moq-vaapi ships `LICENSE.libva` and `LICENSE.cros-codecs`). Do not introduce
   a license-incompatible dependency.
9. **CI hardware gating.** Most of this cannot run in moq CI: no Intel or AMD
   GPU, no Raspberry Pi, no Android device on the runners. Every hardware path
   ships a cfg-gated round-trip test modeled on moq's own `round_trip` helper
   (`decode/backend/nvdec.rs:513`, cfg-gated rather than `#[ignore]`), plus a
   reproducible host-validation script, and each plan states plainly what CI can
   and cannot verify. A backend that only compiles in CI is explicitly marked
   unvalidated, as moq's own VAAPI encoder is today.
10. **Semver across the fan.** B1 through B4 change moq-video's public surface,
    and a string of leaf PRs follow. Agree the versioning expectation upstream
    up front (one base bump, then additive leaves) so the fan does not thrash
    the crate version.
11. **The moq-vaapi PR target.** `moq-vaapi` is a separate external crate
    (crates.io 0.0.2, github.com/moq-dev/vaapi), not a crate in the moq
    monorepo. It ships an encoder, `vaExportSurfaceHandle` surface export, and a
    VPP `VAProcPipelineParameterBuffer` wrapper, but no decode stack, and its
    types are a diverged bindgen trim of cros-libva and cros-codecs that does
    not use the `cros-codecs` crate our decoder is written against. The VA-layer
    work is a PR to that repo; the moq-video backend wiring is a separate
    monorepo PR. Open question: the dependency spine (re-vendor cros-codecs
    decode into moq-vaapi's style, add a `cros-codecs` dependency, or another
    route), discussed in `codec/vaapi-decode.md`; current proposal: re-vendor
    into moq-vaapi's style, matching how the crate already treats cros-libva.
12. **Pair-side cuts follow the counterpart plans.** No iroh-live deletion
    happens outside a paired `up/<name>` branch whose counterpart plan's proof
    passes; `cut-plan.md` and `DISPOSITION.md` are updated with every cut.

## How to work a plan autonomously (agent runbook)

1. Read this overview in full, especially the frozen base API contract, the
   adaptation conventions, and the coordination points.
2. Read your assigned plan file, then the comparison sections and maps it
   references.
3. Create your branch and worktree per the branching model in
   `../prompt-base.md`: moq-side `up/<plan-name>` cut from `up/base`, and the
   paired iroh-live branch when your plan has a cut side.
4. Implement the plan's steps in order, coding against the frozen base API and
   following the adaptation conventions. Port the referenced iroh-live source,
   adapting it to moq's vocabulary; do not copy our trait glue or our config
   mirror.
5. Write tests in moq's style, including a hardware-gated round-trip test where
   the plan calls for one.
6. Run the plan's acceptance checklist and write the PR description. Opening the
   PR is a human action.

You may do autonomously: the implementation, the vocabulary adaptation, the
tests, and the PR text. You must stop and flag (a coordination point above) if:
the base API cannot express your module; you would need to change a shared file
beyond adding your own additive entry; you need a public moq API not in the
contract; an open question your plan names is unresolved; or a hardware test
needs hardware you do not have (say so in the PR text and mark the test with the
reason).

## Plan template

```
# <id>. <title>

> Campaign: upstream | Kind: leaf plan | Branch: up/<name> |
> PR target: moq monorepo | moq-dev/vaapi | Read ../0-overview.md first.
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
Our-vocabulary to moq-vocabulary specifics beyond the shared conventions.

## Counterpart
The paired iroh-live branch's cut, or "none" for improvement-only leaves.

## Coordination
Any point where this plan must defer (from the list above).

## Acceptance checklist
The gate for calling the PR done.
```

## Git and PR model

This is a contribution to an external project. A pull request to upstream moq
targets moq `main`, not a branch on our fork, and a leaf opened before the base
API merges would render as the union of the base diff plus the leaf diff, which
cannot be reviewed in isolation. Therefore:

- The base API (B1, B2, B3, B5) lands on moq `main` first, as its own PR or a
  short dependency-ordered series, B1 first. This is a hard serializing gate:
  Wave 0 must merge upstream before any leaf PR can be reviewed. B4 is a later,
  separate PR, pursued only on the Path B decision.
- Each leaf is then a normal PR against moq `main`, rebased onto the merged
  base. The moq-vaapi VA-layer work is a separate PR to the `moq-dev/vaapi`
  repo (coordination point 11).
- Locally, `up/base` exists in both worktrees so leaves and their iroh-live
  pairs compile against the proposed API before base merges; that is a
  development convenience, not the PR path. Branch and worktree mechanics are in
  `../prompt-base.md`; the registry is `../branches.md`.
- Opening PRs, and any push to moq or moq-vaapi, happens only with explicit
  human authorization; the plans produce branches and PR descriptions, they do
  not self-publish.

## Plan index

Base (in `base/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| B1 frame-vocabulary | public `Native` enum + `Frame` variants | none (keystone) | Both | M-L |
| B2 pts-through-encode | timestamp arg + `Packet` on `Backend::encode` | none | Both | S-M |
| B3 decode-native-accessor | `decode::Frame::native()` | B1 | Both | S |
| B4 backend-trait-registration | public `Backend` + `register_encoder/decoder` | B1, B2 | B only (breaking) | M |
| B5 adaptation-conventions | moq `Error` variants + the house-style checklist | none | shared | S |

Codec leaves (in `codec/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| vaapi-decode | VAAPI H.264/H.265 decode exporting DMA-BUF; grows moq-vaapi | B1, B3 | A | L-XL |
| vaapi-encode | VAAPI encode validation, DMA-BUF input, VPP, honest set_bitrate | B1, B2 | A | L |
| vtb-mf-decode-surface | VideoToolbox and Media Foundation decode retain surface | B1, B3 | A | S-M |
| v4l2-encode | V4L2 M2M encode (Pi and embedded), stride handling | B2 | A | L |
| v4l2-decode | V4L2 M2M decode | B1, B3 | A | M-L |
| android-mediacodec | Android MediaCodec encode and decode, HardwareBuffer | B1, B2, B4 | B | L |
| bitstream-sps-vui | SPS VUI low-latency patcher as an optional pass | none | independent | S |
| av1-software | rav1e encode and rav1d decode | DEFERRED | A | M-L |

Audio leaves (in `audio/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| opus-improvements | runtime set_bitrate, pre-skip, FEC/PLC groundwork, remix | none | independent | S-M |
| pcm | `Codec::Pcm` in moq-audio + the hang catalog variant, same branch | none | independent | S |
| audio-device-unify | playback sink + AEC engine into moq-audio behind features | none | independent | L |

Capture leaves (in `capture/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| pipewire-dmabuf | PipeWire DMA-BUF zero-copy capture delivery | B1 | A | M |
| v4l2-camera-enum | V4L2 camera capture and device enumeration | B1 for EXPBUF zero-copy, else independent | A | M |
| libcamera-preencoded | libcamera raw and on-device H.264 pre-encoded source (required) | open question: publish_preencoded shape | independent | M |
| parity-ports | our fixes ported into moq's adopted backends before the local cut | per-item | A | S-M |

Render leaves (in `render/`):

| Plan | Delivers | Depends on | Path | Size |
|---|---|---|---|---|
| moq-video-render | in-tree render crate: zero-copy importers over `Native`, wgpu and GLES backends both behind feature flags, heavy deps non-default | B1, B3, vtb-mf-decode-surface | A | L-XL |
| moq-egui | `moq-media-egui` moves to moq as `moq-egui` over moq-video-render | moq-video-render | A | M |

## Directory map

- `base/`, `codec/`, `audio/`, `capture/`, `render/`: the moq-side plans.
- `counterpart/`: the iroh-live pair-side cut plans (codec-remove,
  capture-remove, render-adopt); `cut-plan.md` is the deletion ledger behind
  them.
- `DISPOSITION.md`: the standing register of every iroh-live media module and
  its fate.
- `comparisons/`: the evidence base (start at `comparisons/0-index.md`, the
  consolidated capability matrix with inline links). `comparisons/moq-changes.md`
  is the moq-side change design the contract is lifted from.
- `reviews/`: the standing adversarial reviews; findings already folded into the
  plans.
- `analysis/`: the broader refactor analysis these plans were drawn from,
  preserved for context.
