# Zero-copy: the decisive asset

> Campaign: upstream (media stack) | Kind: the load-bearing comparison |
> Read `overview.md` first; the comparison matrix is `comparison.md`.
>
> This is the campaign's most important document. Zero-copy is where iroh-live's
> media stack is decisively ahead of moq, and preserving every zero-copy path
> through the upstreaming is the hard constraint that orders the whole campaign.
> The decode-to-render path (section 2b, 2d) is iroh-live's alone, across three
> platforms and two graphics APIs, and only survives the decoder deletions if the
> renderer (module `modules/render-video-render.md`) and the decode-surface
> retention (`modules/codec-decode-surface.md`) land first. Every module doc
> carries a `Zero-copy:` line and links back here.

Scope: the GPU frame models and every zero-copy path (capture to encode, decode to
render, transcode, and render) in iroh-live's rusty-codecs and rusty-capture against
moq's `rs/moq-video`, `rs/moq-nvenc`, and `rs/moq-transcode`. iroh-live citations are
working-tree `file:line`; moq citations are `rs/...:line` read directly from the moq
working tree at HEAD `3a3e0ea8` (dev merged into main on 2026-07-21, so there is a
single moq codebase now and no branch split to reconcile). This document is the
companion of `comparison/moq-changes.md`, which carries the concrete moq API sketches; the
requirements here reference that document rather than repeating its code.

Conclusion up front: the two zero-copy investments barely overlap. moq built the
NVIDIA end-to-end transcode loop and the Windows capture-to-encode path; iroh-live
built Linux VAAPI and DMA-BUF in both directions, decode-to-render import on three
platforms and two graphics APIs, and a wgpu renderer. The only genuinely shared path
is macOS capture into VideoToolbox encode. The maintainer now intends to upstream the
render stack as well, which turns the central question from "how do we preserve our
paths" into "where does the renderer live and what must moq expose so it can." Section
4 answers that concretely and section 5 lists the discrete upstream requirements.

---

## 1. The two frame and surface models side by side

### 1.1 iroh-live: public `FrameData::Gpu` and `NativeFrameHandle`

One public frame type spans the whole pipeline: capture sources produce it, encoders
consume it, decoders produce it, and the renderer consumes it. The backing storage
enum (`rusty-codecs/src/format.rs:544-566`):

```rust
/// Backing storage for a video frame.
///
/// Variants cover the common pixel layouts produced by capture devices and
/// hardware decoders. Encoders match on the variant to pick the cheapest
/// conversion path (e.g. a VAAPI encoder can consume [`Nv12`](Self::Nv12) or
/// [`Gpu`](Self::Gpu) directly, avoiding an extra color-space round-trip).
#[derive(derive_more::Debug, Clone)]
pub enum FrameData {
    /// Packed RGBA or BGRA pixel data in CPU memory.
    Packed { pixel_format: PixelFormat, data: bytes::Bytes },
    /// Planar I420 (YUV 4:2:0) in CPU memory.
    I420 { y: Bytes, u: Bytes, v: Bytes },
    /// Semi-planar NV12 in CPU memory.
    Nv12(Nv12Planes),
    /// GPU-resident frame from a hardware decoder or capture device.
    Gpu(GpuFrame),
}
```

`GpuFrame` is `Arc<dyn GpuFrameInner>` (`format.rs:420-424`); the public trait
(`format.rs:481-503`) requires `download_rgba`, `gpu_pixel_format`, and `dimensions`,
with defaulted `download_nv12` and `native_handle`. `native_handle` returns the
platform zero-copy handle (`format.rs:68-87`):

```rust
/// Platform-specific handle for zero-copy GPU frame import/export.
///
/// Each variant is gated on the target platform. On platforms with no
/// supported handle type the enum is uninhabited and
/// `Option<&NativeFrameHandle>` is always `None`.
#[derive(Debug)]
#[non_exhaustive]
pub enum NativeFrameHandle {
    /// Linux DMA-BUF file descriptor and layout metadata.
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBufInfo),
    /// Android HardwareBuffer handle for zero-copy GPU import.
    #[cfg(target_os = "android")]
    HardwareBuffer(HardwareBufferInfo),
    /// macOS CVPixelBuffer for zero-copy VideoToolbox encode and Metal render.
    #[cfg(all(target_os = "macos", feature = "apple-gpu"))]
    CvPixelBuffer(CvPixelBufferInfo),
    // Future variants:
    // #[cfg(target_os = "windows")] D3D11Texture(D3D11TextureInfo),
}
```

