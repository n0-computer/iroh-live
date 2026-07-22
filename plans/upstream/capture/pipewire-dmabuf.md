# pipewire-dmabuf. PipeWire DMA-BUF zero-copy capture delivery

> Campaign: upstream | Kind: leaf plan | Branch: up/pipewire-dmabuf |
> PR target: base branch, then moq main | Read ../0-overview.md first.

Depends on: B1 (frame vocabulary: `Native`, `Frame::DmaBuf(dmabuf::Frame)`)
Path: A (in-tree)
Size: M

## Goal

Contribute zero-copy DMA-BUF delivery to moq's existing PipeWire screen-capture
backend. moq already has a PipeWire backend, but it is CPU only: its ScreenCast
format offer carries no dmabuf modifiers, so the compositor hands back shared
memory and a dedicated loop thread converts every frame to CPU `I420` before it
reaches the encoder (`rs/moq-video/src/capture/pipewire.rs:383, 424, 474-482`).
This plan makes the backend negotiate `SPA_DATA_DmaBuf`, wrap the delivered fd as
a `Frame::DmaBuf`, and push it into the same `FrameStream` the backend already
feeds, so that on a compositor delivering GPU buffers the VAAPI encoder imports
the surface with no download. The CPU path stays intact as the fallback whenever
the compositor forces shared memory or the format has no DRM mapping, matching
the "software paths keep working" rule of the base contract.

## Evidence

- moq is CPU only today. Verdict and diagnosis:
  `comparisons/capture.md:187` (per-backend table row, "581 lines, portal, CPU
  I420 only") and `comparisons/capture.md:216-228` (the deliberate CPU offer, the
  three behaviors worth keeping). Change design:
  `comparisons/moq-changes.md:52-56` (change 1a, capture-side zero-copy) and
  `comparisons/moq-changes.md:148-178` (point 4: produce `Frame::DmaBuf` into the
  existing `FrameStream`).
