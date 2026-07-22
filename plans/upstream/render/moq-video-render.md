# moq-video-render. Out-of-tree renderer crate over moq's public frame handles

Branch: moq-upstream/moq-video-render (our own repository; not a moq source branch)
PR target: none against moq source, except the one-paragraph docs link of change 8, which rides in the B1 RFC
Depends on: B1 (Native vocabulary), B3 (decode::Frame::native()), vtb-mf-decode-surface (Apple and Windows decoders retain a surface); on Linux the DMA-BUF path additionally needs vaapi-decode to have something to import, though the crate compiles and its CPU fallback works without it
Path: Both (out-of-tree). The crate is publishable and reusable across the moq ecosystem, yet it lives in our tree and touches no moq source.
Size: 0 upstream. The moq-side diff is a single documentation paragraph (change 8). The crate itself is a port of roughly 3,500 lines of our existing render code, but that authoring lands in our repository, not moq's.

## Goal

Publish `moq-video-render`, an out-of-tree crate that renders a `moq_video::decode::Frame`
to a wgpu (or GLES) RGBA texture with zero copies where the platform allows and a CPU
download everywhere else. This is the maintainer's decided approach for the render stack
(Option B of `comparisons/zerocopy.md` section 4 and `comparisons/moq-changes.md` section
1b): moq-video stays render-free by deliberate scope, so the renderer does not enter moq's
tree, does not add `wgpu`, `ash`, `glow`, or `objc2-metal` to moq's dependency graph, and
does not force per-vendor GPU CI onto maintainers who cannot exercise the code. Instead the
crate consumes moq's public frame handles: it reads `decode::Frame::native()` (B3) to obtain
a `Native` handle (B1), selects a per-frame import path from the handle variant, and falls
back to `into_i420()` when a zero-copy path fails, exactly as our `render.rs:267-359` does
today over our own `NativeFrameHandle`. Its strategic value is the existence proof: a working
third-party renderer built purely on the public `Native` vocabulary is the strongest argument
that B1 is a complete public contract rather than a private convenience. Its downstream value
is that it is the home into which iroh-live moves its `render/` code, letting iroh-live delete
its parallel `FrameData` and `NativeFrameHandle` model and consume moq's single frame
vocabulary.

This plan is not a moq source pull request. moq renders nothing in Rust on purpose
(`comparisons/zerocopy.md` section 2d), and the only thing the renderer needs from moq is the
public `native()` accessor of B3 over the B1 handle enum, plus decoders that actually export a
handle rather than downloading (vtb-mf-decode-surface for Apple and Windows, vaapi-decode for
Linux). The crate is what those base and decoder changes unlock.

## Evidence

- Render upstreaming decision and the Option A/B/C analysis: `comparisons/zerocopy.md`
  section 4 (lines 332-419), with the Option B recommendation at lines 404-419 and the
  discrete requirements U1 through U4 in section 5 (lines 423-487). U4 (lines 471-478) is
  this crate.
- The render-side problem statement and the three placements restated in moq naming:
  `comparisons/moq-changes.md` section 1b, "Problem two: moq renders nothing in Rust"
  (lines 195-202), Option B (lines 257-262), and change 8 in the sequenced list (line 660,
  "Render crate home ... 0 upstream (one paragraph in the U1 RFC)").
- Our render module inventory: `comparisons/maps/rusty-codecs.md` section 3, "render/ GPU
  renderers" (lines 267-297), naming each importer and its role.
- The two frame models side by side, and why on-demand export is the shape moq's enum lacks:
  `comparisons/zerocopy.md` sections 1.1 through 1.3 (lines 25-166).

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
  `render_cached` for external compositors (Bevy, egui, dioxus-native). Carries over whole.
