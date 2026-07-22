# moq-video-render. In-tree moq render crate over moq's public frame handles

> Campaign: upstream | Kind: leaf plan | Branch: up/video-render |
> PR target: moq monorepo | Read ../0-overview.md first.
Depends on: B1 (Native vocabulary), B3 (decode::Frame::native()), vtb-mf-decode-surface (Apple and Windows decoders retain a surface); on Linux the DMA-BUF path additionally needs vaapi-decode to have something to import, though the crate compiles and its CPU fallback works without it
Path: A (in-tree). A new, normal member of the moq workspace. Neither `moq-video` nor `moq-relay` depends on it, and its heavy dependencies (wgpu, ash/Vulkan, EGL/GLES bindings, Metal bindings) sit behind non-default Cargo features, so moq's default and relay builds stay light.
Size: L-XL. A new moq workspace crate carrying the port of roughly 3,500 lines of our existing render code (3,463 LOC across five files), plus the feature wiring that keeps the graphics stacks off the default build. This is a real moq contribution, not a documentation paragraph.

## Goal

Add `moq-video-render`, a new crate in the moq workspace that renders a
`moq_video::decode::Frame` to an RGBA texture with zero copies where the platform
allows and a CPU download everywhere else, through either of two render backends:
wgpu or GLES, each behind its own feature flag. The render primitives live in moq,
in-tree, as a separate workspace member rather than a feature of moq-video, so moq
owns the render vocabulary while its default and relay builds stay render-free
(see `../0-overview.md`, render leaves). The crate is a normal workspace member;
what keeps moq light is that only its heavy dependencies are feature-gated:
building moq-video, moq-relay, or moq-transcode never pulls `wgpu`, `ash`,
`glow`, or `objc2-metal`, because no other moq crate depends on
`moq-video-render` and the crate's default features enable no graphics stack. It
consumes moq's public frame handles: it reads `decode::Frame::native()` (B3) to
obtain a `Native` handle (B1), selects a per-frame import path from the handle
variant, and falls back to `into_i420()` when a zero-copy path fails, exactly as
our `render.rs:267-359` does today over our own `NativeFrameHandle`. Its value is
twofold. It is a real moq contribution, a new workspace crate carrying the ported
render code rather than a single documentation paragraph. It is also the
existence proof that B1 is a complete public contract, since a renderer built
purely on the public `Native` vocabulary is the strongest argument that the
vocabulary is a real public API rather than a private convenience. Its downstream
value is that it is the home into which iroh-live moves its `render/` code,
letting iroh-live delete its parallel `FrameData` and `NativeFrameHandle` model
and consume moq's single frame vocabulary.

Both render backends iroh-live carries today move into the crate and both carry
forward: the wgpu path (`render.rs` and its importers) and the GLES path
(`gles.rs` plus `gles_dmabuf.rs` for GL-only contexts). The GLES path is not
dropped; embedded and GL-only consumers depend on it.

The UI-framework split: the egui shell (iroh-live's `moq-media-egui`) moves to
moq as `moq-egui` over this crate, under the sibling leaf `moq-egui.md` and its
paired `up/moq-egui` branch. The dioxus shell (`moq-media-dioxus`) and the demo
apps stay in iroh-live and consume these moq render primitives.

This plan is a moq source pull request that adds the `moq-video-render` member to
the moq workspace. moq's default and relay builds still render nothing
(`../comparisons/zerocopy.md` section 2d), because the render code lives in a
member no other moq crate depends on and its graphics dependencies are behind
non-default features. What the crate needs from moq beyond its own place in the
workspace is the public `native()` accessor of B3 over the B1 handle enum, plus
decoders that actually export a handle rather than downloading
(vtb-mf-decode-surface for Apple and Windows, vaapi-decode for Linux). The crate
is what those base and decoder changes unlock.

## Evidence

- Render placement analysis: `../comparisons/zerocopy.md` section 4 (lines
  332-419) lays out the placement options; the decision of record
  (`../0-overview.md`) is the in-tree crate, a normal workspace member with
  feature-gated heavy dependencies. The discrete requirements U1 through U4 are
  in section 5 (lines 423-487); U4 (lines 471-478) is this crate.
- The render-side problem statement and the placements restated in moq naming:
  `../comparisons/moq-changes.md` section 1b, "Problem two: moq renders nothing
  in Rust" (lines 195-202), and change 8 in the sequenced list (a new workspace
  member with feature-gated heavy dependencies).
