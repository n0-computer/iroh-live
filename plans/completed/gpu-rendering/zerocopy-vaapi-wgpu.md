# Zero-Copy VAAPI DMA-BUF to wgpu Rendering on Linux

## The Problem

VAAPI decoders on Intel allocate Y-tiled NV12 surfaces (modifier `0x100000000000002`,
`I915_FORMAT_MOD_Y_TILED`). To display these without a GPU-to-CPU-to-GPU round-trip, we need
to import the DMA-BUF directly into our Vulkan/wgpu rendering pipeline.

The core issue: Intel's Vulkan driver (ANV) does NOT support importing Y-tiled modifier for
single-plane formats like `R8` or `RG8`. The multi-planar `VK_FORMAT_G8_B8R8_2PLANE_420_UNORM`
(NV12) format with `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` is the only correct import path --
but it requires `VK_EXT_image_drm_format_modifier`, which wgpu does not enable by default.

Without this extension, `VK_IMAGE_TILING_LINEAR` or `VK_IMAGE_TILING_OPTIMAL` with
externally-imported memory is undefined behavior when the actual tiling doesn't match.

GBM cannot allocate linear NV12 buffers on Intel (`EINVAL`), and the Intel media-driver's decoder
ignores linear surface requests -- decoded data remains in tiled/macroblock order regardless
of surface attributes ([intel/media-driver#497](https://github.com/intel/media-driver/issues/497)).

## How Production Projects Handle This

### mpv / libplacebo

**Current state (mpv 0.41+, 2025)**: mpv now *prefers* Vulkan Video decode (`--hwdec=vulkan`)
over VAAPI when available, bypassing the DMA-BUF problem entirely. Vulkan Video decodes directly
into native VkImages with no format conversion needed.

**VAAPI+Vulkan path** (`--hwdec=vaapi --gpu-api=vulkan`): This path has historically been
problematic on Intel. mpv issue [#8702](https://github.com/mpv-player/mpv/issues/8702) documents
the exact error: `DRM modifier INTEL 0x2 not available for format r8`. The root cause was traced
to both Mesa ANV driver issues and a libplacebo bug
([haasn/libplacebo@3f197a3](https://github.com/haasn/libplacebo)).

**How libplacebo does it when it works**:

1. VAAPI surface exported via `vaExportSurfaceHandle()` to get DMA-BUF FDs + modifier + plane
   layouts
2. Queries modifier support via `vkGetPhysicalDeviceFormatProperties2` +
   `VkDrmFormatModifierPropertiesListEXT`
3. Creates VkImage with `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` +
   `VkImageDrmFormatModifierExplicitCreateInfoEXT`
4. Imports DMA-BUF FDs via `VkImportMemoryFdInfoKHR`
5. **Requires `VK_EXT_image_drm_format_modifier`** -- modern libplacebo (v7+) makes this a
   hard requirement

**Key constraint**: VkImage cannot be rebound to different memory in Vulkan (unlike GL), so
a new texture is created on each map call. Format negotiation via
`VK_EXT_image_drm_format_modifier` was not originally implemented because "no driver implements
it" (from [PR #6684](https://github.com/mpv-player/mpv/pull/6684)), but this is now standard.

**VAAPI+OpenGL/EGL path** (`--hwdec=vaapi --gpu-api=opengl`): This is the fallback that "just
works". EGL's `EGL_EXT_image_dma_buf_import` handles Y-tiled modifiers transparently -- the EGL
driver knows how to sample Y-tiled memory. This is why mpv's GL backend has always worked with
VAAPI while the Vulkan backend was broken for years.

**vaapi-copy path** (`--hwdec=vaapi-copy`): Copies decoded frames back to CPU RAM. Works
universally but uses ~4x more CPU (8% vs 39% in benchmarks). This is what mpv falls back to
when zero-copy import fails.

**Vulkan Video path** (`--hwdec=vulkan`): Preferred in mpv 0.41+. Uses `VK_KHR_video_decode_queue`
+ codec-specific extensions. Frames are native VkImages. Requires Mesa 24.1+ and supports
H.264, H.265, AV1 (older codecs still need VAAPI). On some AMD GPUs, Vulkan Video H.264 decode
is reported 5x slower than VAAPI.

Sources:
- [mpv issue #8702](https://github.com/mpv-player/mpv/issues/8702)
- [mpv PR #6684](https://github.com/mpv-player/mpv/pull/6684) -- VAAPI Vulkan interop
- [mpv issue #11739](https://github.com/mpv-player/mpv/issues/11739) -- Vulkan Video FAQ
- [mpv 0.41 release](https://www.phoronix.com/news/MPV-0.41-Released)
- [libplacebo issue #147](https://code.videolan.org/videolan/libplacebo/-/issues/147)

### GStreamer

**Production zero-copy path is EGL, not Vulkan**: `vah264dec ! glupload ! glimagesink` uses
`EGL_EXT_image_dma_buf_import` to import DMA-BUF into EGLImage into GL texture. This is the
battle-tested path.

**DMA-BUF modifier negotiation**: GStreamer has sophisticated modifier negotiation between
elements. The memory format natively supported by `intel-vaapi-driver` is Y-tiled NV12 for
YUV 420 surfaces. The gstreamer-vaapi dmabuf allocator originally only allocated non-tiled
surfaces, requiring the driver to tile before encoding / untile after decoding -- a known
inefficiency ([gstreamer-vaapi#49](https://gitlab.freedesktop.org/gstreamer/gstreamer-vaapi/-/issues/49)).

**Vulkan path exists but isn't wired up**: GStreamer's Vulkan DMA-BUF import code exists in
design docs using `VkImageDrmFormatModifierExplicitCreateInfoEXT`, but `vulkanupload` does NOT
accept `memory:DMABuf` input. The pipeline `vah264dec ! vulkanupload ! vulkansink` fails in
practice.

**VA-API VPP for tiling conversion**: GStreamer's `vapostproc` element uses VA-API Video Post
Processing for format/tiling conversion. However, `vapostproc` has had issues producing tiled
output on Intel Flex GPUs ([intel/media-driver#1820](https://github.com/intel/media-driver/issues/1820)),
fixed in driver commit `019772d`. Alternative: `msdkvpp` or `vaapipostproc` elements work as
fallbacks.

Sources:
- [DMABuf modifier negotiation in GStreamer](https://blogs.igalia.com/vjaquez/dmabuf-modifier-negotiation-in-gstreamer/)
- [gstreamer-vaapi#49](https://gitlab.freedesktop.org/gstreamer/gstreamer-vaapi/-/issues/49)
- [intel/media-driver#1820](https://github.com/intel/media-driver/issues/1820)

### FFmpeg

**VAAPI to Vulkan zero-copy** via `hwcontext_vulkan.c`:

1. `vaExportSurfaceHandle()` produces `AVDRMFrameDescriptor` (FDs, modifier, plane
   offsets/pitches)
2. `vulkan_map_from_drm_frame_desc()` creates VkImage with:
   - `VkImageDrmFormatModifierExplicitCreateInfoEXT` (modifier + plane layouts)
   - `VkExternalMemoryImageCreateInfo` with `DMA_BUF_EXT`
   - `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`
3. Per-plane `VkImportMemoryFdInfoKHR` to import each FD
4. Disjoint planes: `VK_IMAGE_CREATE_DISJOINT_BIT`, per-plane memory bind via
   `vkBindImageMemory2` + `VkBindImagePlaneMemoryInfo`
5. Sync: `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` to Vulkan timeline semaphores

**FFmpeg creates its own Vulkan device** with the extension enabled (not using wgpu). Fallback
without the extension: `VK_IMAGE_TILING_LINEAR` for modifier 0, `OPTIMAL` for others (technically
UB, works on some drivers).

**`hwmap` filter**: Maps frames between API contexts (e.g., VAAPI to Vulkan). Requires that
both ends agree on memory layout, format, stride, and modifiers. Falls back to minimal-copy
if mismatch.

**FFmpeg 8.0**: Added AV1 Vulkan encoder, VP9 Vulkan decoder, further improving the Vulkan
Video path as an alternative to VAAPI.

Sources:
- [FFmpeg hwcontext_vulkan.c](https://github.com/FFmpeg/FFmpeg/blob/master/libavutil/hwcontext_vulkan.c)
- [FFmpeg 8.0 release](https://news.tuxmachines.org/n/2025/08/22/FFmpeg_8_0_Huffman_Released_with_AV1_Vulkan_Encoder_VVC_VA_API_.shtml)

### Chromium / Chrome

**Uses EGL path on Linux**: `AcceleratedVideoDecodeLinuxGL` or `AcceleratedVideoDecodeLinuxZeroCopyGL`
flags enable VAAPI decode with EGL import.

VAAPI decoded frames are exported as DMA-BUF, imported via `EGL_EXT_image_dma_buf_import` into
EGLImage, then used as GL textures. Chromium's `VaapiVideoDecoder` is designed around Intel's
export format (both planes in same buffer object). AMD exports 2 different buffer objects (one
plane each), which required special handling.

**Vulkan path**: Chromium has experimental Vulkan flags but VAAPI+Vulkan is not confirmed working.
ANGLE's Vulkan backend may bridge the gap in the future.

Sources:
- [Chromium VA-API docs](https://chromium.googlesource.com/chromium/src/+/lkgr/docs/gpu/vaapi.md)

### SDL

SDL 3 added zero-copy VAAPI+EGL decode and display (October 2023). Uses EGLImage as a handle
between VAAPI and OpenGL memory spaces -- no data copied between CPU and GPU.

Sources:
- [SDL VAAPI+EGL commit](https://discourse.libsdl.org/t/sdl-added-support-for-0-copy-decode-and-display-using-vaapi-and-egl/46499)
- [Minimal VAAPI/EGL example](https://gist.github.com/kajott/d1b29c613be30893c855621edd1f212e)

### Wayland Compositors (wlroots, Smithay)

**wlroots** (`render/vulkan/texture.c: vulkan_texture_from_dmabuf()`):

1. Queries modifier support via `vkGetPhysicalDeviceImageFormatProperties2` +
   `VkPhysicalDeviceImageDrmFormatModifierInfoEXT`
2. Creates VkImage with `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` +
   `VkImageDrmFormatModifierExplicitCreateInfoEXT`
3. Disjoint planes: `VK_IMAGE_CREATE_DISJOINT_BIT`, per-plane memory bind
4. Imports each plane FD via `VkImportMemoryFdInfoKHR`

**wlroots requires `VK_EXT_image_drm_format_modifier` as a hard device requirement** -- devices
without it are rejected. Smithay uses the same approach.

Sources:
- [wlroots vulkan/texture.c](https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/master/render/vulkan/texture.c)

## Universal Conclusion

**Every production implementation that does zero-copy DMA-BUF to Vulkan uses
`VK_EXT_image_drm_format_modifier`.** There are no exceptions. This extension is the only
correct way to tell Vulkan about a DMA-BUF's tiling layout.

The EGL path (`EGL_EXT_image_dma_buf_import`) handles Y-tiled modifiers transparently because
the EGL/GL driver knows how to sample tiled memory internally. This is why EGL-based paths
(GStreamer, Chromium, SDL, mpv's GL backend) "just work" while Vulkan paths required the
modifier extension.

The trend is toward Vulkan Video decode (mpv 0.41, FFmpeg 8.0) which bypasses the DMA-BUF
problem entirely by decoding directly into native VkImages.

## Viable Approaches for wgpu+Vulkan

### Approach 1: `VK_EXT_image_drm_format_modifier` via wgpu callback (RECOMMENDED)

Use `VK_EXT_image_drm_format_modifier` -- the same extension every other project uses. wgpu
does not enable it by default, but `open_with_callback` (merged in wgpu-hal 27.0.4,
[PR #7829](https://github.com/gfx-rs/wgpu/pull/7829)) lets us inject it at device creation time
without forking wgpu.

**How it works**:

```rust
// 1. Create device with extra extension via callback
let open_device = unsafe {
    hal_adapter.open_with_callback(
        features,
        &wgpu::MemoryHints::default(),
        Some(Box::new(|mut args| {
            args.extensions.push(ash::ext::image_drm_format_modifier::NAME);
        })),
    )?
};

// 2. Export DMA-BUF from VAAPI decoded frame
// vaExportSurfaceHandle() -> DRM_PRIME_2 -> FDs + modifier + plane layouts

// 3. Create VkImage with DRM format modifier
let image_info = vk::ImageCreateInfo::default()
    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)  // NV12
    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
    .push_next(&mut modifier_info)     // VkImageDrmFormatModifierExplicitCreateInfoEXT
    .push_next(&mut ext_mem_info);     // VkExternalMemoryImageCreateInfo

// 4. Import DMA-BUF fd as VkDeviceMemory
// 5. GPU-copy planes to separate R8/RG8 textures for shader sampling
// 6. Wrap via texture_from_raw() -> create_texture_from_hal()
// 7. Feed into existing NV12->RGBA shader pipeline
```

**Pros**:
- No wgpu fork needed (callback API exists)
- Uses the exact same Vulkan extension stack as FFmpeg, wlroots, mpv, Smithay
- ~200 lines of import code
- Extension is purely additive, low risk

**Cons**:
- Per-frame VkImage+VkDeviceMemory allocation (can be mitigated with pooling)
- Need to handle queue family ownership transfer (`VK_QUEUE_FAMILY_EXTERNAL`)
- VAAPI surface must stay alive while Vulkan references the DMA-BUF

### Approach 2: VAAPI VPP blit to linear surface

Use VA-API Video Post Processing to blit from the decoder's Y-tiled output to a linear NV12
surface, then import the linear DMA-BUF (which doesn't need the modifier extension).

**How it works**: Create a VPP context, allocate a linear output surface, and use
`vaBeginPicture`/`vaRenderPicture`/`vaEndPicture` with `VAProcPipelineParameterBuffer` to blit.

**Pros**:
- GPU-accelerated blit (uses fixed-function hardware, very fast)
- Linear DMA-BUFs can be imported with just `VK_EXT_external_memory_dma_buf`
- Simpler import path (no modifier negotiation)

**Cons**:
- Intel's decoder ignores linear surface allocation requests
  ([media-driver#497](https://github.com/intel/media-driver/issues/497)) -- you MUST use VPP,
  not just request linear output
- Extra GPU copy (though hardware-accelerated, ~0.1ms per frame)
- `vapostproc` has bugs on some Intel hardware (Flex GPUs)
- Not truly zero-copy (one extra GPU blit)

### Approach 3: EGL interop (fallback to GL backend)

Import DMA-BUF via `EGL_EXT_image_dma_buf_import` into EGLImage into GL texture.

**Pros**:
- Battle-tested path (GStreamer, Chromium, SDL, Firefox all use it)
- EGL handles Y-tiled modifiers transparently
- Works on every Intel GPU without driver issues

**Cons**:
- Requires wgpu GL backend or mixing GL+Vulkan contexts
- wgpu GL backend has different performance characteristics
- Complex to integrate with a primarily Vulkan rendering pipeline
- Cannot use Vulkan-specific features (compute shaders, etc.)

### Approach 4: Vulkan Video decode (bypass VAAPI entirely)

Skip VAAPI. Use `VK_KHR_video_decode_queue` + codec-specific extensions for decode.
Frames are native VkImages, trivially usable in wgpu.

**Pros**:
- True zero-copy, no DMA-BUF dance, no format conversion
- Frames are native Vulkan textures
- This is where the ecosystem is headed (mpv 0.41 prefers this)

**Cons**:
- Requires Mesa 24.1+ for Intel
- Limited codec support (H.264, H.265, AV1 only)
- Would need to replace cros-codecs decoder entirely
- On some AMD hardware, 5x slower than VAAPI for H.264
- Less mature than VAAPI (driver bugs more likely)

**Reference**: [smelter](https://github.com/software-mansion/smelter) project does Vulkan Video
decode + wgpu integration successfully.

### Approach 5: Separate ash Vulkan device

Create a standalone `ash::Device` with all needed extensions. Import DMA-BUF there, do
NV12-to-RGBA conversion, export result as a new linear DMA-BUF, import into wgpu.

**Pros**: No wgpu changes needed

**Cons**: Two Vulkan devices, cross-device sync complexity, double import/export overhead.
Not recommended.

## Tradeoffs Summary

| Approach | Zero-copy | wgpu fork | Complexity | Maturity | Works on Intel |
|----------|-----------|-----------|------------|----------|----------------|
| 1. DRM modifier extension | Yes | No (callback) | Medium | High (FFmpeg/wlroots) | Yes* |
| 2. VPP blit to linear | GPU-to-GPU | No | Low-Medium | Medium | Yes |
| 3. EGL interop | Yes | No | High | Very High | Yes |
| 4. Vulkan Video | Yes | No | High | Low-Medium | Mesa 24.1+ |
| 5. Separate device | No (2 copies) | No | Very High | Low | Yes |

*Approach 1 requires ANV to support the specific modifier for the format. On modern Mesa (24+)
with `VK_EXT_image_drm_format_modifier`, this works for NV12 with Y-tiled modifier.

## Recommendation

**Short-term: Approach 1 (DRM modifier extension via `open_with_callback`)**. This is the
standard path used by every major project. No wgpu fork needed. The callback API already
exists in wgpu-hal 27.0.4.

**Fallback: Approach 2 (VPP blit to linear)** for hardware where the modifier is not supported
by the Vulkan driver. The VPP blit is GPU-accelerated and adds minimal latency.

**Long-term: Approach 4 (Vulkan Video)** when codec coverage and driver maturity improve.
This eliminates the VAAPI+DMA-BUF dance entirely.

## Current State

### What we have now

VAAPI H.264 decode via cros-codecs, with three rendering paths and automatic fallback:

1. **DMA-BUF zero-copy** (Approach 1): `VK_EXT_image_drm_format_modifier` via `open_with_callback`
2. **NV12 plane upload**: `vaDeriveImage` (GPU-to-CPU) + `write_texture` (CPU-to-GPU) + shader
3. **RGBA CPU fallback**: `vaDeriveImage` + CPU NV12-to-RGBA conversion + `write_texture`

### wgpu-hal API details (27.0.4)

```rust
// Adapter::open_with_callback -- inject extensions at device creation
pub unsafe fn open_with_callback(
    &self,
    features: wgpu_types::Features,
    memory_hints: &MemoryHints,
    callback: Option<Box<dyn FnOnce(CreateDeviceCallbackArgs)>>,
) -> Result<OpenDevice<Api>, DeviceError>

// CreateDeviceCallbackArgs fields:
pub struct CreateDeviceCallbackArgs<'arg, 'pnext, 'this> {
    pub extensions: &'arg mut Vec<&'static CStr>,
    pub device_features: &'arg mut PhysicalDeviceFeatures,
    pub queue_create_infos: &'arg mut Vec<vk::DeviceQueueCreateInfo<'pnext>>,
    pub create_info: &'arg mut vk::DeviceCreateInfo<'pnext>,
}

// Wrap raw VkImage into wgpu texture
pub unsafe fn texture_from_raw(
    &self,
    vk_image: vk::Image,
    desc: &TextureDescriptor,
    drop_callback: Option<DropCallback>,
) -> Texture

// Promote HAL texture to wgpu::Texture
pub unsafe fn create_texture_from_hal<A: HalApi>(
    &self,
    hal_texture: A::Texture,
    desc: &TextureDescriptor,
) -> Texture
```

### DMA-BUF import code path (wlroots/FFmpeg pattern)

```rust
// 1. Create VkImage with DRM format modifier
let plane_layouts = [vk::SubresourceLayout {
    offset: plane_offset,
    size: 0,
    row_pitch: plane_pitch,
    array_pitch: 0,
    depth_pitch: 0,
}];
let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
    .drm_format_modifier(modifier)
    .plane_layouts(&plane_layouts);
let mut ext_mem_info = vk::ExternalMemoryImageCreateInfo::default()
    .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
let image_info = vk::ImageCreateInfo::default()
    .image_type(vk::ImageType::TYPE_2D)
    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
    .extent(vk::Extent3D { width, height, depth: 1 })
    .mip_levels(1)
    .array_layers(1)
    .samples(vk::SampleCountFlags::TYPE_1)
    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
    .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
    .sharing_mode(vk::SharingMode::EXCLUSIVE)
    .push_next(&mut modifier_info)
    .push_next(&mut ext_mem_info);

// 2. Import DMA-BUF fd as VkDeviceMemory
let mut import_info = vk::ImportMemoryFdInfoKHR::default()
    .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
    .fd(dup'd_fd);
let alloc_info = vk::MemoryAllocateInfo::default()
    .allocation_size(mem_reqs.size)
    .memory_type_index(memory_type_index)
    .push_next(&mut import_info);

// 3. Bind memory, create image views, copy planes to R8/RG8 textures
// 4. Wrap as wgpu textures via texture_from_raw + create_texture_from_hal
```

### Known issues with DMA-BUF zero-copy path

1. **Per-frame Vulkan allocation**: `import_nv12()` creates 3 VkImage + 3 VkDeviceMemory per
   frame. Fix: pool Y/UV plane images, reuse across frames.

2. **VkImage size mismatch**: Y plane created with coded dimensions (1920x1088) but wrapped
   as wgpu texture with display dimensions (1920x1080). Fix: use coded dimensions in wgpu
   descriptor, crop via shader UV coords.

3. **No explicit VAAPI-to-Vulkan sync**: Works on Intel (implicit sync) but may break on
   explicit-sync-only drivers. Need `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` or equivalent.

4. **Queue family ownership**: wgpu issue [#2948](https://github.com/gfx-rs/wgpu/issues/2948).
   Imported DMA-BUFs need `VK_QUEUE_FAMILY_EXTERNAL` barrier. Workaround: `CONCURRENT` sharing
   mode with `VK_QUEUE_FAMILY_IGNORED`.

5. **Frame lifetime**: VAAPI surface must stay alive while Vulkan references the DMA-BUF.
   The `drop_callback` must hold a reference to the VAAPI frame.

## References

- [FFmpeg hwcontext_vulkan.c](https://github.com/FFmpeg/FFmpeg/blob/master/libavutil/hwcontext_vulkan.c) -- `vulkan_map_from_drm_frame_desc()`
- [wlroots vulkan/texture.c](https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/master/render/vulkan/texture.c) -- `vulkan_texture_from_dmabuf()`
- [libplacebo vulkan/gpu_tex.c](https://github.com/haasn/libplacebo) -- DMA-BUF import
- [smelter (vk-video)](https://github.com/software-mansion/smelter) -- Vulkan Video decode + wgpu
- [wgpu-video](https://github.com/karmakarmeghdip/wgpu-video) -- Vulkan DMA-BUF import (older wgpu API)
- [Minimal VAAPI/EGL interop example](https://gist.github.com/kajott/d1b29c613be30893c855621edd1f212e)
- [DMABuf modifier negotiation in GStreamer](https://blogs.igalia.com/vjaquez/dmabuf-modifier-negotiation-in-gstreamer/)
- [mpv issue #8702](https://github.com/mpv-player/mpv/issues/8702) -- Y-tiled modifier problem
- [mpv PR #6684](https://github.com/mpv-player/mpv/pull/6684) -- VAAPI Vulkan interop
- [mpv issue #11739](https://github.com/mpv-player/mpv/issues/11739) -- Vulkan Video FAQ
- [intel/media-driver#497](https://github.com/intel/media-driver/issues/497) -- decoder ignores linear tiling
- [intel/media-driver#1820](https://github.com/intel/media-driver/issues/1820) -- vapostproc tiled output bug
- [wgpu issue #2320](https://github.com/gfx-rs/wgpu/issues/2320) -- texture memory import API
- [wgpu PR #7829](https://github.com/gfx-rs/wgpu/pull/7829) -- `open_with_callback`
- [wgpu issue #2948](https://github.com/gfx-rs/wgpu/issues/2948) -- queue family ownership barriers
- [Chromium VA-API docs](https://chromium.googlesource.com/chromium/src/+/lkgr/docs/gpu/vaapi.md)
