# Upstreaming iroh-live's media stack to moq

> Campaign: upstream (media stack) | Kind: overview | Read this first, then
> `zerocopy.md` (the decisive asset) and `prompt.md` (how to work). moq is a
> single codebase at `/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`; the VA layer is
> the external `moq-dev/vaapi` repo.

This directory is the execution kit for moving iroh-live's owned media code
(codecs, capture, audio device layer, and the renderer) into moq, either as new
backends and crates or as improvements to moq's existing code, so iroh-live can
delete its parallel stack and consume moq's native media crates.

The work is organized as one doc per module. Each `modules/*.md` leads with a
VERDICT and carries all the detail for that capability: the moq-side
contribution, the iroh-live removal, the evidence, and the coordination gates.

## Zero-copy is the point

Read `zerocopy.md`. Zero-copy is where iroh-live is decisively ahead of moq, and
preserving every zero-copy path through the upstreaming is the hard constraint
that orders the campaign. iroh-live has the only decode-to-render GPU path in
either codebase, across three platforms and two graphics APIs (Vulkan/DMA-BUF,
EGL/GLES, Metal). That path only survives the decoder deletions if the renderer
(`modules/render-video-render.md`) and the decode-surface retention
(`modules/codec-decode-surface.md`) land first. Every module doc carries a
`Zero-copy:` line; the zero-copy-critical modules are VAAPI encode/decode, V4L2,
the decode-surface retention, PipeWire capture, Android, and above all the
renderer.

## The four verdicts

Every module resolves to one of these, stated as its first line:

- **move from iroh-live to moq** - moq lacks it; we contribute ours (VAAPI,
  V4L2, Android, PCM, libcamera, PipeWire, X11, the renderer, the audio device
  layer, the bitstream low-latency pass).
- **apply improvements from iroh-live to moq** - moq has it; we contribute our
  deltas upstream, then use moq's (Opus, the VideoToolbox and Media Foundation
  decode-surface retention).
- **use moq version, remove iroh-live version** - moq's wins; we adopt and
  delete ours (openh264, VideoToolbox encode, macOS and Windows capture, the
  dispatch table, Annex-B tooling, the config mirror, the resampler, and the
  redundant nokhwa and xcap fallbacks).