`DmaBufInfo` carries `fd: OwnedFd`, `modifier`, `drm_format`, coded and display
dimensions, and per-plane `offset` and `pitch` (`format.rs:508-528`). The handle is
computed on demand: `GpuFrame::native_handle` documents "no file descriptors are held
per frame" (`format.rs:462-464`), and the VAAPI decoder caches the PRIME export in a
`OnceCell<Option<CachedDmaBufExport>>` (`codec/vaapi/decoder.rs:85-113`) so repeated
calls from the render thread and the playout buffer dup the same fd
(`decoder.rs:228`) instead of re-running surface import, sync, and export.

### 1.2 moq: crate-private `Frame` enum with an I420 escape hatch

moq-video's raw frame type is crate-internal (`rs/moq-video/src/frame.rs:23-36`):

```rust
pub(crate) enum Frame {
	/// Zero-copy GPU surface (macOS `CVPixelBuffer`).
	#[cfg(target_os = "macos")]
	Surface(macos::Surface),
	/// Zero-copy GPU texture (Windows Direct3D11 NV12).
	#[cfg(target_os = "windows")]
	Texture(d3d11::Texture),
	/// Zero-copy GPU buffer (Linux CUDA NV12). Produced only by the NVDEC
	/// decoder, consumed in place by the NVENC encoder.
	#[cfg(all(target_os = "linux", feature = "nvdec"))]
	Cuda(cuda::Frame),
	/// CPU-resident planar I420.
	I420(I420),
}
```

`Frame::to_i420() -> Cow<'_, I420>` downloads the GPU variants on demand
(`frame.rs:63-74`) and is the universal escape hatch. The public decode output wraps
the private union (`rs/moq-video/src/decode/mod.rs:36-46`):

```rust
pub struct Frame {
	/// Presentation timestamp, carried through from the container. ...
	pub timestamp: Timestamp,
	/// The decoded resolution ...
	pub size: Size,
	/// The pixels: CPU I420 or a GPU surface.
	pub(crate) inner: crate::frame::Frame,
}
```

The only public ways out of a decoded frame are
`Frame::into_i420() -> Result<Bytes, Error>` (`decode/mod.rs:94-101`, a CPU download
for GPU variants) and `Encoder::encode(&decode::Frame, keyframe)`
(`encode/encoder.rs:249`, the transcode input that keeps CUDA frames on the GPU).
There is no public GPU handle, no renderer, and the encode and decode `Backend` traits
are `pub(crate)` (`encode/backend/mod.rs:37-57`, `decode/backend/mod.rs:56-75`). This
is a stated stability policy: no backend types in the public API, all configs
`#[non_exhaustive]` (`lib.rs:35-44`).

### 1.3 Analysis

Extensibility. A `DmaBuf` variant fits their enum mechanically, since it is already
per-platform cfg-gated and `pub(crate)`, so adding
`#[cfg(all(target_os = "linux", feature = "vaapi"))] DmaBuf(...)` breaks nothing. The
hard part is not the variant but the consumers: nothing outside the crate can see
`crate::frame::Frame`, so a DmaBuf variant is useful only to in-tree backends until a
public accessor or public handle type exists. Our `NativeFrameHandle` is
`#[non_exhaustive]` for exactly that reason, and the commented `D3D11Texture` variant
(`format.rs:85-86`) shows the intended growth path.

Visibility consequences. Their private model lets them refactor freely and keeps the
codec-library churn out of the public API, but it also means the GPU work is invisible
to anyone downstream: a renderer, a compositor, or a third-party hardware decoder
cannot participate. Our model is a public contract. `GpuFrameInner` is a public trait,
so rusty-capture implements it for PipeWire DMA-BUF frames
(`rusty-capture/src/platform/linux/pipewire.rs:145-247`) and rusty-codecs implements it
for VAAPI, VideoToolbox, and Android decoder outputs, all without the frame type
knowing about any of them. The cost is that `GpuFrameInner`'s method set is now semver
surface, and the `download_rgba` requirement bakes a CPU fallback into every
implementation.