- Our render module inventory: `../comparisons/maps/rusty-codecs.md` section 3,
  "render/ GPU renderers" (lines 267-297), naming each importer and its role.
- The two frame models side by side, and why on-demand export is the shape moq's
  enum lacks: `../comparisons/zerocopy.md` sections 1.1 through 1.3 (lines
  25-166).

Verified against source at iroh-live working tree on 2026-07-22:

- `rusty-codecs/src/render.rs`: 799 lines. `WgpuVideoRenderer` struct at `render.rs:46`,
  `RenderPath` enum (`None`, `CpuRgba`, `CpuNv12`, `DmaBuf`, `MetalZeroCopy`, `GpuDownload`)
  at `render.rs:70-100`, per-frame path selection in `render()` at `render.rs:280-359`, the
  DMA-BUF native-handle branch at `render.rs:287` with the three-strike disable at
  `render.rs:302-303`, the Metal branch at `render.rs:319-325`, the CPU-download fallback at
  `render.rs:349`, and `render_cached` at `render.rs:576`.
- `rusty-codecs/src/render/dmabuf_import.rs`: 1,452 lines. `import_nv12` at
  `dmabuf_import.rs:232`, the multi-plane `VkImage` import at `dmabuf_import.rs:368`,
  `VaProcPipelineParameterBuffer` raw FFI at `dmabuf_import.rs:1026`, `VppRetiler` at
  `dmabuf_import.rs:1057`, its `retile` (Y_TILED to CCS identity blit) at
  `dmabuf_import.rs:1136`, and `create_device_with_dmabuf_extensions` at
  `dmabuf_import.rs:1413`.
- `rusty-codecs/src/render/gles.rs` (536 lines), `render/gles_dmabuf.rs` (402 lines,
  EGLImage import), `render/metal_import.rs` (274 lines, `CVMetalTextureCache` aliasing).
  Total across the five files is 3,463 lines, the roughly 3,500 LOC cited throughout the
  comparisons.

## moq API consumed

Only the frozen base contract, nothing behind a `pub(crate)` seam:

- `moq_video::decode::Frame` as the renderer's input frame type (B3, defined at
  `rs/moq-video/src/decode/mod.rs:36-46`).
- `decode::Frame::native(&self) -> Option<Native>` (B3, the accessor beside `into_i420()` at
  `decode/mod.rs:94-101`). `None` means the frame is CPU-resident I420.
- `decode::Frame::into_i420(self) -> Result<Bytes, Error>` (existing,
  `decode/mod.rs:94`; it consumes the frame, as its doc states) as the universal
  CPU fallback when `native()` is `None` or a zero-copy import fails. This
  consuming signature collides with a renderer that borrows: `render(&mut self,
  frame: &Frame)` holds only a shared reference, and moq exposes no borrowing
  download on `decode::Frame` (the private `frame::Frame::to_i420(&self) -> Cow`
  at `frame.rs:64` is not surfaced publicly). There are two ways to resolve it,
  and the crate must pick one and hold to it. Either (a) take the frame by value,
  `render(&mut self, frame: Frame)`, call `native()` first (it borrows `&self`,
  leaving the frame intact for a zero-copy import) and consume the frame with
  `into_i420()` only on the no-handle or import-failed branch; or (b) ask B1/B3
  for a borrowing `to_i420(&self)` accessor on `decode::Frame` (a thin public
  wrapper over the existing private `frame::Frame::to_i420`) so `render(&Frame)`
  keeps its borrowing signature. Option (b) is a base-API request under
  coordination point 1; option (a) needs no moq change and is the default. Do not
  clone the frame to work around this, since a GPU-backed frame is not cheaply
  cloneable in general.
- The `Native` enum and its `DmaBuf`, `CvPixelBuffer`, `D3d11`, `Cuda`, and `HardwareBuffer`
  variants (B1). For DMA-BUF the crate calls `Native::DmaBuf`'s accessors: `fourcc()`,
  `modifier()`, `coded_size()`, `display_size()`, `planes()` (offset and pitch per plane),
  and `export() -> Result<OwnedFd, Error>` for the on-demand descriptor.
- `Frame::size` and `Frame::timestamp` for texture sizing and any presentation bookkeeping.