- moq's source, verified against HEAD 3a3e0ea8:
  - `rs/moq-video/src/capture/pipewire.rs:1-19` module doc, states each RGB frame
    is converted to CPU `I420`.
  - `rs/moq-video/src/capture/pipewire.rs:383-393` the shm-only comment ("Without
    dmabuf modifiers in our format offer the compositor uses shared memory").
  - `rs/moq-video/src/capture/pipewire.rs:422-435` the `format_offer` connect with
    `MAP_BUFFERS` and the "No dmabuf modifiers" comment.
  - `rs/moq-video/src/capture/pipewire.rs:474-482` `convert()`, BGRx/BGRA to
    `I420::from_bgra`, RGBx/RGBA to `I420::from_rgba`.
  - `rs/moq-video/src/capture/mod.rs:230-242` the `pub(crate) FrameStream` every
    backend feeds, and `mod.rs:225-229` the drop-releases-device contract.
  - `rs/moq-video/src/capture/channel.rs` the `FrameChannel` (`DEPTH = 4`,
    drop-oldest) the backend pushes into.
- Our reference: `comparisons/maps/rusty-capture.md:200-211` (PipeWire is the
  richest backend, DMA-BUF path).

## moq API consumed

From the frozen base contract (B1):
- `crate::frame::Frame::DmaBuf(dmabuf::Frame)`, the new cfg-gated private variant
  under `cfg(all(target_os = "linux", feature = "dmabuf"))`, with its
  `width`/`height`/`to_i420` arms. This backend is a producer of that variant, and
  PipeWire is exactly why B1 gates it on the shared `dmabuf` feature rather than on
  `vaapi`: PipeWire capture produces DMA-BUF without VAAPI, so this leaf enables the
  `dmabuf` feature (it does not depend on `vaapi`).
- The backing `dmabuf::Frame` exporter type from B1 (holds the dup source, mints
  an `OwnedFd` on demand, carries DRM fourcc, modifier, coded and display size,
  and per-plane offset and pitch). This backend constructs one per delivered
  buffer.

This plan consumes B1 only. It does not touch the public `Native` enum directly:
it produces the private `Frame::DmaBuf`, and B1 owns the public projection. It
adds no candidate table entry (that is a codec concern, coordination point 2),
so it is free of the shared-table conflict.

## Source to port

`rusty-capture/src/platform/linux/pipewire.rs` (1,655 LOC). What carries over:

- The DMA-BUF negotiation and delivery. `PipeWireDmaBufFrame`
  (`pipewire.rs:145-159`) is the shape of the exporter: dup'd `OwnedFd`,
  `drm_format`, `modifier` parsed from `SPA_FORMAT_VIDEO_modifier`, width, height,
  stride, offset. `dmabuf_to_frame` (`pipewire.rs:731-780`) is the construction
  path: on a known DRM fourcc it dups the fd and wraps it as a GPU frame,
  otherwise it falls back to mmap plus copy (`dmabuf_to_frame_cpu`,
  `pipewire.rs:782`).
- The DRM fourcc mapping. `spa_format_to_drm_fourcc` (`pipewire.rs:114-133`) maps
  SPA video formats to DRM fourcc: BGRA to `AR24`, BGRx to `XR24`, RGBA to `AB24`,
  RGBx to `XB24`, NV12 to `NV12`, YUY2 to `YUYV`. Port the table; drop our
  NV12-only zero-copy gate (`pipewire.rs:731`), which existed because our
  `GpuPixelFormat` only modeled NV12. moq's `dmabuf::Frame` carries the fourcc
  verbatim, so all mapped formats can take the DMA-BUF path and the encoder side
  decides what it can import.
- The modifier offer. Extending the SPA `EnumFormat` pod (moq's `format_offer`,
  `pipewire.rs:437+`) to advertise `SPA_FORMAT_VIDEO_modifier` alongside the RGB
  layouts, so the compositor may pick a DMA-BUF path, and connecting without
  forcing `MAP_BUFFERS` when a modifier is negotiated.

What is dropped:
- Our `GpuFrame`/`GpuFrameInner`/`NativeFrameHandle` trait glue
  (`rusty-codecs::format`). moq's model is the private `Frame::DmaBuf` plus the
  B1 public `Native`; we do not port our parallel vocabulary.
- Our portal and camera code (`PipeWireScreenCapturer`,
  `PipeWireCameraCapturer`, restore-token threading). moq already owns portal
  negotiation, restore-token replay, and static-screen re-pacing, and they are
  better than ours (`comparisons/capture.md:224-228`); this plan changes only the
  buffer negotiation and the delivered frame type, not the portal or lifecycle.
- Our `Duration` timestamps; capture frames are unstamped in moq (the encode loop
  stamps from `moq_mux::Clock`), so no timestamp travels with the pushed frame.

## Target in moq

`rs/moq-video/src/capture/pipewire.rs`, in place. Changes, all behind the
existing `pub(crate)` seam:

1. `format_offer` (around `pipewire.rs:437`): add a DMA-BUF branch to the
   `EnumFormat` pod that advertises `SPA_FORMAT_VIDEO_modifier` for the RGB and
   NV12 layouts moq can hand downstream, keeping the existing shm layouts as the
   fallback the compositor may still pick.
2. The stream connect (`pipewire.rs:424-435`): stop unconditionally requesting
   `MAP_BUFFERS`; request it only for the shm path. When a modifier is
   negotiated, take the DMA-BUF branch in the process callback.
3. The buffer callback (`pipewire.rs:378-417`): branch on the SPA data type. For
   `SPA_DATA_DmaBuf`, dup the fd, read the negotiated modifier and per-plane
   layout, construct a `dmabuf::Frame` (B1), and `chan.push(Frame::DmaBuf(..))`.
   For `SPA_DATA_MemPtr` (the current path), keep `convert()` and push
   `Frame::I420` exactly as today.
4. A small `spa_format_to_drm_fourcc` helper ported from `pipewire.rs:114-133`.

No change to `capture/mod.rs`, `capture/channel.rs`, or `capture/pump.rs`. The
`FrameStream` and `FrameChannel` already accept any `Frame` variant. The
static-screen re-pacing timer (`pipewire.rs:440-457`, the `add_timer` that pushes
`Frame::I420(last.clone())` at `pipewire.rs:449`) keeps working because it
re-emits `state.last`; extend `state.last` to hold whichever variant was last
delivered so a static screen on the DMA-BUF path re-emits the last DMA-BUF frame.

## Implementation steps

1. Port `spa_format_to_drm_fourcc` (`pipewire.rs:114-133`) into moq's backend as
   a private helper. Small, no dependencies beyond the DRM fourcc constants.
2. Extend `format_offer` to advertise dmabuf modifiers for the supported layouts.
   Keep the shm layouts in the same offer so a compositor without dmabuf support
   still negotiates the CPU path. This is the reversible half: if the DMA-BUF
   branch is not selected, behavior is byte-identical to today.
3. Rework the stream connect so `MAP_BUFFERS` is conditional and the negotiated
   data type is recorded in the loop state.
4. In the buffer callback, add the `SPA_DATA_DmaBuf` branch: dup the fd, build a
   `dmabuf::Frame` (B1 exporter) from the DRM fourcc, modifier, and plane layout,
   push `Frame::DmaBuf`. Leave the `SPA_DATA_MemPtr` branch unchanged.
5. Extend the re-pace timer and `state.last` to carry the last frame regardless
   of variant, so static-screen pacing works on both paths.
6. Gate the whole DMA-BUF addition on the same feature the `Frame::DmaBuf`
   variant is gated on in B1 (`vaapi`, or the new `dmabuf` feature). With the
   feature off, the backend compiles and runs as the current CPU-only path.

## Tests

moq-style, hardware-gated. The zero-copy path needs a compositor delivering
DMA-BUF buffers (a real Wayland session), which CI does not have, so:

- A DMA-BUF round-trip test marked `#[ignore]` with the reason stated: open the
  PipeWire backend, receive a frame, assert on a DMA-BUF compositor it is
  `Frame::DmaBuf` with a plausible fourcc and modifier and a non-empty plane
  layout, and assert `to_i420()` (the B1 CPU fallback) produces a frame of the
  expected geometry. Model it on moq's existing hardware round-trip tests
  (`comparisons/moq-changes.md` acceptance style; the VideoToolbox and NVENC
  encoder tests are the template).
- A unit test for `spa_format_to_drm_fourcc` covering every mapped SPA format,
  which needs no hardware and runs in CI.
- A compile-time check that with the DMA-BUF feature off the backend still builds
  and the CPU path is unchanged.

Note in the PR that the ignored test was validated on the author's Wayland
hardware and name the compositor, because moq's runners cannot exercise it.

## Adaptation notes

- No ffmpeg: the port introduces none; moq already uses `yuv` for the CPU
  conversion and that stays.
- dlopen: libpipewire is a build-time link dependency behind moq's `pipewire`
  feature already; this plan adds no new link dependency. libdrm is not required
  because the fourcc constants are inlined (as in our reference,
  `pipewire.rs:116-121`).
- Timestamps: none travel with the frame; the encode loop stamps from
  `moq_mux::Clock`. No `Duration` crosses any boundary.
- Errors: a DMA-BUF construction failure degrades to the CPU branch or, if the
  compositor forced an unreadable buffer, warns and stops the loop exactly as the
  current shm `None` branch does (`pipewire.rs:386-392`); no new public error
  variant is needed beyond B5's additive set.
- Feature gate: the DMA-BUF branch lives under the same cfg as `Frame::DmaBuf`.

## Coordination

- Coordination point 1 (base API freeze). This plan codes strictly against the
  frozen `Native`/`DmaBuf` contract and the private `Frame::DmaBuf` variant it
  produces. If the `dmabuf::Frame` exporter cannot express a PipeWire buffer (for
  example a multi-plane modifier layout B1 did not anticipate), stop and file the
  gap against B1 rather than inventing a local frame type.
- No candidate-table edit, so coordination point 2 does not apply.
- The VAAPI import side that consumes this frame lands in the `vaapi-encode`
  leaf; this plan delivers the producer half only and does not depend on the
  encoder work landing first (the CPU fallback keeps the backend whole on its
  own).

## Scope: the PipeWire portal camera source

This plan contributes the screen path (moq already owns portal screen capture;
we add DMA-BUF zero-copy delivery to it). Our `rusty-capture` also has a PipeWire
portal CAMERA capturer (`PipeWireCameraCapturer`), and moq has no portal camera
source at all, so `comparisons/capture.md`'s PipeWire verdict to keep our camera
support has no home elsewhere. Carry it here as an in-scope camera sibling: a
`org.freedesktop.portal.Camera` counterpart to the screen `ScreenCast` path. It
reuses the same SPA format negotiation, DRM-fourcc mapping, and `Frame::DmaBuf`
delivery once a stream is running, but the connection setup differs from
`ScreenCast` and does not transfer unchanged. `ScreenCast` returns a
picker-selected node id that the client connects to directly. The Camera portal
has no picker-provided node id: after `AccessCamera`, `OpenPipeWireRemote` hands
back a single PipeWire remote fd, and the client connects to that fd and
enumerates the camera nodes announced on the remote registry itself, then binds
the chosen node. So the camera sibling adds a fd-open plus node-enumeration step
ahead of the shared negotiation, rather than receiving a node id from a picker.
It is a follow-up commit within this leaf (or a near sibling branch off the same
base), sequenced after the screen path proves the DMA-BUF delivery, so the camera
source is not lost. If upstream review prefers it as a separate leaf,
that is the alternative disposition; either way it is tracked, not dropped.

Note on CPU scaling: our `processing/scale` (pic-scale) CPU scaler is dropped by
decision, not lost, because moq ships `fast_image_resize` for the same job
(`comparisons/codecs.md`); GPU scaling for the zero-copy path is VPP, carried by
the VAAPI leaves.

## Acceptance checklist

- The PipeWire backend offers dmabuf modifiers and, on a compositor that
  delivers `SPA_DATA_DmaBuf`, pushes `Frame::DmaBuf` into the `FrameStream`.
- On a shm-only compositor, behavior is byte-identical to the current CPU path.
- `spa_format_to_drm_fourcc` unit test passes in CI.
- The hardware DMA-BUF round-trip test exists, is `#[ignore]`d with a stated
  reason, and is confirmed passing on named hardware in the PR body.
- With the DMA-BUF feature disabled, the crate builds and the CPU path is
  unchanged.
- `cargo clippy` clean; no new public API beyond what B1 provides; no ffmpeg, no
  new link dependency, no `Duration` at a boundary.