Refcount semantics. Both sides settled on cheap-clone refcounting. Our `GpuFrame`
clones an `Arc` (`format.rs:420-424`); their `Surface` clone is a CoreFoundation
retain, their `Texture` clone is a COM `AddRef`, and their `cuda::Frame` clones an
`Arc<Buffer>` (`frame.rs`, per-variant `Clone`). The structural difference is handle
ownership. Their enum holds the resource itself, so the variant is the storage; our
`native_handle()` mints an owned handle per call (a dup'd fd for DMA-BUF, a retain for
CVPixelBuffer), with the VAAPI decoder caching the expensive export behind the frame
(`vaapi/decoder.rs:85-113`). Their model cannot express "export on demand" because the
variant is the storage, while ours pays one `try_clone` per handle access
(`vaapi/decoder.rs:228`). This matters for upstreaming: a public handle accessor on
moq's `decode::Frame` (section 5, U1) is the on-demand-export shape their enum does not
have today, and it is the single change everything else in this document depends on.

---

## 2. Path-by-path comparison

### 2a. Capture to encode: verdict complementary

iroh-live. PipeWire screen and camera capture negotiates `SPA_DATA_DmaBuf` and wraps
the dup'd fd as a `PipeWireDmaBufFrame: GpuFrameInner` exposing
`NativeFrameHandle::DmaBuf`; only NV12 DMA-BUFs take the zero-copy path
(`rusty-capture/src/platform/linux/pipewire.rs:145-247, 721-766`). The VAAPI encoder
consumes that handle directly: `push_frame` checks `native_handle()` first
(`codec/vaapi/encoder.rs:1268-1289`), and a matching NV12 DMA-BUF is imported as a
VAAPI surface via a hand-built `VADRMPRIMESurfaceDescriptor` with no CPU copy
(descriptor builder at `encoder.rs:87-119`). Mismatches stay on the GPU too: wrong
dimensions route through `vpp_scale_or_cpu` (`encoder.rs:1069`, backed by the
cached-context `VppScaler` in `vaapi/vpp_scaler.rs:24-37`), and non-NV12 formats route
through `vpp_convert_or_cpu` (`encoder.rs:1009`), so capture at 1080p BGRx and encode
at 720p NV12 remains a pure GPU pipeline. On macOS, ScreenCaptureKit wraps each
IOSurface-backed CVPixelBuffer as an `AppleGpuFrame`
(`rusty-capture/src/platform/apple/screen.rs:207-209`), and the VTB encoder retains the
CVPixelBuffer and hands it straight to `VTCompressionSession`
(`codec/vtb/encoder.rs:243-292`), letting VideoToolbox do BGRA-to-YUV and scaling on
the GPU. Two gaps on our side: the V4L2 encoder downloads GPU frames to CPU NV12, and
V4L2 camera capture is CPU MMAP with DMA-BUF export documented but dead code
(`rusty-capture/src/platform/linux/v4l2.rs:161`). Windows capture is a documentation
stub.

moq. macOS: AVFoundation and ScreenCaptureKit produce `Frame::Surface` (shared
extraction in `rs/moq-video/src/capture/surface.rs:12-19`), and the VideoToolbox
backend clones the retain and encodes it directly
(`encode/backend/videotoolbox.rs:162-166`). Windows: the Media Foundation source
reader delivers NV12 `Frame::Texture` on a shared D3D11 device, and the encoder MFT is
bound to the same device via a DXGI device manager, consuming the texture zero-copy
(`capture/mediafoundation.rs`, `encode/backend/mediafoundation.rs`). Linux is entirely
CPU: v4l2 capture is a CPU path, pipewire converts BGRx shared-memory buffers to CPU
I420, and the VAAPI encoder accepts only tightly packed CPU NV12, interleaving I420 to
NV12 on the CPU per frame (`encode/backend/vaapi.rs:60, 95-111`) with no GPU-surface
input path.

Verdict: complementary. On Linux ours is strictly ahead, since DMA-BUF capture into
VAAPI encode with GPU-side scale and color conversion has no counterpart. On Windows
theirs is strictly ahead, since we have only stubs
(`rusty-capture/src/platform/windows/mod.rs`). On macOS the two are equivalent in
mechanism and result.

### 2b. Decode to render: verdict ours

