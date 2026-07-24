# Video render crate (moq-video-render)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: this is the decisive zero-copy asset of the whole campaign. It is
> the only decode-to-render GPU path in either codebase, spanning three platforms
> and two graphics APIs, and it is the module the rest of the zero-copy story
> exists to feed. See ../zerocopy.md sections "2b. Decode to render: verdict ours"
> and "2d. Render itself: verdict ours".

## What it is

iroh-live renders a decoded GPU frame to an RGBA texture with zero copies where
the platform allows and a CPU download everywhere else, through two render
backends (wgpu and GLES) and three importers (Vulkan DMA-BUF, EGL/GLES DMA-BUF,
and Metal). moq renders nothing in Rust: its decode backends download every
frame to CPU I420 except NVDEC, and NVDEC's GPU frame is `pub(crate)` and exists
only to feed NVENC (../zerocopy.md section 2b). The renderer and its importers
move into moq as a new in-tree crate `moq-video-render`, a normal member of the
moq workspace whose heavy dependencies sit behind non-default Cargo features so
moq's default and relay builds stay render-free. The verdict is "move from
iroh-live to moq" because moq has no counterpart to merge into: this capability
can only be preserved by carrying it forward, and it is also the existence proof
that the public `Native` vocabulary (B1) is a complete public contract rather
than a private convenience.

## iroh-live vs moq

iroh-live has the full decode-to-render chain. `WgpuVideoRenderer`
(`rusty-codecs/src/render.rs:45`) renders any `FrameData` variant to a wgpu RGBA
texture, selects a `RenderPath` per frame, and disables a zero-copy path after
three consecutive failures. Below it sit the Vulkan `ash` DMA-BUF import with the
Intel `VppRetiler` re-tile (`render/dmabuf_import.rs`), the EGL/GLES DMA-BUF
import (`render/gles_dmabuf.rs`), the GL-only GLES renderer (`render/gles.rs`),
and the Metal `CVMetalTextureCache` aliasing (`render/metal_import.rs`). This is
roughly 3,500 lines of the hardest, most hardware-specific zero-copy engineering
in either codebase (3,463 LOC across the five files), and it has no upstream
counterpart.

moq has no Rust rendering of any kind and no public GPU handle on decoded frames
(../zerocopy.md section 2d). On the Rust side the application receives CPU I420.
For a relay and transcode server that is a defensible scope decision; for a
client SDK it means every rendered frame is decoded on GPU at best, downloaded to
CPU, and re-uploaded by the application. moq is stronger on the NVIDIA transcode
loop and the Windows capture path; on decode-to-render iroh-live is alone and it
is not close (../zerocopy.md section 6).

## Zero-copy

This module is the reason the campaign's zero-copy investment matters end to end.
Every decoder leaf that retains a GPU surface instead of downloading, and every
capture leaf that delivers a DMA-BUF, terminates here: the renderer is the sink
that turns a retained surface into pixels on screen without a round trip through
system memory. It reads a decoded GPU frame over moq's public frame vocabulary
and imports the surface directly into the graphics API.

Import mechanism, per platform and API (all ported as-is, see "What to do"):

- Linux, Vulkan (wgpu backend). `DmaBufImporter::import_nv12`
  (`render/dmabuf_import.rs:232`) imports the NV12 DMA-BUF as a multi-plane
  `VkImage` using `VK_EXT_image_drm_format_modifier` plus external-memory-fd (the
  multi-plane `VkImage` import at `dmabuf_import.rs:368`), then GPU-copies the
  planes into R8 and RG8 textures wgpu can sample. When the decoder's tiling
  modifier is not Vulkan-importable (Intel Y_TILED on ANV, verified on Meteor
  Lake hardware), `VppRetiler` (`dmabuf_import.rs:1057`) runs a VAAPI VPP
  identity blit to re-tile the surface to a compatible modifier (Y_TILED to CCS)
  before import, at `retile` (`dmabuf_import.rs:1136`), invoked conditionally.
  The VPP pipeline parameter buffer is defined via raw libva FFI
  (`VaProcPipelineParameterBuffer` at `dmabuf_import.rs:1026`) because cros-libva
  does not wrap VPP. `create_device_with_dmabuf_extensions`
  (`dmabuf_import.rs:1413`) lets a caller build the wgpu device carrying
  `VK_EXT_external_memory_fd` and the DRM-format-modifier extensions the import
  needs. This is the crown of the zero-copy work and the single hardest file in
  either codebase.