The crate names no moq backend type, no `crate::frame::Frame`, and no `Candidate` table. If
a platform surface cannot be expressed through a `Native` variant the crate needs (for
example a Windows path with no `D3d11` accessor shape it can import), that is a gap in B1 to
file against the base plan under coordination point 1, not something the crate improvises.

## Source to port

All five files move largely as-is, with only the input type and the handle accessors
rewritten from our vocabulary to moq's:

- `render.rs` (799 lines): `WgpuVideoRenderer`, the `RenderPath` selector, the NV12-to-RGBA
  fragment shader pass (`nv12_to_rgba.wgsl`), the I420 and packed upload paths, the
  three-strike failure counters that disable a zero-copy path and drop to CPU, and
  `render_cached` for external compositors (Bevy, egui, dioxus-native). Carries over whole
  as the `wgpu` backend.
- `render/dmabuf_import.rs` (1,452 lines): the raw-Vulkan (`ash`) DMA-BUF import using
  `VK_EXT_image_drm_format_modifier` plus external-memory-fd, the GPU copy of the NV12 planes
  into R8 and RG8 textures, the `VppRetiler` Y_TILED-to-CCS identity blit for Intel ANV
  modifiers Vulkan cannot import (raw libva FFI because cros-libva does not wrap VPP), and
  `create_device_with_dmabuf_extensions`. Carries over whole; this is the hardest and most
  hardware-specific code in the crate, and the non-default feature gating keeps it out of
  moq's default and relay builds while the iroh-live device test runner validates it on the
  Meteor Lake hardware we run.
- `render/gles.rs` (536 lines) and `render/gles_dmabuf.rs` (402 lines): the GLES2 renderer
  and the EGLImage import via `EGL_EXT_image_dma_buf_import` for GL-only contexts. Carry over
  whole as the `gles` backend; this path is load-bearing for embedded and GL-only consumers
  and is not dropped.
- `render/metal_import.rs` (274 lines): the `CVMetalTextureCache` aliasing of a decoded
  VideoToolbox `CVPixelBuffer` into a wgpu-visible Metal texture. Carries over whole,
  consuming `Native::CvPixelBuffer` rather than our `AppleGpuFrame`.

What is dropped: our `FrameData`, `GpuFrame`, `GpuFrameInner`, and `NativeFrameHandle`
(`format.rs`), along with the `native_handle()` trait method and the `download_rgba` and
`download_nv12` defaults. The renderer no longer defines a frame model at all; it borrows
moq's. The `VideoFrame::rgba_image()` lazy cache and the `PixelFormat`, `Nv12Planes`, and
`GpuPixelFormat` types either move into the crate as private import-side helpers or are
replaced by reading `Native` directly.

## Target in moq (a new workspace member with feature-gated heavy dependencies)

A new `moq-video-render` crate under `rs/moq-video-render`, added to the moq workspace as a
normal member. The Cargo wiring is what keeps its graphics dependencies off the default and
relay builds while the crate itself is an ordinary part of the tree:

- It is added to the `[workspace] members` array in moq's root `Cargo.toml` (the members
  list at `Cargo.toml:2-30`, alongside members such as `moq-nvenc` at `Cargo.toml:20`) and
  to the default members like any other library crate, so release-plz, the workspace
  tooling, and the default `cargo build` and CI own it normally.
- Its default features enable no render backend, so the default build of the crate pulls no
  graphics dependency. The two render backends are opt-in features: `wgpu` (the
  `WgpuVideoRenderer` and the wgsl pipeline, pulling `wgpu`) and `gles` (the GLES2 renderer,
  pulling the `glow` and EGL bindings). The importers gate further: `dmabuf-import` (Vulkan
  via `ash`, requires `wgpu`), `gles-dmabuf` (EGLImage import, requires `gles`), and
  `metal-import` (`objc2-metal`, requires `wgpu`). A consumer pulls only the backend and
  importers it enables.
- Neither `moq-video` (`rs/moq-video/Cargo.toml`) nor `moq-relay`
  (`rs/moq-relay/Cargo.toml`, which does not depend on `moq-video` at all) lists
  `moq-video-render` as a dependency, so building moq-video, moq-relay, or moq-transcode
  never pulls `wgpu`, `ash`, `glow`, or `objc2-metal`. This mirrors how `moq-nvenc` is a
  separate workspace member (`Cargo.toml:83`) that `moq-video` reaches only through its
  optional `nvenc` and `nvdec` features, except that `moq-video-render` is referenced by no
  other moq crate at all.