iroh-live. The full chain on Linux: the VAAPI decoder wraps each decoded surface as a
`VaapiGpuFrame` (`codec/vaapi/decoder.rs:103`) and exports a PRIME descriptor on
demand, calling `vaSyncSurface` before export because `vaExportSurfaceHandle` does not
synchronize (`decoder.rs:254-273`), with the export cached per frame
(`decoder.rs:85-113, 210-237`). `WgpuVideoRenderer` (`rusty-codecs/src/render.rs:45`)
selects a `RenderPath` per frame (`CpuRgba`, `CpuNv12`, `DmaBuf`, `MetalZeroCopy`,
`GpuDownload`; `render.rs:70-100`) and disables a zero-copy path after three
consecutive failures, falling back to CPU (`render.rs:267-359`). The DMA-BUF path is
`DmaBufImporter::import_nv12` (`render/dmabuf_import.rs:232`): raw Vulkan via ash,
importing the NV12 DMA-BUF as a multi-plane VkImage using
`VK_EXT_image_drm_format_modifier` plus external-memory-fd, then GPU-copying the planes
into R8 and RG8 textures wgpu can sample (`dmabuf_import.rs:1-13`). When the decoder's
tiling modifier is not Vulkan-importable (Intel Y_TILED on ANV, verified on Meteor Lake
hardware), `VppRetiler` (`dmabuf_import.rs:1057`) runs a VAAPI VPP identity blit to
re-tile the surface to a compatible modifier (Y_TILED to CCS) before import
(`retile` at `dmabuf_import.rs:1136`, invoked conditionally at `:244-254`); the VPP
pipeline parameter buffer is defined via raw libva FFI because cros-libva does not wrap
VPP (`VaProcPipelineParameterBuffer` at `dmabuf_import.rs:1026`).
`create_device_with_dmabuf_extensions` (`dmabuf_import.rs:1413`) lets applications
create the wgpu device with the required extensions. Two alternatives exist for
non-Vulkan contexts: `gles_dmabuf.rs` imports the same DMA-BUFs as EGLImages via
`EGL_EXT_image_dma_buf_import` into the GLES NV12 shader program
(`render/gles_dmabuf.rs:1-11`), and on macOS `metal_import.rs` aliases decoded
VideoToolbox CVPixelBuffers as Metal textures through `CVMetalTextureCache`
(`render/metal_import.rs:1-8`); the VTB decoder produces CVPixelBuffer-backed
`GpuFrame`s for exactly this purpose (`codec/vtb/decoder.rs:219-220`). On Android,
`AndroidHwDecoder` uses MediaCodec's Surface output through an ImageReader and wraps the
AHardwareBuffer as a `GpuFrameInner` with a `HardwareBuffer` native handle
(`format.rs:89-109`).

moq. There is no decode-to-render GPU path and no render API. Concretely: the
VideoToolbox decoder requests NV12 output and downloads every frame to packed CPU I420
(`decode/backend/videotoolbox.rs:12-13, 57-58`, `Sink { frames: Vec<I420> }`); the
Media Foundation decoder downloads the DXVA pool's NV12 textures to CPU I420; only
NVDEC keeps decoded frames on the GPU (`Frame::Cuda`), and that variant is
`pub(crate)`, produced only for the NVENC transcode path. There is no VAAPI decode
backend, so on any non-NVIDIA Linux machine their decode is software openh264 into CPU
I420 (`decode/backend/mod.rs`).

Verdict: ours, and it is not close. What they lack, itemized: any hardware decode on
non-NVIDIA Linux, any GPU-resident decode output on macOS or Windows, any public GPU
handle on decoded frames, any import machinery (Vulkan, EGL, or Metal), any renderer,
and the modifier-compatibility layer (VPP re-tiling) that makes DMA-BUF import work on
Intel. The import machinery, the renderer, and the re-tile layer are the bulk of
iroh-live's zero-copy engineering (roughly 3,500 lines across `render.rs`,
`dmabuf_import.rs`, `gles.rs`, `gles_dmabuf.rs`, and `metal_import.rs`) and have no
upstream counterpart to merge into; they can only be preserved by carrying them forward,
which is the subject of sections 4 and 5.

### 2c. Transcode, decode then scale then encode: verdict theirs

