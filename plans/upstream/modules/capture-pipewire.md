# PipeWire screen capture (DMA-BUF zero-copy delivery)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: central. Ours delivers `SPA_DATA_DmaBuf` buffers as a DMA-BUF frame
> into VAAPI encode with no download; moq's PipeWire backend is CPU-I420 only.
> See ../zerocopy.md, section 2a (capture to encode) and section 5 requirement
> U3.

## What it is

Linux screen (and portal camera) capture over PipeWire and the XDG desktop
portal. moq already has a PipeWire screen backend, but it is deliberately CPU
only: its ScreenCast format offer carries no dmabuf modifiers, so the compositor
hands back shared memory and a dedicated loop thread converts every frame to CPU
`I420` before it reaches the encoder. Ours negotiates `SPA_DATA_DmaBuf`, wraps
the delivered fd as a GPU frame, and feeds it into the VAAPI encoder with no CPU
copy. The verdict is move-ours because the zero-copy DMA-BUF delivery has no
counterpart upstream and is the single largest capture contribution we carry;
we add it to moq's existing backend rather than replacing that backend.

## iroh-live vs moq

Ours (`rusty-capture/src/platform/linux/pipewire.rs`, 1,655 lines) is the
deepest capture backend on either side. `PipeWireScreenCapturer`
(`pipewire.rs:1347`) negotiates the source through the XDG ScreenCast portal via
`ashpd`, accepts a `pipewire_restore_token` in `ScreenConfig` to skip the picker
on reconnect, and exposes the fresh token back to the caller
(`pipewire.rs:1378-1381, 1434-1436`). When the compositor delivers
`SPA_DATA_DmaBuf`, the fd is duplicated and wrapped as a `PipeWireDmaBufFrame`
implementing `GpuFrameInner`, exposing `NativeFrameHandle::DmaBuf` with DRM
fourcc, modifier, and per-plane layout (`pipewire.rs:145-247, 743-766`). The DRM
mapping covers NV12, BGRA, BGRx, RGBA, RGBx, and YUY2/YUYV
(`pipewire.rs:114-133`); anything unmapped falls back to mmap plus copy. The
same file also implements `PipeWireCameraCapturer` (`pipewire.rs:1513`) for
portal cameras, which moq does not have at all.