The one documentation touch, change 8, is a paragraph in the B1 RFC or the `moq-video`
crate docs that points readers wanting to render decoded frames to the sibling
`moq-video-render` member and states that moq-video itself renders nothing in Rust by
scope. That is a pointer to another crate in the same workspace, not to an external
repository.

## Public API

The crate exposes one renderer type per backend, each behind its feature flag and
parameterized on nothing moq-internal: `WgpuVideoRenderer` under `wgpu` and a GLES
renderer under `gles`, sharing the `RenderPath` diagnostics vocabulary. Each takes a
`decode::Frame` and returns a texture for its API. Sketch of the wgpu backend (the GLES
backend mirrors it over a GL context and `glow` texture handles):

```rust
//! Renders `moq_video::decode::Frame` to a GPU texture with zero copies where the
//! platform allows and a CPU download otherwise. moq-video renders nothing in Rust;
//! this crate is moq's in-tree render member over its public `Native` frame handles.
//! Backends: `wgpu` and `gles`, each behind its own feature flag.

use moq_video::decode::Frame;
use moq_video::frame::Native; // or the moq-frame crate, per B1's chosen home

pub struct VideoRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    // NV12-to-RGBA pipeline, staging textures, and per-path importers.
    dmabuf: Option<DmaBufImporter>,     // Linux, feature = "dmabuf-import"
    metal: Option<MetalImporter>,       // macOS, feature = "metal-import"
    failures: PathFailures,             // three-strike disable per zero-copy path
    last_path: RenderPath,
}

/// Which path rendered the most recent frame, for diagnostics and tests.
#[non_exhaustive]
pub enum RenderPath {
    None,
    CpuI420,       // decode::Frame::into_i420 upload plus shader convert
    CpuNv12,
    DmaBuf,        // Native::DmaBuf, Vulkan import (VppRetiler if needed)
    MetalZeroCopy, // Native::CvPixelBuffer, CVMetalTextureCache
    GpuDownload,   // zero-copy attempt failed; into_i420 fallback
}

impl VideoRenderer {
    /// Creates a renderer. `create_device_with_extensions` is the helper ported from
    /// `render/dmabuf_import.rs:1413` so callers can build a wgpu device carrying the
    /// external-memory extensions the DMA-BUF path needs.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self { /* ... */ }

    /// Renders one decoded frame and returns the RGBA texture view. Selects a path from
    /// `frame.native()`, and on any zero-copy failure disables that path after three
    /// strikes and falls through to the CPU download. The parameter is `Frame` by value
    /// (option (a) in "moq API consumed") so the CPU fallback can consume it through
    /// `into_i420(self)`; switch to `&Frame` only if B1/B3 add a borrowing `to_i420(&self)`.
    pub fn render(&mut self, frame: Frame) -> Result<&wgpu::TextureView, Error> { /* ... */ }

    /// Renders into a persistent, cheaply cloneable texture for external compositors.
    /// Takes `Frame` by value for the same CPU-fallback ownership reason as `render`.
    pub fn render_cached(&mut self, frame: Frame) -> Result<&CachedOutput, Error> { /* ... */ }

    pub fn last_path(&self) -> RenderPath { self.last_path }
}
```

## Per-platform native-handle to texture mapping

The `render()` body is a match on `frame.native()`, one arm per `Native` variant, each the
port of the equivalent branch in `render.rs:280-359`:

```rust
let path = match frame.native() {
    #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
    Some(Native::DmaBuf(db)) => {
        // Port of render.rs:287-317 over Native::DmaBuf's accessors instead of DmaBufInfo.
        // fourcc(), modifier(), coded_size(), display_size(), planes(), and export() give the
        // importer everything the descriptor builder needs.
        match self.dmabuf.import_nv12(&db) {          // dmabuf_import.rs:232; VppRetiler at :1136
            Ok(view) => { self.failures.dmabuf = 0; RenderPath::DmaBuf }
            Err(_)   => { self.failures.dmabuf += 1;  // three strikes, then stop trying
                          return self.render_i420(frame); }
        }
    }
    #[cfg(all(target_os = "macos", feature = "metal-import"))]
    Some(Native::CvPixelBuffer(surface)) => {
        match self.metal.import(&surface) {           // metal_import.rs CVMetalTextureCache
            Ok(view) => RenderPath::MetalZeroCopy,
            Err(_)   => return self.render_i420(frame),
        }
    }
    #[cfg(target_os = "windows")]
    Some(Native::D3d11(_texture)) => {
        // Future: D3D11 shared-texture import. Until then, CPU fallback.
        return self.render_i420(frame);
    }
    #[cfg(all(target_os = "linux", feature = "nvdec"))]
    Some(Native::Cuda(_)) => return self.render_i420(frame), // no CUDA import path today
    #[cfg(target_os = "android")]
    Some(Native::HardwareBuffer(_)) => return self.render_i420(frame), // NV12 download today
    None | Some(_) => return self.render_i420(frame),        // CPU frame: universal path
};
```

