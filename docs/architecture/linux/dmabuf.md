# DMA-BUF Zero-Copy Rendering on Linux

| Field | Value |
|-------|-------|
| Status | research |
| Applies to | rusty-codecs |
| Platforms | Linux (Intel confirmed, AMD planned) |

On Linux, video frames decoded by VAAPI live as GPU surfaces backed by
DMA-BUF file descriptors. Rendering them without copying to the CPU
requires importing these DMA-BUFs into the display API (Vulkan via
wgpu). This is the zero-copy path: decoded pixels stay in GPU memory
from decode through display, avoiding the 2-4 ms per frame that a CPU
round-trip would add at 1080p.

The challenge is that GPU drivers encode surface memory layout in DRM
format modifiers (tiling, compression), and the decoder's output
modifier may not match what the display API can import. This page
covers how `DmaBufImporter` handles the import, including the VPP
retiler that bridges incompatible modifiers.

## Modifier incompatibility

VAAPI's H.264 decoder outputs NV12 surfaces with a tiling modifier chosen by the driver. On Intel Meteor Lake, this is `I915_FORMAT_MOD_Y_TILED` (0x100000000000002). Vulkan's ANV driver cannot import images with this modifier; it requires `I915_FORMAT_MOD_4_TILED_MTL_RC_CCS` (0x100000000000009) or linear. Without a fix, DMA-BUF import fails and the renderer falls back to CPU NV12 download and upload, adding 2-4 ms per frame.

## The solution

A VAAPI VPP (Video Post Processing) identity blit re-tiles the decoded surface from Y_TILED to a CCS-compatible modifier. The data stays on the GPU throughout: decode surface, VPP blit (same GPU, different tiling), export with compatible modifier, Vulkan import. The VPP retiler is implemented in `VppRetiler` within `rusty-codecs/src/render/dmabuf_import.rs`.

The retiler is conditional: at `DmaBufImporter` creation, the import path is probed with a small test surface. If the decoder's output modifier is directly importable, VPP is skipped entirely. The probe result is logged at `info!` level.

## Import pipeline

`DmaBufImporter` handles the Vulkan side. The per-frame import sequence:

1. Wait for the previous frame's fence (deferred from the prior import, overlapping GPU copy with CPU work).
2. Clean up the previous frame's NV12 import resources.
3. Import the DMA-BUF fd as a multi-plane NV12 `VkImage` (`VK_FORMAT_G8_B8R8_2PLANE_420_UNORM`) using `VK_EXT_image_drm_format_modifier`.
4. GPU-copy each plane to a separate single-plane `VkImage` (R8 for Y, RG8 for UV) using `vkCmdCopyImage`.
5. Submit the copy and record a fence (waited at step 1 of the next frame).
6. Return wgpu texture views wrapping the R8 and RG8 images.

The R8/RG8 copy targets, command buffer, and fence are cached and reused across frames. Only the imported NV12 `VkImage` is created per-frame because it wraps a new fd.

## wgpu-hal integration

The DMA-BUF import requires Vulkan extensions (`VK_EXT_image_drm_format_modifier`, `VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf`) that wgpu does not request by default. `create_device_with_dmabuf_extensions()` uses wgpu-hal's `open_with_callback` to inject these extensions at device creation time.

## Industry comparison

Every production Vulkan video renderer uses `VK_EXT_image_drm_format_modifier` for DMA-BUF import:

| Project | Import method | Tiling handling |
|---------|--------------|-----------------|
| mpv / libplacebo | Vulkan DRM modifier | Probes at init, excludes incompatible formats |
| FFmpeg | DRM PRIME descriptors | Downstream consumer decides |
| wlroots / Smithay | Vulkan DRM modifier | Compositor negotiates modifiers |
| GStreamer | EGL DMA-BUF | EGL handles tiling transparently |
| Chromium | EGL per-plane import | Ozone negotiates modifiers |

Our VPP retiler approach is unique. mpv excludes incompatible formats entirely (falling back to software decode). GStreamer and Chromium use the EGL path, which handles Y_TILED transparently. We cannot fall back to software decode because the VAAPI decoder is already selected and producing frames, so the VPP retiler is the only GPU-resident solution.

## Known issues

**AMD DMA-BUF size=0**: AMD's VA-API driver returns `size=0` in the `VADRMPRIMESurfaceDescriptor`. The importer handles this with `lseek(fd, 0, SEEK_END)` to determine the actual buffer size, matching the approach used by mpv and GStreamer.

**No explicit VAAPI-to-Vulkan sync**: `vaSyncSurface` is called before export, but there is no explicit Vulkan-side sync primitive for the imported DMA-BUF. This works on Intel because the i915 driver uses implicit synchronization. On AMD (RADV), explicit sync may be needed.

**Per-frame Vulkan resource allocation**: the imported NV12 `VkImage` and its memory allocation are created per-frame. Pooling these when the allocation size is stable would reduce kernel ioctl overhead.

## Improvements adopted from industry review

Several improvements were adopted after reviewing mpv/libplacebo, GStreamer, Chromium, and FFmpeg implementations:

- Format probing at init time (instead of reactive discovery through failures).
- Dedicated memory allocations for imported DMA-BUF images.
- Composed layer export (`VA_EXPORT_SURFACE_COMPOSED_LAYERS`) for the Vulkan path.
- Command buffer and fence reuse across frames.
- Deferred fence wait to overlap GPU copy with CPU work.
- Emulation detection via VA-API vendor string (warns on NVIDIA/VDPAU translation layers).
- `vaSyncSurface` before `vaExportSurfaceHandle` (prevents visual glitches from unsynchronized decode).