- `render/dmabuf_import.rs` (1,452 lines): the raw-Vulkan (`ash`) DMA-BUF import using
  `VK_EXT_image_drm_format_modifier` plus external-memory-fd, the GPU copy of the NV12 planes
  into R8 and RG8 textures, the `VppRetiler` Y_TILED-to-CCS identity blit for Intel ANV
  modifiers Vulkan cannot import (raw libva FFI because cros-libva does not wrap VPP), and
  `create_device_with_dmabuf_extensions`. Carries over whole; this is the hardest and most
  hardware-specific code in the crate, and Option B keeps it where we run the Meteor Lake
  hardware that validates it.
- `render/gles.rs` (536 lines) and `render/gles_dmabuf.rs` (402 lines): the GLES2 renderer
  and the EGLImage import via `EGL_EXT_image_dma_buf_import` for GL-only contexts. Carry over
  whole.
- `render/metal_import.rs` (274 lines): the `CVMetalTextureCache` aliasing of a decoded
  VideoToolbox `CVPixelBuffer` into a wgpu-visible Metal texture. Carries over whole,
  consuming `Native::CvPixelBuffer` rather than our `AppleGpuFrame`.

What is dropped: our `FrameData`, `GpuFrame`, `GpuFrameInner`, and `NativeFrameHandle`
(`format.rs`), along with the `native_handle()` trait method and the `download_rgba` and
`download_nv12` defaults. The renderer no longer defines a frame model at all; it borrows
moq's. The `VideoFrame::rgba_image()` lazy cache and the `PixelFormat`, `Nv12Planes`, and
`GpuPixelFormat` types either move into the crate as private import-side helpers or are
replaced by reading `Native` directly.

## Target in moq (docs only)

No moq source file changes. The one moq-side artifact is change 8
(`comparisons/moq-changes.md:660`): a single documentation paragraph, added to the B1 RFC or
the `moq-video` crate docs, that points readers wanting to render decoded frames to the
external `moq-video-render` crate, and states that moq-video renders nothing in Rust by
scope. No code, no dependency, no feature flag on the moq side.

## Public API

The crate exposes one renderer type parameterized on nothing moq-internal. It takes a
`decode::Frame` and returns a wgpu texture view. Sketch:

```rust
//! Renders `moq_video::decode::Frame` to a GPU texture with zero copies where the
//! platform allows and a CPU download otherwise. moq-video renders nothing in Rust;
//! this crate is the out-of-tree renderer over its public `Native` frame handles.

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
CCS modifier first. The GLES variants of the DMA-BUF and CPU paths live behind the `gles`
and `gles-dmabuf` features and mirror the wgpu arms.

## wgpu integration

The renderer owns a wgpu `Device` and `Queue` and renders into `wgpu::TextureView`s. NV12
sources, whether zero-copy imported into R8 and RG8 textures or CPU-uploaded, run through the
`nv12_to_rgba.wgsl` fragment pass; I420 from `into_i420()` and packed RGBA upload directly.
The ported `create_device_with_dmabuf_extensions` (`dmabuf_import.rs:1413`, renamed to a
public constructor helper) lets a caller build a device carrying `VK_EXT_external_memory_fd`
and the DRM-format-modifier extensions the Vulkan import needs; a caller who does not use
that helper still gets every CPU path and, on macOS, the Metal zero-copy path. `render_cached`
copies into a persistent texture the crate recreates only on a size change, so an external
compositor holds a stable handle.

## Fallback

Two fallbacks, both already proven in `render.rs`:

1. Per-frame: any zero-copy import that errors routes to `frame.into_i420()` for that frame,
   sets `RenderPath::GpuDownload`, and returns a correct picture.
2. Per-path: after three consecutive failures on a given zero-copy path the renderer disables
   it (`render.rs:302-303`) so a fundamentally unsupported driver or GPU stops churning
   allocations and log lines, and every subsequent frame takes the CPU path directly.

Because `into_i420()` is moq's universal escape hatch and exists on every `decode::Frame`,
the crate renders correctly on any platform and any decoder, hardware or software, with or
without a working zero-copy path. That property is exactly what makes it a safe published
crate to track a small, `#[non_exhaustive]` public API.

