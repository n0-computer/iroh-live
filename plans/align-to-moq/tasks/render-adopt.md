# render-adopt

Branch: align/render-adopt          Wave: 3
Depends on: upstream base plan B1 (public `Native` frame vocabulary), base plan B3
(`decode::Frame::native()` accessor), the `vtb-mf-decode-surface` leaf (so the
macOS and Windows decoders retain their GPU surface), and moq's in-tree
`moq-video-render` crate (an off-default moq workspace member) being released from
`../../upstream/render/moq-video-render.md`.
On Linux the DMA-BUF import additionally needs `vaapi-decode` to have something to
export, though the crate and its CPU fallback compile and run without it.
Kind: upstream-gated

## Goal

Replace iroh-live's renderer with moq's in-tree `moq-video-render` crate (an
off-default moq workspace member iroh-live consumes as a normal dependency once
moq releases it), deleting `render.rs` and the whole `render/` tree
(3,463 LOC) and collapsing iroh-live's parallel `FrameData`/`NativeFrameHandle`
frame model onto moq's public `Native` vocabulary, so there is one frame model in
the pipeline rather than the two-model translation carried today. The renderer,
its Vulkan and EGL and Metal importers, its `VppRetiler` for the Intel Y_TILED to
CCS re-tile, and `create_device_with_dmabuf_extensions` all move into
`moq-video-render`, which iroh-live then consumes as a dependency reading
`moq_video::decode::Frame::native()`. This task is coordination-point-2 critical:
it is the single thing that lets the decode-to-render zero-copy path survive the
decoder deletions in `codec-remove`, because after adoption the renderer imports
moq's decode surfaces rather than iroh-live's `GpuFrame`, so the local VAAPI, VTB,
and Android decoders can be removed without dropping a pixel path to the screen.
It must therefore land before or together with `codec-remove`'s decode deletions.

## Evidence

- `../../upstream/comparisons/zerocopy.md` section 4 (the render upstreaming
  decision, originally Option B but REVISED 2026-07-22 to in-tree, an off-default moq workspace crate; see ../../upstream/0-overview.md Review revisions revision 1) and section 5 (the U1
  through U4 requirements), plus section 2b, which shows the decode-to-render path
  is iroh-live's alone and has no upstream code to merge into, so it can only be
  preserved by carrying it forward.