- Linux, EGL (GLES backend). `render/gles_dmabuf.rs` imports the same DMA-BUFs as
  EGLImages via `EGL_EXT_image_dma_buf_import` and `glEGLImageTargetTexture2DOES`,
  binding the Y and UV EGLImages into the `gles::GlesRenderer` NV12 program, for
  GL-only contexts that have no Vulkan device.
- macOS, Metal (wgpu backend). `render/metal_import.rs` aliases a decoded
  VideoToolbox `CVPixelBuffer` into a wgpu-visible Metal texture through
  `CVMetalTextureCache` (IOSurface aliasing), the exact mirror of the DMA-BUF
  path for VideoToolbox and ScreenCaptureKit frames.
- Android, HardwareBuffer. The decode side wraps the `AHardwareBuffer` as a GPU
  handle; the in-tree renderer currently falls back to NV12 download for it, with
  the handle exposed for future zero-copy import (../zerocopy.md section 3, the
  AHardwareBuffer row).

Path selection and fallback (`render.rs:280-359`): `render()` matches on the
frame's native handle, one arm per platform, and any zero-copy import that errors
routes to `into_i420()` for that frame, sets `RenderPath::GpuDownload`, and
returns a correct picture. After three consecutive failures on a given path the
renderer disables it (`render.rs:302-303`) so a broken driver or GPU stops
churning allocations and log lines, and every subsequent frame takes the CPU path
directly. Because `into_i420()` exists on every decoded frame, the renderer draws
correctly on any platform and any decoder, hardware or software, with or without
a working zero-copy path, on either backend. That universal escape hatch is
exactly what makes it safe to track a small `#[non_exhaustive]` public API.

Frame-model relation to B1/B3. The renderer reads a GPU handle from moq's
`decode::Frame::native()` (B3, `../base/B3-decode-native-accessor.md`) and
consumes the public `Native` vocabulary (B1,
`../base/B1-frame-vocabulary.md`): `Native::DmaBuf`, `Native::CvPixelBuffer`,
`Native::D3d11`, `Native::Cuda`, and `Native::HardwareBuffer`. For DMA-BUF it
calls `Native::DmaBuf`'s accessors: `fourcc()`, `modifier()`, `coded_size()`,
`display_size()`, `planes()` (per-plane offset and pitch), and
`export() -> Result<OwnedFd, Error>` for the on-demand descriptor. The renderer
holds no descriptor of its own: it calls `export()` per frame rather than caching
a stored fd, so moq's mint-on-access `DmaBuf` (../zerocopy.md section 1.3) is
what lets a buffered playout queue avoid exhausting the descriptor table. A
renderer built purely on this public vocabulary is the strongest argument that
B1 is a real public API, which is the reason this crate is the intended existence
proof of the base contract.

## What to do

MOVE the renderer and all four importers into a new moq workspace crate, and
delete the iroh-live copies once the new path is proven.

moq-side contribution (target crate `rs/moq-video-render`, a normal workspace
member with feature-gated heavy dependencies):

- `render.rs` (799 lines) carries over whole as the `wgpu` backend:
  `WgpuVideoRenderer` (`render.rs:46`), the `RenderPath` selector (the enum
  `None`, `CpuRgba`, `CpuNv12`, `DmaBuf`, `MetalZeroCopy`, `GpuDownload` at
  `render.rs:70-100`), per-frame path selection in `render()`
  (`render.rs:280-359`) with the DMA-BUF native-handle branch at `render.rs:287`
  and the three-strike disable at `render.rs:302-303`, the Metal branch
  (`render.rs:319-325`), the CPU-download fallback (`render.rs:349`), the
  NV12-to-RGBA fragment pass (`nv12_to_rgba.wgsl`), the I420 and packed upload
  paths, and `render_cached` (`render.rs:576`) for external compositors such as
  Bevy, egui, and dioxus-native.
- `render/dmabuf_import.rs` (1,452 lines) carries over whole as the
  `dmabuf-import` importer: `import_nv12` (`dmabuf_import.rs:232`), the
  multi-plane `VkImage` import (`dmabuf_import.rs:368`), the raw-Vulkan (`ash`)
  DMA-BUF import via `VK_EXT_image_drm_format_modifier` plus external-memory-fd,
  the GPU copy of the NV12 planes into R8 and RG8 textures, the
  `VaProcPipelineParameterBuffer` raw FFI (`dmabuf_import.rs:1026`), `VppRetiler`
  (`dmabuf_import.rs:1057`) with its `retile` Y_TILED-to-CCS identity blit
  (`dmabuf_import.rs:1136`), and `create_device_with_dmabuf_extensions`
  (`dmabuf_import.rs:1413`, renamed to a public constructor helper). This is the
  hardest and most hardware-specific code in the crate; the non-default feature
  gating keeps it out of moq's default and relay builds while the iroh-live
  device test runner validates it on Meteor Lake.