- **remove from iroh-live for now** - deferred; dropped locally with no moq
  contribution this series (AV1: rav1e too slow, the rav1d git-fork dependency
  too heavy for moq's crates.io-only rule).

`comparison.md` is the master table of every module with its verdict, the
iroh-live-versus-moq contrast, and links to both the module doc and the
evidence.

## Strategy: one base series, then a fan of modules

The zero-copy modules all rest on a small set of additive moq-video API changes
(the base plans in `base/`). So the campaign lands the base first, then the
modules, each rebased onto the merged base. A module with a cut side has a paired
iroh-live branch that adopts the contribution and deletes the local code; the
mechanics are in `prompt.md`.

The base contract (full detail in `base/`):

- **B1** a public GPU-frame vocabulary: a `#[non_exhaustive]` `Native` enum of
  concrete OS handles (DmaBuf, CvPixelBuffer, D3d11, Cuda, HardwareBuffer) with a
  `DmaBuf` accessor, plus cfg-gated `Frame` variants behind a shared `dmabuf`
  feature. The keystone; every GPU module needs it. See
  `base/B1-frame-vocabulary.md`.
- **B2** the presentation timestamp threaded through `Backend::encode` returning
  per-packet `Packet { payload, timestamp }`, so pipelined encoders (V4L2 M2M,
  Android) do not mis-stamp. See `base/B2-pts-through-encode.md`.
- **B3** a `decode::Frame::native() -> Option<Native>` accessor beside
  `into_i420()`, so the renderer imports GPU surfaces. See
  `base/B3-decode-native-accessor.md`.
- **B4** a public registerable `Backend` trait plus registration, the only
  breaking change, needed only for the external Android path. See
  `base/B4-backend-trait-registration.md`.
- **B5** the adaptation conventions (no ffmpeg, dlopen system libraries,
  crates.io only, `moq_net::Timestamp`, hang catalog types, honest `set_bitrate`,
  heavy deps behind non-default features). See `base/B5-adaptation-conventions.md`.

## Wave ordering

- **Wave 0, base.** Land B1, B2, B3, B5 on moq main first (a hard serializing
  gate); defer B4 until the Android placement forces it.
- **Wave 1, the zero-copy series.** VAAPI encode and decode, the decode-surface
  retention, PipeWire capture, and the renderer. These carry the campaign.
- **Wave 2, the remaining backends, capture, and audio.** V4L2 encode and
  decode, V4L2 camera, Opus, PCM, the audio device layer, the adopt-theirs
  removals.
- **Wave 3, the larger and conditional items.** libcamera (pre-encoded API is an
  open question), Android (after the B4 decision), moq-egui, the bitstream pass.

## Coordination points

Most work is autonomous. Defer to a human or another module only here:

1. **Base API freeze.** No module finalizes against a base contract not yet
   agreed. A module that cannot express itself in the contract stops and files
   the gap against the base plan.
2. **Zero-copy is never regressed.** A cut that would drop a zero-copy path waits
   for the renderer and the frame vocabulary; the decode deletions land only
   after `modules/render-video-render.md` and `modules/codec-decode-surface.md`.
3. **Shared candidate tables** in `encode/backend/mod.rs` and
   `decode/backend/mod.rs`: each module adds only its own additive row; serialize
   and rebase.
4. **The moq-vaapi crate** (external `moq-dev/vaapi`, crates.io 0.0.2) ships an
   encoder, surface export, and a VPP wrapper but no decode stack. Open question:
   the dependency spine for the decode contribution; current proposal: re-vendor
   the decode half into moq-vaapi's diverged bindgen style. See
   `modules/codec-vaapi-decode.md`.
5. **Pre-encoded capture** (libcamera) is required. Open question: the
   `publish_preencoded` API shape; current proposal: mirror `publish_capture`
   minus `encode::Options`. See `modules/capture-libcamera.md`.
6. **The B4 breaking change** for Android. Open question: in-tree versus
   external; current proposal: external (Path B), which is what B4 exists for.
   See `modules/codec-android-mediacodec.md`.
7. **Per-segment transcoding and FETCH.** moq's codec direction is per-group
   transcoding with custom per-GOP rate control, owned by moq-transcode, which
   drives encoders only through the public `encode::{Kind, Config, Encoder}`
   front end. Every encoder module exposes rate-control primitives (honest
   `set_bitrate` with no forced-IDR side effect, a per-encode target or QP knob,
   forced IDR per GOP, and cheap session reuse between groups) and defers the
   policy to moq-transcode. See `notes-unstructured/transcode-and-fetch.md`.
8. **Licensing, CI hardware gating, and semver** for the ported FFI. See
   `notes-unstructured/coordination.md`.

## Directory map

- `prompt.md` - how to work the campaign (the fleet coordinator prompt).
- `zerocopy.md` - the decisive comparison; read it early and often.
- `comparison.md` - the master matrix: every module, iroh-live versus moq, the
  verdict, and links to the module doc and the evidence.
- `base/` - the enabling moq API changes (B1 through B5).
- `modules/` - one doc per capability or backend, each leading with a VERDICT.
- `comparison/` - the evidence: the codec, capture, audio, traits-API, and
  moq-change analyses, the iroh-live code map, the moq inventory, and the `maps/`
  of both codebases.
- `notes-unstructured/` - material that does not belong to one module: the
  per-segment transcode and FETCH rule, the cross-cutting coordination concerns
  (licensing, CI gating, semver, the moq-vaapi repo), the parity-port register,
  and the preserved refactor analysis.

## Module index

Grouped by category; each links to its doc. Verdicts: M = move, A = apply
improvements, U = use moq and remove ours, R = remove for now.

Codec:
[vaapi-encode](modules/codec-vaapi-encode.md) M,
[vaapi-decode](modules/codec-vaapi-decode.md) M,
[v4l2-encode](modules/codec-v4l2-encode.md) M,
[v4l2-decode](modules/codec-v4l2-decode.md) M,
[android-mediacodec](modules/codec-android-mediacodec.md) M,
[decode-surface](modules/codec-decode-surface.md) A,
[opus](modules/codec-opus.md) A,
[pcm](modules/codec-pcm.md) M,
[bitstream-sps-vui](modules/codec-bitstream-sps-vui.md) M,
[openh264](modules/codec-openh264.md) U,
[videotoolbox-encode](modules/codec-videotoolbox-encode.md) U,
[dispatch](modules/codec-dispatch.md) U,
[annexb](modules/codec-annexb.md) U,
[config-mirror](modules/codec-config-mirror.md) U,
[av1](modules/codec-av1.md) R.

Audio:
[audio-backend](modules/audio-backend.md) M,
[resample](modules/audio-resample.md) U.

Capture:
[pipewire](modules/capture-pipewire.md) M,
[v4l2](modules/capture-v4l2.md) M,
[libcamera](modules/capture-libcamera.md) M,
[x11](modules/capture-x11.md) M,
[android](modules/capture-android.md) M,
[macos-screen](modules/capture-macos-screen.md) U,
[macos-camera](modules/capture-macos-camera.md) U,
[windows](modules/capture-windows.md) U,
[nokhwa](modules/capture-nokhwa.md) U,
[xcap](modules/capture-xcap.md) U.

Render:
[video-render](modules/render-video-render.md) M,
[egui](modules/render-egui.md) M.