Theirs (`rs/moq-video/src/capture/pipewire.rs`, 581 lines, #2238) is CPU only by
design: the format offer carries no dmabuf modifiers, so buffers stay in shared
memory and a dedicated PipeWire loop thread converts BGRx/BGRA to CPU I420 per
frame (`pipewire.rs:383, 424, 474-482`). It is feature-gated off by default
because libpipewire is a build-time link dependency. Where theirs is ahead: the
restore token is a process-wide static replayed automatically across
demand-driven reopens and forgotten when the compositor ends the stream so a
revoked grant re-prompts (`pipewire.rs:51-55`, module doc `:9-18`); a
damage-driven compositor is re-paced by re-emitting the last frame each frame
interval so a static screen does not starve the encoder (`:18-20`); and `open`
fails fast on a 10 s format timeout or 5 s first-frame timeout rather than
handing the encoder a dead stream (`:41-49`). Ours threads the token through the
caller instead, and our long-lived capturer object cannot survive `stop()`,
which is exactly the operation demand-gating wants: a comment at
`moq-media/src/publish.rs:1109-1113` records that PipeWire capturers cannot
round-trip a stop because it permanently kills the capture thread, forcing the
gating to track `ever_started` and avoid stopping before the first start.

The right outcome keeps ours for the zero-copy path and the camera support, and
ports three of moq's behaviors into the kept backend: automatic token replay
across reopens, static-screen re-pacing, and open-per-demand-cycle teardown in
place of `stop()`, which retires the cannot-stop-before-start wart.

## Zero-copy

This is a zero-copy-critical module (see the mandatory list in ../zerocopy.md).
The question is not whether a GPU handle exists but whether the frame reaches the
encoder without a download.

Ours delivers `NativeFrameHandle::DmaBuf`, and the VAAPI encoder imports it
directly as a VA surface through a hand-built `VADRMPRIMESurfaceDescriptor` with
no CPU mapping permitted on that variant
(`rusty-codecs/src/codec/vaapi/encoder.rs:87-119, 1268-1289`). Capture to encode
is zero-copy today when the compositor hands out DMA-BUFs and the format maps;
SHM streams fall back to CPU. Mismatches stay on the GPU too: wrong dimensions
route through the cached-context VPP scaler (`encoder.rs:1069`,
`vaapi/vpp_scaler.rs`) and non-NV12 formats route through VPP color conversion
(`encoder.rs:1009`), so capture at 1080p BGRx and encode at 720p NV12 remains a
pure GPU pipeline. moq's Linux path is entirely CPU: pipewire converts BGRx
shared-memory buffers to CPU I420 and the VAAPI encoder accepts only tightly
packed CPU NV12, interleaving I420 to NV12 on the CPU per frame with no
GPU-surface input path (`encode/backend/vaapi.rs:60, 95-111`). See ../zerocopy.md
section 2a: on Linux ours is strictly ahead; DMA-BUF capture into VAAPI encode
with GPU-side scale and color conversion has no counterpart.

The B1 frame vocabulary is why PipeWire matters to the base contract. B1 gates
the private `Frame::DmaBuf` variant on a shared `dmabuf` feature rather than on
`vaapi`, precisely because PipeWire capture produces DMA-BUF without VAAPI: this
leaf enables `dmabuf` (pulled in by the `pipewire` feature) and does not depend
on `vaapi`. See ../base/B1-frame-vocabulary.md.

The VAAPI import side that consumes this frame is the sibling
[codec-vaapi-encode.md](codec-vaapi-encode.md). This leaf delivers the producer
half only and does not depend on the encoder work landing first, because the CPU
fallback keeps the backend whole on its own.

## What to do

MOVE the DMA-BUF delivery into moq's existing backend, in place. Target
`rs/moq-video/src/capture/pipewire.rs`, all behind the existing `pub(crate)`
seam.

Source to port from `rusty-capture/src/platform/linux/pipewire.rs`:

- The DMA-BUF negotiation and delivery. `PipeWireDmaBufFrame`
  (`pipewire.rs:145-159`) is the exporter shape: dup'd `OwnedFd`, `drm_format`,
  `modifier` parsed from `SPA_FORMAT_VIDEO_modifier`, width, height, stride,
  offset. `dmabuf_to_frame` (`pipewire.rs:731-780`) is the construction path: on
  a known DRM fourcc it dups the fd and wraps it as a GPU frame, otherwise it
  falls back to mmap plus copy (`dmabuf_to_frame_cpu`, `pipewire.rs:782`).
- The DRM fourcc mapping. `spa_format_to_drm_fourcc` (`pipewire.rs:114-133`) maps
  SPA video formats to DRM fourcc: BGRA to `AR24`, BGRx to `XR24`, RGBA to
  `AB24`, RGBx to `XB24`, NV12 to `NV12`, YUY2 to `YUYV`. Port the table; drop
  our NV12-only zero-copy gate (`pipewire.rs:731`), which existed only because
  our `GpuPixelFormat` modeled NV12 alone. moq's `dmabuf::Frame` carries the
  fourcc verbatim, so all mapped formats can take the DMA-BUF path and the
  encoder side decides what it can import.
- The modifier offer. Extend moq's `format_offer` `EnumFormat` pod to advertise
  `SPA_FORMAT_VIDEO_modifier` alongside the RGB layouts, so the compositor may
  pick a DMA-BUF path, and connect without forcing `MAP_BUFFERS` when a modifier
  is negotiated.

What is dropped in the port: our `GpuFrame`/`GpuFrameInner`/`NativeFrameHandle`
trait glue (moq's model is the private `Frame::DmaBuf` plus the B1 public
`Native`, so we do not port our parallel vocabulary); our own portal negotiation,
restore-token threading, and static-screen re-pacing, because moq already owns
those and they are better than ours; and our `Duration` timestamps, because
capture frames are unstamped in moq and the encode loop stamps from
`moq_mux::Clock`.

Concrete changes to moq's backend:

1. `format_offer` (around `pipewire.rs:437`): add a DMA-BUF branch to the
   `EnumFormat` pod advertising `SPA_FORMAT_VIDEO_modifier` for the RGB and NV12
   layouts moq can hand downstream, keeping the existing shm layouts as the
   fallback the compositor may still pick. This is the reversible half: if the
   DMA-BUF branch is not selected, behavior is byte-identical to today.
2. The stream connect (`pipewire.rs:424-435`): stop unconditionally requesting
   `MAP_BUFFERS`; request it only for the shm path, and record the negotiated
   data type in the loop state.
3. The buffer callback (`pipewire.rs:378-417`): branch on the SPA data type. For
   `SPA_DATA_DmaBuf`, dup the fd, read the negotiated modifier and per-plane
   layout, construct a `dmabuf::Frame` (B1), and `chan.push(Frame::DmaBuf(..))`.
   For `SPA_DATA_MemPtr` (the current path), keep `convert()` and push
   `Frame::I420` exactly as today.
4. Port `spa_format_to_drm_fourcc` (`pipewire.rs:114-133`) as a private helper.
5. Extend the re-pace timer and `state.last` to carry the last frame regardless
   of variant, so static-screen pacing works on both paths. The static-screen
   re-pacing timer (`pipewire.rs:440-457`, the `add_timer` that pushes
   `Frame::I420(last.clone())` at `pipewire.rs:449`) keeps working because it
   re-emits `state.last`; extend `state.last` to hold whichever variant was last
   delivered.
6. Gate the whole DMA-BUF addition on the same feature the `Frame::DmaBuf`
   variant is gated on in B1 (the `dmabuf` feature). With the feature off, the
   backend compiles and runs as the current CPU-only path.

No change to `capture/mod.rs`, `capture/channel.rs`, or `capture/pump.rs`. The
`FrameStream` and the drop-oldest `FrameChannel` (`DEPTH = 4`) already accept any
`Frame` variant.

The portal camera source is an in-scope sibling of this leaf. Our
`rusty-capture` also has a PipeWire portal camera capturer
(`PipeWireCameraCapturer`), and moq has no portal camera source at all, so the
comparison's verdict to keep our camera support has no other home. Carry it here:
an `org.freedesktop.portal.Camera` counterpart to the screen `ScreenCast` path.
It reuses the same SPA format negotiation, DRM-fourcc mapping, and
`Frame::DmaBuf` delivery once a stream is running, but the connection setup
differs and does not transfer unchanged. `ScreenCast` returns a picker-selected
node id the client connects to directly; the Camera portal has no
picker-provided node id, so after `AccessCamera`, `OpenPipeWireRemote` hands back
a single PipeWire remote fd, and the client connects to that fd, enumerates the
camera nodes on the remote registry, and binds the chosen node. The camera
sibling therefore adds an fd-open plus node-enumeration step ahead of the shared
negotiation. It is a follow-up commit within this leaf (or a near sibling branch
off the same base), sequenced after the screen path proves DMA-BUF delivery. If
upstream review prefers it as a separate leaf, that is the alternative
disposition; either way it is tracked, not dropped.

The iroh-live removal side: `rusty-capture/src/platform/linux/pipewire.rs`
(1,655 LOC, disposition upstream-ours) is deleted only after the upstream
contribution merges and releases, on the paired counterpart branch
(`the capture removal sequencing`). The three moq behaviors (token replay,
static-screen re-pacing, open-per-demand lifecycle) are ported into our kept
backend meanwhile, so the local backend improves before it is cut. Note on CPU
scaling: our `processing/scale` (pic-scale) CPU scaler is dropped by decision,
not lost, because moq ships `fast_image_resize` for the same job; GPU scaling for
the zero-copy path is VPP, carried by the VAAPI leaves.

## Tests

moq-style, hardware-gated. The zero-copy path needs a compositor delivering
DMA-BUF buffers (a real Wayland session), which CI does not have.

- A DMA-BUF round-trip test marked `#[ignore]` with the reason stated: open the
  PipeWire backend, receive a frame, assert on a DMA-BUF compositor that it is
  `Frame::DmaBuf` with a plausible fourcc and modifier and a non-empty plane
  layout, and assert `to_i420()` (the B1 CPU fallback) produces a frame of the
  expected geometry. Model it on moq's existing hardware round-trip tests (the
  VideoToolbox and NVENC encoder tests are the template). Note in the PR that the
  ignored test was validated on the author's Wayland hardware and name the
  compositor, because moq's runners cannot exercise it.