- `../cut-plan.md` section 2, the `render.rs + render/` row (3,463 LOC, "keep, then
  upstream as an in-tree off-default crate", prerequisite U1), and section 4, which fixes the
  end-state: the importers and renderer move to a published crate over U1 public
  handles.
- `../../upstream/render/moq-video-render.md` is the upstream plan that authors the
  crate; `../../upstream/base/B1-frame-vocabulary.md` and
  `../../upstream/base/B3-decode-native-accessor.md` define the handle vocabulary
  and accessor this task consumes.

## moq primitive adopted

- The public `Native` enum from B1 (`#[non_exhaustive]`, naming OS handles:
  `DmaBuf`, `CvPixelBuffer`, `D3d11`, `Cuda`, `HardwareBuffer`), with the `DmaBuf`
  accessor exposing fourcc, modifier, coded and display size, planes, and an
  on-demand `export()` yielding a fresh dup'd fd.
- The B3 accessor `decode::Frame::native() -> Option<Native>`
  (`moq:decode/mod.rs`, beside `into_i420()`), which returns the platform handle
  for GPU-resident frames and `None` for CPU frames.
- The surface-retaining decoders from `vtb-mf-decode-surface`, so on macOS the
  decoded NV12 `CVPixelBuffer` and on Windows the DXVA NV12 texture reach the
  renderer without a CPU round trip.
- The published `moq-video-render` crate itself, which iroh-live adds as a
  dependency in place of the local `render` module.

## iroh-live code changed

Deleted, once the new path is proven (LOC verified against the working tree):

| Module | file:line | LOC | Note |
|---|---|---:|---|
| renderer core | `rusty-codecs/src/render.rs` | 799 | path selection, shaders, per-frame failure fallback, and `render_cached`; ports into `moq-video-render` |
| DMA-BUF import | `rusty-codecs/src/render/dmabuf_import.rs` | 1,452 | raw Vulkan via ash, `VppRetiler` (Intel Y_TILED to CCS), and `create_device_with_dmabuf_extensions` |
| GLES DMA-BUF | `rusty-codecs/src/render/gles_dmabuf.rs` | 402 | EGLImage import via `EGL_EXT_image_dma_buf_import` |
| GLES renderer | `rusty-codecs/src/render/gles.rs` | 536 | GL-only-context renderer |
| Metal import | `rusty-codecs/src/render/metal_import.rs` | 274 | CVPixelBuffer aliased as Metal texture via `CVMetalTextureCache` |

Total 3,463 LOC, matching the cut-plan `render.rs + render/` row.

Frame-model collapse, coordinated with `codec-remove`'s `format.rs` merge: the
parallel model that the renderer consumes is deleted in the same window. This is
the U1-donor half of `rusty-codecs/src/format.rs` (the public `FrameData::Gpu`,
`GpuFrame`, `GpuFrameInner`, `NativeFrameHandle` at `format.rs:68-87`, and
`DmaBufInfo` at `format.rs:508-528`), which collapses into moq's public `Native`
vocabulary so the two-model translation disappears (zerocopy.md section 6). The
consumers that implement `GpuFrameInner` today, the VAAPI, VTB, and Android
decoders and the PipeWire capture frames, stop needing it as they migrate.

## Steps

1. Confirm the upstream prerequisites are in a pinned moq release: B1 `Native`,
   B3 `native()`, and `vtb-mf-decode-surface`, and that `moq-video-render` is
   published and consumes those handles.
2. Add `moq-video-render` as an iroh-live dependency and route the client playout
   path through it, reading `moq_video::decode::Frame::native()` for the GPU
   handle and falling back to `into_i420()` for CPU frames, behind a
   `moq-render` feature flag so the old and new renderers coexist.
3. Prove the new path end to end (see Proof before deletion) on each platform,
   including the Intel Y_TILED re-tile path on Meteor Lake hardware, then flip the
   default to the new renderer.
4. Delete `render.rs` and the four files under `render/` in a deletion-only
   commit, once the flip is proven, so a revert restores the old renderer cleanly.
5. Coordinate the `format.rs` frame-model collapse with `codec-remove`: as each
   platform's decoders migrate to producing moq `decode::Frame`s, remove their
   `GpuFrameInner` implementations, and delete the `NativeFrameHandle`,
   `GpuFrame`, and `FrameData::Gpu` surface once no consumer remains. Sequence
   this before or together with the corresponding decoder deletion on that
   platform, never after.

## Proof before deletion

Mandatory, per coordination point 1, and doubly load-bearing here because this
task is the guarantee for coordination point 2.

- The hardware-gated `moq-media/tests/zero_copy_pipeline.rs` passes with the
  playout path rendering through `moq-video-render` from moq's
  `decode::Frame::native()`, on Intel Linux (DMA-BUF import, including the
  `VppRetiler` re-tile), on macOS (CVPixelBuffer into Metal), and, where hardware
  is available, on Windows (D3D11 texture) and Android (HardwareBuffer), before
  the local `render/` tree is deleted.
- A client playback example renders a decoded stream to screen through the new
  crate on at least the Intel Linux and macOS paths, with recorded on-hardware
  results, satisfying the R-g gate for the platforms lacking CI.
- The CPU fallback (`into_i420()` into the software render path) is verified on a
  GPU-less host so the crate degrades cleanly.

## Coordination

- Coordination point 2 (no zero-copy regression) is this task's reason to exist:
  `render-adopt` must land before or with `codec-remove`'s decode deletions (VTB
  decoder, VAAPI decode, Android HardwareBuffer decode), because those decoders
  are the current producers of the GPU frames the renderer imports. Deleting them
  before the renderer consumes moq's `decode::Frame::native()` would drop the
  decode-to-render path (zerocopy.md sections 2b and 5).
- Coordination point 3 (upstream gating): the crate cannot be adopted until B1,
  B3, `vtb-mf-decode-surface`, and the published `moq-video-render` are in a moq
  release iroh-live can pin, and on Linux until `vaapi-decode` gives the DMA-BUF
  path something to import.
- Coordination point 4 (atomic per platform): the frame-model collapse in
  `format.rs` is done per platform in lockstep with `codec-remove`, so a platform
  never holds both the local `NativeFrameHandle` and moq's `Native` at once.
- The UI-framework integrations stay in iroh-live for now (overview Review
  revisions revision 1): this task deletes `rusty-codecs/src/render/`, the
  low-level importers and renderer, but the dioxus and egui shells
  (`moq-media-dioxus`, `moq-media-egui`) and the demo apps are not deleted. They
  are rewired to consume `moq-video-render`'s output instead of iroh-live's
  renderer, and moving them elsewhere is a later, separate effort.

## Acceptance checklist

- iroh-live's client playout renders through the published `moq-video-render`
  crate over `moq_video::decode::Frame::native()`, with the CPU fallback intact.
- `render.rs` and all of `render/` (3,463 LOC) are deleted in a deletion-only,
  revertible commit, after the new path passes `zero_copy_pipeline.rs` on each
  supported platform.
- The parallel `FrameData::Gpu`, `GpuFrame`, `GpuFrameInner`, `NativeFrameHandle`,
  and `DmaBufInfo` model is collapsed onto moq's public `Native` vocabulary,
  coordinated with `codec-remove`, with no remaining `GpuFrameInner` implementors.
- The decode-to-render zero-copy path is preserved on every platform it exists on
  today (Intel and AMD Linux via DMA-BUF, macOS via CVPixelBuffer, Android via
  HardwareBuffer), proven before any decoder deletion in `codec-remove`.
- `cargo make check-all` is green at every commit.
</content>