## Adaptation notes

- Frame vocabulary: replace every use of our `FrameData`, `GpuFrame`, and `NativeFrameHandle`
  with `decode::Frame`, `Native`, and the `Native::DmaBuf` accessors. The DMA-BUF import
  reads `export() -> OwnedFd` per frame instead of holding our cached `DmaBufInfo.fd`; moq's
  `DmaBuf` is the on-demand exporter (`comparisons/zerocopy.md:160-166`), so the crate never
  stores a descriptor.
- No moq source dependency beyond the public API: the crate depends on the `moq-video` crate
  (or the `moq-frame` crate if B1 homes the vocabulary there) at a released version, plus
  `wgpu`, `ash`, `glow`, and `objc2-metal`, all crates.io, none of which touch moq's graph.
- Timestamps: read `frame.timestamp` as `moq_net::Timestamp`; the renderer does no `Duration`
  boundary conversion.
- Errors: the crate defines its own `thiserror` `Error` (import failure, device creation,
  unsupported modifier), independent of moq's `Error`, since it is downstream and publishes
  its own semver.
- CI and hardware: the crate is tested through the iroh-live device test runner already on
  the roadmap, so no GPU CI burden lands on moq (`comparisons/zerocopy.md:384-385`).

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
- Placement decision already made. Option B is the maintainer's decided approach, so no
  decision gate remains here. The two documented alternatives are recorded below only as
  contingencies.

## Alternatives (recorded, not chosen)

- Option C, keep the renderer inside iroh-live fully aligned to moq's frame model
  (`comparisons/zerocopy.md:386-402`, `comparisons/moq-changes.md:264-268`), is the minimal
  fallback if the public-handle work (B1 and B3) stalls. It still consumes
  `moq_video::decode::Frame` and its `native()` accessor directly and still deletes our
  parallel `FrameData` and `NativeFrameHandle`, so it collapses the two-model translation we
  carry today, but it forgoes reusability and keeps the render stack out of the moq ecosystem.
  It needs exactly the same moq API as Option B; the only difference is that the renderer
  stays iroh-live-private rather than a published crate.
- Option A, an in-tree `moq-video-render` crate inside the moq workspace
  (`comparisons/zerocopy.md:344-367`), is revisit-only, and only if moq-video is ever
  repositioned as a client playback SDK rather than a relay and transcode stack. It would drag
  `wgpu`, `ash`, `glow`, and `objc2-metal` and per-vendor GPU CI into moq's tree, which
  contradicts moq's minimal-dependency and render-free scope, so it is not the opening move.

## Acceptance checklist

- The crate builds on Linux (with and without `dmabuf-import`), macOS (with and without
  `metal-import`), and a GPU-less host, and renders correctly on each via the CPU fallback
  when no zero-copy path applies.
- `render()` and `render_cached()` accept a `moq_video::decode::Frame` and consume only B1
  and B3 public API; no `pub(crate)` moq type is named anywhere in the crate.
- The DMA-BUF path imports a `Native::DmaBuf` frame from moq's VAAPI decoder zero-copy on
  Intel Meteor Lake through the device test runner, including the `VppRetiler` re-tile for a
  Y_TILED modifier, and the Metal path imports a `Native::CvPixelBuffer` from moq's retained
  VideoToolbox decoder.
- Every zero-copy path falls back to `into_i420()` on failure and disables itself after three
  strikes, verified by a test that forces an import error.
- The moq side carries only the change 8 documentation paragraph, with no code, dependency,
  or feature added to moq-video.
- iroh-live can drop its `rusty-codecs/src/render/` tree and its `FrameData`,
  `NativeFrameHandle`, and `GpuFrameInner` types in favor of this crate over
  `moq_video::decode::Frame`, confirming the crate is a complete replacement for the parallel
  frame model.