The DMA-BUF arm is the one that carries the Intel modifier-compatibility work: when
`db.modifier()` is a tiling `import_nv12` cannot bind (Y_TILED on ANV, verified on Meteor
Lake), `VppRetiler::retile` (`dmabuf_import.rs:1136`) runs a VAAPI VPP identity blit to a
CCS modifier first. The GLES backend mirrors these arms over EGL: its DMA-BUF import goes
through `EGL_EXT_image_dma_buf_import` (`gles-dmabuf`) and its CPU paths through GL texture
uploads, so a GL-only context gets the same path selection and fallback behavior.

## wgpu integration

The wgpu backend owns a wgpu `Device` and `Queue` and renders into `wgpu::TextureView`s.
NV12 sources, whether zero-copy imported into R8 and RG8 textures or CPU-uploaded, run
through the `nv12_to_rgba.wgsl` fragment pass; I420 from `into_i420()` and packed RGBA
upload directly. The ported `create_device_with_dmabuf_extensions`
(`dmabuf_import.rs:1413`, renamed to a public constructor helper) lets a caller build a
device carrying `VK_EXT_external_memory_fd` and the DRM-format-modifier extensions the
Vulkan import needs; a caller who does not use that helper still gets every CPU path and,
on macOS, the Metal zero-copy path. `render_cached` copies into a persistent texture the
crate recreates only on a size change, so an external compositor holds a stable handle.
The GLES backend provides the equivalent over a caller-supplied GL context.

## Fallback

Two fallbacks, both already proven in `render.rs`:

1. Per-frame: any zero-copy import that errors routes to `frame.into_i420()` for that frame,
   sets `RenderPath::GpuDownload`, and returns a correct picture.
2. Per-path: after three consecutive failures on a given zero-copy path the renderer disables
   it (`render.rs:302-303`) so a fundamentally unsupported driver or GPU stops churning
   allocations and log lines, and every subsequent frame takes the CPU path directly.

Because `into_i420()` is moq's universal escape hatch and exists on every `decode::Frame`,
the crate renders correctly on any platform and any decoder, hardware or software, with or
without a working zero-copy path, on either backend. That property is exactly what makes it
a safe crate to track a small, `#[non_exhaustive]` public API.

## Adaptation notes

- Frame vocabulary: replace every use of our `FrameData`, `GpuFrame`, and `NativeFrameHandle`
  with `decode::Frame`, `Native`, and the `Native::DmaBuf` accessors. The DMA-BUF import
  reads `export() -> OwnedFd` per frame instead of holding our cached `DmaBufInfo.fd`; moq's
  `DmaBuf` is the on-demand exporter (`../comparisons/zerocopy.md:160-166`), so the crate
  never stores a descriptor.
- Dependencies: as a workspace member the crate depends on the `moq-video` crate (or the
  `moq-frame` crate if B1 homes the vocabulary there) through the workspace, plus `wgpu`,
  `ash`, `glow`, and `objc2-metal` from crates.io, every one of them optional and reached
  only through the crate's non-default features. Because no other moq crate depends on
  `moq-video-render` and its default features are empty, none of these graphics crates
  reach the default or relay dependency graph.
- Timestamps: read `frame.timestamp` as `moq_net::Timestamp`; the renderer does no `Duration`
  boundary conversion.
- Errors: the crate defines its own `thiserror` `Error` (import failure, device creation,
  unsupported modifier), independent of moq's `Error`, since it is downstream and publishes
  its own semver.