- `render/gles.rs` (536 lines) and `render/gles_dmabuf.rs` (402 lines, 938 LOC
  together) carry over whole as the `gles` backend: the GLES2
  fullscreen-triangle renderer with RGBA and NV12 upload paths, and the EGLImage
  import via `EGL_EXT_image_dma_buf_import` for GL-only contexts. This path is
  load-bearing for embedded and GL-only consumers and MUST NOT be dropped; it
  ships behind its own feature flag beside wgpu.
- `render/metal_import.rs` (274 lines) carries over whole as the `metal-import`
  importer, the `CVMetalTextureCache` aliasing of a VideoToolbox `CVPixelBuffer`,
  consuming `Native::CvPixelBuffer` rather than iroh-live's `AppleGpuFrame`.

Total: 3,463 LOC across the five files, the roughly 3,500 LOC cited throughout
../zerocopy.md.

Cargo wiring (what keeps moq light):

- The crate is added to `[workspace] members` in moq's root `Cargo.toml`
  (`Cargo.toml:2-30`, alongside members such as `moq-nvenc`) and to the default
  members like any other library crate, so release-plz, the workspace tooling,
  and the default `cargo build` and CI own it normally.
- Its default features enable no render backend, so the default build pulls no
  graphics dependency. The two backends are opt-in: `wgpu` (the
  `WgpuVideoRenderer` and the wgsl pipeline, pulling `wgpu`) and `gles` (the
  GLES2 renderer, pulling `glow` and EGL bindings). The importers gate further:
  `dmabuf-import` (Vulkan via `ash`, requires `wgpu`), `gles-dmabuf` (EGLImage
  import, requires `gles`), and `metal-import` (`objc2-metal`, requires `wgpu`).
- Neither `moq-video` (`rs/moq-video/Cargo.toml`) nor `moq-relay`
  (`rs/moq-relay/Cargo.toml`, which does not depend on `moq-video` at all) lists
  `moq-video-render` as a dependency, so building moq-video, moq-relay, or
  moq-transcode never pulls `wgpu`, `ash`, `glow`, or `objc2-metal`. This mirrors
  how `moq-nvenc` is a separate workspace member reached only through optional
  features, except that no other moq crate references `moq-video-render` at all.
- The only moq-video source touch is a documentation paragraph in the B1 RFC or
  the moq-video crate docs pointing readers who want to render decoded frames to
  the sibling `moq-video-render` member and stating that moq-video renders
  nothing in Rust by scope.

Adaptation to moq's vocabulary. Replace every use of iroh-live's `FrameData`,
`GpuFrame`, `GpuFrameInner`, and `NativeFrameHandle` with `decode::Frame`,
`Native`, and the `Native::DmaBuf` accessors. The `render()` body becomes a match
on `frame.native()`, one arm per `Native` variant, each the port of the
equivalent branch in `render.rs:280-359`. The DMA-BUF arm reads
`export() -> OwnedFd` per frame instead of holding a cached `DmaBufInfo.fd`; moq's
`DmaBuf` is the on-demand exporter (../zerocopy.md section 1.3), so the crate
never stores a descriptor. Timestamps read `frame.timestamp` as
`moq_net::Timestamp` with no `Duration` boundary conversion. The crate defines
its own `thiserror` `Error` (import failure, device creation, unsupported
modifier), independent of moq's `Error`, since it is downstream and publishes its
own semver.

The frame-parameter ownership question. moq's `into_i420(self)` consumes the
frame, which collides with a renderer that borrows. Open question: how does
`render()` take the frame; current proposal: option (a), take the frame by value,
`render(&mut self, frame: Frame)`, call `native()` first (it borrows `&self`,
leaving the frame intact for a zero-copy import) and consume the frame with
`into_i420()` only on the no-handle or import-failed branch. Option (b) is to ask
B1/B3 for a borrowing `to_i420(&self)` accessor on `decode::Frame` (a thin public
wrapper over the existing private `frame::Frame::to_i420` at `frame.rs:64`) so
`render(&Frame)` keeps a borrowing signature; that is a base-API request under
the base API freeze. Option (a) needs no moq change and is the default. Do not
clone the frame to work around this, since a GPU-backed frame is not cheaply
clonable in general.