moq. A complete GPU transcode product. NVDEC outputs NV12 in CUDA device memory, and
each mapped cuvid surface is copied device-to-device into an owned `cuda::Frame` (one
D2D copy, zero host copies; `decode/backend/nvdec.rs:339-390`). Downscaling is free when
possible: `Config::resize` maps to cuvid's `ulTargetWidth`
(`nvdec.rs:299`), hardware scaling during decode; otherwise `Frame::resize`
(`decode/mod.rs:57`) runs vendored box-filter CUDA kernels (`frame/nv12_resize.cu`,
driver-JIT'd from embedded PTX so no CUDA toolkit is needed). The NVENC backend then
registers the CUDA buffer as an external resource
(`NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR`) via moq-nvenc's `register_generic_resource`
(`rs/moq-nvenc/src/safe/buffer.rs:208`, called at
`encode/backend/nvenc.rs:187-196`) and encodes in place. Both codecs share the device's
primary CUDA context so frames pass without a copy. On top of this, moq-transcode's
`Feed` decodes each source once and fans the frames out to every ladder rung as
`Arc<moq_video::decode::Frame>` (`rs/moq-transcode/src/feed.rs:35`), each rung doing its
own GPU resize and encode; NVDEC throughput scales with source count, not ladder depth.
A hardware test asserts the loop stays on the GPU (`nvdec.rs:666`).

iroh-live. No transcode product exists. The building blocks are present but wired to
other paths: `VppScaler` does GPU NV12 scaling, but on the capture-to-encode input path
(`vaapi/vpp_scaler.rs`, used from `vaapi/encoder.rs:1069`); CPU scaling is `pic-scale`
bilinear (`processing/scale.rs:34-66`); and `VideoDecoder::set_viewport` exists as a
decoder-side downscale hint (`traits.rs:60`). A VAAPI decode-to-VPP-scale-to-VAAPI-encode
loop would be the DMA-BUF analog of their CUDA loop, and all three stages exist in-tree,
but nobody has connected them, and there is no fanout layer.

Verdict: theirs. This is their crown exactly as decode-to-render is ours, and their
`Feed` fanout design (refcounted GPU frames, decode-once) is worth adopting regardless
of backend.

### 2d. Render itself: verdict ours

`WgpuVideoRenderer` renders any `FrameData` variant to an RGBA wgpu texture: NV12-to-RGBA
is a fragment-shader pass (`nv12_to_rgba.wgsl`), I420 and packed CPU frames upload
directly, per-frame path selection with failure counters protects against broken drivers
(`render.rs:267-359`), and `render_cached()` copies into a persistent texture for
external compositors such as Bevy, egui, and dioxus-native. A parallel GLES2 renderer
covers GL-only contexts (`render/gles.rs`). moq has no Rust rendering of any kind;
rendering is the application's problem, and on the Rust side the application receives CPU
I420. For a relay and transcode server that is a defensible scope decision; for a client
SDK it means every rendered frame is decoded on GPU at best, downloaded to CPU, and
re-uploaded by the app. Verdict: ours exists and theirs does not, and the "different
goals" framing holds only as long as moq-video is not positioned as a client playback
SDK.

---

## 3. Vendor-API coverage matrix

Cells name the pipeline direction or directions actually wired. "handle" means the type
exists in the frame model but no in-tree consumer imports it.

| Vendor API | iroh-live | moq |
|---|---|---|
| DMA-BUF | capture (PipeWire NV12, `pipewire.rs:721-766`); encode input (VAAPI import, `vaapi/encoder.rs:1268-1289`); decode output (VAAPI PRIME export, `vaapi/decoder.rs:254-273`); render (Vulkan and EGL import) | none |
| VAAPI surface | decode (cros-codecs plus export); encode (cros-codecs plus DMA-BUF import); VPP scale, csc, and re-tile (`vpp_scaler.rs`, `dmabuf_import.rs:1057`) | encode only, CPU NV12 upload (`encode/backend/vaapi.rs:95-111`); no decode backend |
| CUDA device ptr | none | decode output (NVDEC, `nvdec.rs:339-390`); encode input (NVENC register, `nvenc.rs:187-196`); GPU resize (PTX kernels) |
| CVPixelBuffer and IOSurface | capture (SCK, `apple/screen.rs:207-209`); encode input (VTB, `vtb/encoder.rs:243-292`); decode output (VTB decoder GpuFrame, `vtb/decoder.rs:219-220`); render (Metal import) | capture (AVF and SCK, `capture/surface.rs`); encode input (VT, `videotoolbox.rs:162-166`); decode downloads to I420 (`videotoolbox.rs:57-58`) |
| Metal texture | render (CVMetalTextureCache aliasing, `metal_import.rs`) | none |
| D3D11 texture | none (commented future variant, `format.rs:85-86`; platform stub) | capture (MF source reader); encode input (MFT via shared DXGI device); decode downloads to I420 |
| AHardwareBuffer | decode output (MediaCodec Surface mode); handle exposed for consumers, in-tree renderer falls back to NV12 download | none (no Android support) |
| EGLImage | render (`gles_dmabuf.rs`, `EGL_EXT_image_dma_buf_import`) | none |
| wgpu integration | renderer plus device-creation helper (`render.rs:45`, `dmabuf_import.rs:1413`) | none |
| Vulkan import | yes (ash, `VK_EXT_image_drm_format_modifier` plus external-memory-fd, `dmabuf_import.rs:1-13`) | none |

---

## 4. Render upstreaming decision

DECISION: the render stack goes in-tree to moq, as a new `moq-video-render`
crate that is a normal member of the moq workspace with its heavy graphics
dependencies behind non-default features, so moq's default and relay builds stay
light; both the wgpu and GLES backends land behind feature flags. The egui
integration follows as `moq-egui`. See `overview.md` and
`modules/render-video-render.md`. The analysis below is the record of the options
as they were weighed; where it disagrees, `overview.md` governs.

The maintainer intends to upstream the render stack, not merely tolerate it living
downstream. That reframes the decision. Every option below requires the same enabling
change on the moq side, namely a public frame vocabulary with native-handle accessors
plus decoders that export handles instead of downloading (sketched in
`comparison/moq-changes.md`, U1 and U2 in section 5). Without those, the renderer has nothing
to import regardless of where it lives, because `crate::frame::Frame` and its GPU
variants are `pub(crate)` and the only public exits are `into_i420()` and
`Encoder::encode` (`decode/mod.rs:94-101, encode/encoder.rs:249`). Given that shared
precondition, the three placements differ in one axis: who carries the graphics
dependency tree and the vendor-specific FFI.

### Option A: upstream the renderer into moq as an in-tree `moq-video-render` crate

What moq must expose: U1 and U2, plus it takes ownership of the render crate inside the
moq workspace.

Dependency weight. This pulls `wgpu`, `ash`, `glow`, `wgpu-hal`, and the platform
graphics stacks into moq's workspace and CI. moq-video's stated scope is deliberately
render-free: there is no renderer, no GPU import machinery, and rendering is the
application's problem (section 2d). Even as a separate workspace crate rather than a
feature of moq-video, it adds a heavy build and a large transitive dependency surface
that the maintainers have so far avoided. The maintainer's new willingness to upstream
render softens this objection but does not remove the cost.

Maintenance and ownership. moq maintainers would own the hardest, most
hardware-specific code in either codebase: the Vulkan external-memory import, the EGL
path, the Metal aliasing, and the Intel-specific Y_TILED-to-CCS VPP re-tile via raw
libva FFI (`dmabuf_import.rs:1026, 1057, 1136`). That is code whose correctness depends
on driver and GPU quirks the maintainers do not currently test.

Testability. It needs GPU CI runners keyed by vendor (Intel ANV for the re-tile path,
NVIDIA, AMD). moq's current CI is GPU-optional by design: NVENC and NVDEC dlopen their
libraries and probe with `driver_libs_present` so a GPU-less host still builds and falls
back. Making the render crate meaningful in CI reverses that posture.

### Option B: an out-of-tree `moq-video-render` crate we maintain over moq's public handles

What moq must expose: U1 and U2, and nothing else. moq-video stays render-free; the
public handle vocabulary is the entire contract.

Dependency weight. None on moq. The graphics dependency tree lives in our crate.

Maintenance and ownership. We own it and control its cadence, and the Intel VPP re-tile
knowledge stays where it is exercised on hardware we run. The renderer becomes a
published, reusable crate that consumes moq's public frame handles, so a third party
building a client can use it, which is also the proof that moq's public handle
vocabulary is sufficient rather than a private convenience. The risk is that the crate
tracks moq's public API and breaks when it changes, but the public handle vocabulary is
small and `#[non_exhaustive]`, so the blast radius is bounded.

Testability. We run our own GPU CI through the iroh-live device test runner that is
already on the roadmap, so no new burden lands on moq.

### Option C: keep the renderer in iroh-live, fully aligned to moq's frame model

What moq must expose: U1 and U2, identical to Option B. The difference is downstream: we
consume `moq_video::decode::Frame` (and its public handle accessor) directly as the
renderer's input and delete our parallel `FrameData` and `NativeFrameHandle`, so there
is exactly one frame vocabulary, moq's, rather than the two-model translation we carry
today.

Dependency weight. None on moq.

Maintenance and ownership. We own it, embedded in iroh-live, with no separate crate to
publish or version. The renderer stays iroh-live-private, so no third party benefits and
the render stack does not become part of the moq ecosystem, which is precisely the
outcome the maintainer's new priority argues against.

Testability. Unchanged from today, inside iroh-live.

### Recommendation

Recommend Option B. It is the placement that satisfies the maintainer's intent to
upstream the render stack while respecting moq-video's render-free scope: the renderer
becomes a reusable crate in the moq ecosystem, consuming moq's public frame handles,
without dragging `wgpu`, `ash`, and the vendor FFI into moq's workspace or CI. It keeps
the hardest and most hardware-specific code (the Vulkan import and the Intel Y_TILED
re-tile) where it is tested on real hardware, and it turns the renderer into the
existence proof that the public handle vocabulary of U1 is sufficient for outside
consumers. Option A is the right long-term home only once moq's CI grows per-vendor GPU
runners, and it should not be the opening move because it front-loads that burden onto
maintainers who cannot yet exercise the code. Option C is the minimal fallback if the
public-handle work stalls, since it still deletes our parallel frame model and aligns us
to moq's vocabulary, but it forgoes reusability and keeps the render stack out of the
upstream. All three depend on the same moq API additions; the concrete sketches for the
public handle enum, the `decode::Frame` accessor, and the per-decoder export live in
`comparison/moq-changes.md`, and Option B is the one that gets the most value from them.

---

## 5. Concrete upstream requirements

This list supersedes the earlier R1 through R7 enumeration. It is scoped to what Option
B needs, and each item names what it touches. The API drafts are in `comparison/moq-changes.md`.

U1. Public frame vocabulary and native-handle accessor. This is the keystone; every
other item depends on it. A public handle enum equivalent to our `NativeFrameHandle` and
`DmaBufInfo` (`format.rs:68-87, 508-528`), carrying the fd (`OwnedFd`), DRM format,
modifier, coded and display dimensions, and per-plane offset and pitch for DMA-BUF, plus
a retained CVPixelBuffer accessor for macOS and a D3D11 texture accessor for Windows.
Touches `rs/moq-video/src/frame.rs` (the private enum grows cfg-gated variants alongside
`Cuda`) and `rs/moq-video/src/decode/mod.rs` (a public accessor beside `into_i420()`,
returning the public handle enum and `None` for CPU frames). This is the on-demand-export
shape their enum lacks (section 1.3) and it respects the no-backend-types rule, because
the handle names a kernel or OS object, not a backend. See `comparison/moq-changes.md` for the
enum and accessor signatures.

U2. Per-decoder surface export (VAAPI, VideoToolbox, D3D). Three decoder-side changes so
that a decoded frame carries a native handle rather than a CPU download. A new VAAPI
decode backend in the candidate table producing a DMA-BUF handle, with the two hard-won
details from our reference implementation: `vaSyncSurface` before `vaExportSurfaceHandle`
(`vaapi/decoder.rs:254-273`) and export caching so repeated handle access does not
re-export (`decoder.rs:85-113`). The VideoToolbox decoder keeps the NV12 `CVPixelBuffer`
as a surface instead of downloading to I420
(`decode/backend/videotoolbox.rs:57-58`); the Surface type, its `Send + Sync`
justification, and retain-on-clone already exist on the capture side, so this is the
smallest change. The Media Foundation decoder retains the DXVA NV12 texture instead of
downloading it. Touches `rs/moq-video/src/decode/backend/{mod,vaapi,videotoolbox,
mediafoundation}.rs`. The VAAPI addition also requires `moq-vaapi` to grow surface export
(`vaExportSurfaceHandle` plus `VADRMPRIMESurfaceDescriptor`) and the decode half of
cros-codecs it currently omits, which is the largest single piece of enabling work.
Reference impls: our `vaapi/decoder.rs` and `vtb/decoder.rs:219-220`. Sketches in
`comparison/moq-changes.md`.

U3. Capture DMA-BUF and AHardwareBuffer delivery, with the matching encoder input. The
PipeWire capture source negotiates `SPA_DATA_DmaBuf` and produces the DMA-BUF handle
(our reference is `pipewire.rs:145-247, 721-766`, including the fd dup, the NV12-only
gate, and the CPU fallback), and an Android capture path produces an AHardwareBuffer
handle. This is only useful once the VAAPI encoder can consume `Frame::DmaBuf` by
importing the fd as a VAAPI surface (our descriptor builder at `vaapi/encoder.rs:87-119`
and the `push_frame` dispatch at `:1268-1289`), plus VPP scale and color conversion for
mismatched input (`vaapi/encoder.rs:1009, 1069`, `vpp_scaler.rs`), so the encoder change
is part of this requirement rather than a separate one. Touches
`rs/moq-video/src/capture/pipewire.rs`, a new Android capture module, and
`rs/moq-video/src/encode/backend/vaapi.rs`, and it requires VPP bindings in `moq-vaapi`
(raw FFI on our side today, since cros-libva does not wrap VPP). Sketches in
`comparison/moq-changes.md`.

U4. The render-crate home. A published, out-of-tree `moq-video-render` crate (Option B)
consuming only the public handle types from U1, into which our importers and renderer
port largely as-is: `render.rs` (path selection, shaders, failure fallback, and
`render_cached`), `dmabuf_import.rs` (Vulkan import, `VppRetiler`, and
`create_device_with_dmabuf_extensions`), `metal_import.rs`, `gles.rs`, and
`gles_dmabuf.rs`. This item touches no moq source; it is the downstream home that U1 and
U2 unlock, and it is what keeps the renderer out of moq-video's semver surface while
proving the public handles are sufficient for a third party.

Ordering: U1 is the keystone, and U2, U3, and U4 each depend on it and on nothing else
in the list, so they can proceed in parallel once the handle vocabulary is agreed. U2 and
U3 additionally gate on `moq-vaapi` growing export and VPP support, which is the largest
enabling piece and the one most likely to meet resistance, since the current VAAPI
encoder is marked not yet validated on hardware. A fallback worth pricing into the plan
is keeping our cros-codecs-based VAAPI pair as an out-of-tree backend, which requires the
`Backend` traits or a registration hook to become public, a larger stability ask than U1.

---

## 6. Verdict

Per axis: moq is ahead on NVIDIA, holding the only complete GPU transcode loop in either
codebase with GPU resize and a decode-once fanout, on Windows with D3D11 capture into MF
encode where we have stubs, and on encoder operability with congestion-driven rate
control and demand-driven capture wired end to end. iroh-live is ahead on Linux
generally with VAAPI decode and encode and DMA-BUF in both directions plus VPP scale,
convert, and re-tile, on decode to render as the only such path in either codebase across
three platforms and two graphics APIs, on rendering as the only renderer, on import
breadth across Vulkan, EGL, and Metal, and on Android as the only support at all. The
single genuinely shared path is macOS capture into VideoToolbox encode, where the
implementations are equivalent.

The two zero-copy investments are complementary, not competing. moq built the server-side
GPU story on NVIDIA plus the Windows client path; iroh-live built the Linux and Apple
client story from decoder to pixels on screen. Almost nothing is duplicated, which cuts
both ways: there is little merge conflict, and there is also no upstream code that
replaces ours, so adopting moq without the section 5 changes means deleting capability,
not deduplicating it.

Under the recommended Option B, the end-state for our `render/` code is concrete. The
importers and the renderer (`render.rs`, `dmabuf_import.rs` with its `VppRetiler`,
`metal_import.rs`, `gles.rs`, and `gles_dmabuf.rs`) move into a published, out-of-tree
`moq-video-render` crate that consumes moq's public frame handles. Our parallel
`FrameData::Gpu` and `NativeFrameHandle` collapse into moq's public handle vocabulary, so
the two-model translation we carry today disappears. moq-video stays render-free, the
Intel Y_TILED re-tile and the Vulkan import stay where they are tested on real hardware,
and the render stack becomes reusable across the moq ecosystem rather than iroh-live
private. The concrete moq API additions that make this possible are drafted in
`comparison/moq-changes.md`.