- CI and hardware: the crate's default features pull no graphics dependency, so moq's
  default CI compiles it as a thin shell without the graphics stack; feature-enabled builds
  compile on hosts without the hardware per the adaptation conventions, and the zero-copy
  paths are validated through the iroh-live device test runner already on the roadmap
  rather than on moq's runners (`../comparisons/zerocopy.md:384-385`).

## Counterpart

The paired iroh-live `up/video-render` branch executes
`../counterpart/render-adopt.md`: it adds `moq-video-render` as a dependency
(enabling the `wgpu` and `gles` backend features its platforms need), routes the
client playout path through it over `decode::Frame::native()`, proves the
zero-copy pipeline per platform, then deletes `rusty-codecs/src/render.rs` and
the whole `render/` tree (3,463 LOC) and collapses the parallel
`FrameData`/`NativeFrameHandle` frame model onto moq's `Native` vocabulary, in
coordination with `../counterpart/codec-remove.md`. The sibling leaf
`moq-egui.md` then moves iroh-live's `moq-media-egui` shell to moq as `moq-egui`
over this crate; the dioxus shell stays in iroh-live.

## Coordination

- Base API freeze (coordination point 1). The crate finalizes only against agreed B1 and B3.
  If it finds a `Native` variant or accessor it needs but the contract lacks (a Windows
  `D3d11` import shape, a `Native::Cuda` accessor), it stops and files the gap against B1 or
  B3 rather than diverging. As the intended existence proof of B1, this crate is also the
  best early check that the vocabulary is sufficient; surfacing a gap here is a feature, not a
  blocker.
- Decoder dependency. The DMA-BUF path is inert until vaapi-decode exports a handle, and the
  Metal and Windows paths until vtb-mf-decode-surface does. The crate compiles and its CPU
  fallback works before any of them land, so it is not blocked, but its zero-copy paths only
  become exercisable as those decoder leaves merge.
- Placement. The decision of record (`../0-overview.md`) is the in-tree workspace member
  with feature-gated heavy dependencies; no decision gate remains here. Two contingencies
  are on record in case circumstances change: if the public-handle work (B1 and B3) stalls,
  the minimal fallback is keeping the renderer inside iroh-live fully aligned to moq's
  frame model (`../comparisons/zerocopy.md:386-402`,
  `../comparisons/moq-changes.md:264-268`), which consumes exactly the same moq API and
  still collapses the two-model translation but forgoes the shared moq crate; and an
  out-of-tree published crate over the same public handles
  (`../comparisons/zerocopy.md:368-385`) also needs the identical API but makes the
  renderer a downstream crate rather than a moq contribution. Both differ from the chosen
  approach only in where the crate lives.

## Acceptance checklist

- The crate builds on Linux (with and without `dmabuf-import`), macOS (with and without
  `metal-import`), and a GPU-less host, and renders correctly on each via the CPU fallback
  when no zero-copy path applies.
- Both render backends build and work behind their feature flags: `wgpu` (the
  `WgpuVideoRenderer` path) and `gles` (the GLES2 renderer, including the `gles-dmabuf`
  EGLImage import on a GL-only context). The GLES path is a first-class backend, not a
  leftover.
- `render()` and `render_cached()` accept a `moq_video::decode::Frame` and consume only B1
  and B3 public API; no `pub(crate)` moq type is named anywhere in the crate.
- The DMA-BUF path imports a `Native::DmaBuf` frame from moq's VAAPI decoder zero-copy on
  Intel Meteor Lake through the device test runner, including the `VppRetiler` re-tile for a
  Y_TILED modifier, and the Metal path imports a `Native::CvPixelBuffer` from moq's retained
  VideoToolbox decoder.
- Every zero-copy path falls back to `into_i420()` on failure and disables itself after three
  strikes, verified by a test that forces an import error.
- The `moq-video-render` crate is a normal moq workspace member that no other moq crate
  depends on, with empty default features, so moq's default and relay builds add no `wgpu`,
  `ash`, `glow`, or `objc2-metal`; the only moq-video source touch is the change 8
  documentation paragraph pointing to the new member.
- iroh-live can drop its `rusty-codecs/src/render/` tree and its `FrameData`,
  `NativeFrameHandle`, and `GpuFrameInner` types in favor of this crate over
  `moq_video::decode::Frame` (the `up/video-render` pair branch,
  `../counterpart/render-adopt.md`), confirming the crate is a complete replacement for the
  parallel frame model.