What is dropped from iroh-live. The renderer no longer defines a frame model at
all; it borrows moq's. Dropped: `FrameData`, `GpuFrame`, `GpuFrameInner`, and
`NativeFrameHandle` (`format.rs`), along with the `native_handle()` trait method
and the `download_rgba` and `download_nv12` defaults. The `VideoFrame::rgba_image()`
lazy cache and the `PixelFormat`, `Nv12Planes`, and `GpuPixelFormat` types either
move into the crate as private import-side helpers or are replaced by reading
`Native` directly.

iroh-live removal (the paired `up/video-render` branch, gated on the released
crate). Once the new path is proven, delete the five files in a deletion-only,
revertible commit:

| Module | file | LOC | Note |
|---|---|---:|---|
| renderer core | `rusty-codecs/src/render.rs` | 799 | path selection, shaders, per-frame failure fallback, `render_cached`; ports to `moq-video-render` |
| DMA-BUF import | `rusty-codecs/src/render/dmabuf_import.rs` | 1,452 | raw Vulkan via ash, `VppRetiler` (Intel Y_TILED to CCS), `create_device_with_dmabuf_extensions` |
| GLES DMA-BUF | `rusty-codecs/src/render/gles_dmabuf.rs` | 402 | EGLImage import; carries forward as the GLES backend, not dropped |
| GLES renderer | `rusty-codecs/src/render/gles.rs` | 536 | GL-only-context renderer; carries forward as the GLES backend, not dropped |
| Metal import | `rusty-codecs/src/render/metal_import.rs` | 274 | CVPixelBuffer aliased as Metal texture via `CVMetalTextureCache` |

Total 3,463 LOC, matching the render removal ledger row.

The `format.rs` frame-model collapse, coordinated with the codec removals: the
parallel model the renderer consumes is deleted in the same window. This is the
B1-donor half of `rusty-codecs/src/format.rs`: the public `FrameData::Gpu`,
`GpuFrame`, `GpuFrameInner`, `NativeFrameHandle` (`format.rs:68-87`), and
`DmaBufInfo` (`format.rs:508-528`) collapse onto moq's public `Native` vocabulary
so the two-model translation carried today disappears (../zerocopy.md section 6).
The consumers that implement `GpuFrameInner` today, the VAAPI, VTB, and Android
decoders and the PipeWire capture frames, stop needing it as they migrate, done
per platform in lockstep so a platform never holds both `NativeFrameHandle` and
`Native` at once.

## Tests

Proof before deletion (mandatory per the proof-before-deletion rule, and doubly
load-bearing because this task is the guarantee behind the zero-copy rule P2):

- The hardware-gated `moq-media/tests/zero_copy_pipeline.rs` passes with the
  playout path rendering through `moq-video-render` from moq's
  `decode::Frame::native()`, on Intel Linux (DMA-BUF import, including the
  `VppRetiler` re-tile), on macOS (CVPixelBuffer into Metal), and where hardware
  is available on Windows (D3D11 texture) and Android (HardwareBuffer), before
  the local `render/` tree is deleted.
- A client playback example renders a decoded stream to screen through the crate
  on at least Intel Linux and macOS, with recorded on-hardware results,
  satisfying the platform verification gate for platforms lacking CI. The GLES
  backend is exercised on a GL-only context so the GLES path is proven alongside
  wgpu.
- The CPU fallback (`into_i420()` into the software render path) is verified on a
  GPU-less host so the crate degrades cleanly.
- A test that forces an import error verifies the per-path three-strike disable
  and the per-frame `into_i420()` fallback.

What CI can and cannot verify. The crate's default features pull no graphics
dependency, so moq's default CI compiles it as a thin shell without the graphics
stack. Feature-enabled builds compile on hosts without the hardware per the
adaptation conventions. The zero-copy paths themselves need a GPU and a
windowing system that moq CI lacks; they are validated through the iroh-live
device test runner already on the roadmap rather than on moq's runners
(../zerocopy.md section 4, Option B testability).

## Evidence

- ../zerocopy.md section "2b. Decode to render: verdict ours" (the itemized list
  of what moq lacks) and section "2d. Render itself: verdict ours".
- ../zerocopy.md section "4. Render upstreaming decision" (the placement options
  A, B, C; the decision of record per ../overview.md is the in-tree crate) and
  section "5. Concrete upstream requirements", requirement U4 (the render-crate
  home) and requirement U1 (the keystone public vocabulary).