- A unit test for `spa_format_to_drm_fourcc` covering every mapped SPA format,
  which needs no hardware and runs in CI.
- A compile-time check that with the `dmabuf` feature off the backend still
  builds and the CPU path is unchanged.

## Evidence

- Per-backend table and verdict: ../comparison/capture.md (Linux screen PipeWire
  row, "1,655 lines, portal + DMA-BUF GPU frames, camera too" against "581 lines,
  portal, CPU I420 only"; the deliberate CPU offer at `capture.md:216-228`; the
  section 5 verdict "keep ours" at `capture.md:483-486`).
- Zero-copy diagnosis: ../zerocopy.md, section 2a (capture to encode, verdict
  complementary; ours strictly ahead on Linux) and section 5 requirement U3
  (capture DMA-BUF delivery with the matching encoder input).
- Code map: ../comparison/maps/rusty-capture.md (PipeWire backend detail, the
  DMA-BUF path at `pipewire.rs:14-16, 145-247, 721-766`, the NV12-only gate at
  `:721`).
- Disposition: ../comparison covers the PipeWire row (upstream-ours,
  `capture/pipewire-dmabuf.md`, the DMA-BUF delivery upstreams and the three moq
  behaviors port into ours meanwhile).

## Coordination

- Base plan needed: ../base/B1-frame-vocabulary.md. This leaf produces the
  private `Frame::DmaBuf` variant and consumes the `dmabuf::Frame` exporter type;
  it does not touch the public `Native` enum directly, and it adds no
  candidate-table entry (that is a codec concern), so it is free of the
  shared-table conflict. If the `dmabuf::Frame` exporter cannot express a
  PipeWire buffer (for example a multi-plane modifier layout B1 did not
  anticipate), stop and file the gap against B1 rather than inventing a local
  frame type.
- The `dmabuf` feature: this leaf is the reason B1 gates the variant on a shared
  `dmabuf` feature and not on `vaapi`. It enables `dmabuf` (via `pipewire`) and
  does not depend on `vaapi`.
- Sibling: the VAAPI import side that consumes this frame lands in
  [codec-vaapi-encode.md](codec-vaapi-encode.md). This leaf is the producer half
  only; the CPU fallback keeps the backend whole without the encoder work landing
  first.
- Adaptation constraints: no ffmpeg (moq already uses `yuv` for the CPU
  conversion and it stays); no new link dependency (libpipewire is already behind
  moq's `pipewire` feature, libdrm is not needed because the fourcc constants are
  inlined); no `Duration` at a boundary (the encode loop stamps from
  `moq_mux::Clock`); errors degrade a DMA-BUF construction failure to the CPU
  branch or warn and stop the loop as the current shm `None` branch does
  (`pipewire.rs:386-392`), needing no new public error variant beyond B5's
  additive set.
- Release gate: the local module is cut only when the release carrying the leaf
  is pinned, on the paired `up/pipewire-dmabuf` branch.