- ../zerocopy.md section "6. Verdict" (the end-state for the render tree and the
  `FrameData`/`NativeFrameHandle` collapse) and sections "1.1" through "1.3" (the
  two frame models side by side and the on-demand-export shape moq's enum lacks).
- ../zerocopy.md section 3, the vendor-API coverage matrix (the DMA-BUF, Metal,
  EGLImage, Vulkan-import, and wgpu-integration rows are iroh-live-only).
- ../comparison/maps/rusty-codecs.md, section "render/ GPU renderers", naming
  each importer and its role: `WgpuVideoRenderer` (`render.rs:45-799`),
  `render/dmabuf_import.rs`, `render/gles.rs`, `render/gles_dmabuf.rs`,
  `render/metal_import.rs`, and the observation that these render modules are the
  parts most clearly unique to iroh-live.
- ../base/B1-frame-vocabulary.md (the `Native` enum, the `DmaBuf` accessor, and
  the mint-on-access `export()`) and ../base/B3-decode-native-accessor.md (the
  `native()` accessor this crate reads).

Verified against the iroh-live working tree on 2026-07-22: `render.rs` 799
lines, `render/dmabuf_import.rs` 1,452 lines, `render/gles.rs` 536 lines,
`render/gles_dmabuf.rs` 402 lines, `render/metal_import.rs` 274 lines, 3,463 LOC
total.

## Coordination

- Base API freeze (coordination point 1). The crate finalizes only against agreed
  B1 and B3. If it finds a `Native` variant or accessor it needs but the contract
  lacks (a Windows `D3d11` import shape, a `Native::Cuda` accessor), it stops and
  files the gap against B1 or B3 rather than diverging. As the intended existence
  proof of B1, surfacing a gap here is a feature, not a blocker.
- Decode-to-render ordering (the the zero-copy rule zero-copy rule, and coordination
  point 12 upstream gating). This module is the second critical coordination
  gate of the campaign, after the base API freeze: it must land before or
  together with the decode deletions in the sibling codec modules
  codec-vaapi-decode.md, codec-decode-surface.md, and codec-android-mediacodec.md.
  Those decoders are the current producers of the GPU frames the renderer
  imports, so deleting them before the renderer consumes moq's
  `decode::Frame::native()` surfaces would drop the decode-to-render path
  entirely (../zerocopy.md sections 2b and 5). After adoption the renderer imports
  moq's decode surfaces rather than iroh-live's `GpuFrame`, which is precisely
  what lets the local VAAPI, VTB, and Android decoders be removed without dropping
  a pixel path to the screen.
- Decoder dependency. The DMA-BUF path is inert until codec-vaapi-decode.md
  exports a handle, and the Metal and Windows paths until codec-decode-surface.md
  (the VideoToolbox and Media Foundation retain-surface decoders) does. The crate
  compiles and its CPU fallback works before any of them land, so it is not
  blocked, but its zero-copy paths only become exercisable as those decoder
  modules merge.
- Release gate. The crate cannot be adopted by iroh-live until B1, B3,
  codec-decode-surface.md, and the released `moq-video-render` are in a moq
  release iroh-live can pin, and on Linux until codec-vaapi-decode.md gives the
  DMA-BUF path something to import.
- CI hardware gating. moq's current CI is GPU-optional by design; the crate's
  empty default features preserve that posture, and the zero-copy paths are
  validated on the iroh-live device test runner rather than on moq's runners.
- Placement contingencies (on record in case circumstances change; the decision
  of record is the in-tree member). If the public-handle work (B1 and B3) stalls,
  the minimal fallback is keeping the renderer inside iroh-live fully aligned to
  moq's frame model (Option C, ../zerocopy.md section 4), which consumes exactly
  the same moq API and still collapses the two-model translation but forgoes the
  shared moq crate. An out-of-tree published crate over the same public handles
  (Option B) also needs the identical API but makes the renderer a downstream
  crate rather than a moq contribution. Both differ from the chosen approach only
  in where the crate lives.
- UI split. The egui shell moves to moq as `moq-egui` over this crate
  (render-egui.md, its paired branch), while the dioxus shell and the demo apps
  stay in iroh-live. The DMA-BUF modifier findings the retired `gpu-debug` harness
  produced live on in `dmabuf_import.rs` and travel with this module.
- Capture side. The PipeWire DMA-BUF producer (capture-pipewire.md) delivers the
  handles the DMA-BUF import path consumes on the capture-to-encode side; this
  module is the decode-to-render consumer of the same `Native::DmaBuf` vocabulary.
