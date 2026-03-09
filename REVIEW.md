# GPU Rendering Review — zerocopy-vaapi-wgpu

Review of the `good-bye-ffmpeg` branch diff. Covers VAAPI decoder, DMA-BUF import,
wgpu renderer, and format types.

## Bugs Fixed This Session

### 1. DMA-BUF resource leak on error paths (`dmabuf_import.rs`)

`import_nv12()` created Vulkan resources (VkImage, VkDeviceMemory, dup'd fd) without
cleanup on error paths. If `create_nv12_image` failed, the dup'd fd leaked. If later
steps failed, partially-created VkImages and memory leaked. This caused a death spiral:
leaked resources caused memory exhaustion, which caused more failures, which leaked more.

**Fixed**: Each step now cleans up all previously-created resources on error.

### 2. DMA-BUF import log spam (`render.rs`)

`render()` logged a warn-level message on every single frame when DMA-BUF import
failed. At 30fps this created massive log spam.

**Fixed**: After 3 consecutive failures, the DMA-BUF importer is disabled permanently
for the session and a single warn-level message is logged. Individual failures are debug.

### 3. `nv12_to_rgba_data` dead_code warning (`convert.rs`)

`nv12_to_rgba_data` is only used by the `vaapi` feature but was unconditionally compiled.
**Fixed**: Added `cfg_attr(not(vaapi), allow(dead_code))`.

## "No NAL found" Errors

This comes from cros-codecs' H.264 parser when it receives a packet without a valid NAL
start code. Happens at stream start (before first keyframe) or after packet loss. The
recovery mechanism in `subscribe.rs` (`waiting_for_keyframe = true`) handles this correctly.
Not a bug — cros-codecs is just stricter than openh264 about malformed input.

The openh264 software decoder silently ignores the same packets, so there's no visible
difference in behavior, only in log noise.

## Open Issues (Not Fixed)

### A. Per-frame Vulkan resource allocation (dmabuf_import.rs)

`import_nv12()` creates 3 VkImage + 3 VkDeviceMemory allocations per frame (NV12 source,
Y plane, UV plane). wgpu defers cleanup until device poll, so at 30fps resources can pile up
faster than they're freed. This is the root cause of the "device memory allocation failed"
errors even after the leak fix.

**Fix**: Pool Y and UV plane images across frames (same resolution = same image). Only the
NV12 source image needs to be per-frame (different DMA-BUF fd). This would reduce allocations
from 6/frame to 2/frame.

### B. VkImage size mismatch: coded vs display dimensions (dmabuf_import.rs:146-153)

The Y plane VkImage is created with `coded_width x coded_height` (e.g., 1920x1088) but
wrapped as a wgpu texture with `display_width x display_height` (e.g., 1920x1080). When
coded > display (which is common — H.264 coded height is often rounded up to 16), the wgpu
TextureDescriptor size doesn't match the actual VkImage extent. This could cause Vulkan
validation errors or incorrect sampling.

**Fix**: Either create plane images at display dimensions and adjust the copy region, or
wrap the wgpu texture with coded dimensions and let the shader crop via UV coords.

### C. NV12→RGBA shader: no limited-range expansion (nv12_to_rgba.wgsl)

The shader treats Y values as full-range (0.0-1.0) but VAAPI output is BT.601 limited range
(Y: 16-235, UV: 16-240). The Y value should be scaled: `y = (y_raw - 16.0/255.0) * (255.0/219.0)`.
Currently produces slightly washed-out blacks and dim whites.

**Fix**: Add limited→full range expansion to the shader. For reference, the CPU-side
`nv12_to_rgba_data` (via yuvutils-rs) does handle this correctly with `YuvRange::Limited`.

### D. No sync between VAAPI decode and Vulkan DMA-BUF read

`drain_events()` calls `handle.sync()` to wait for VAAPI decode completion, but there's no
explicit synchronization primitive (fence/semaphore) between VAAPI writing and Vulkan reading
the DMA-BUF. The `VK_QUEUE_FAMILY_EXTERNAL` barrier in `copy_planes` provides an implicit
acquire, but the Vulkan spec requires either a fence signaled by the exporting API or
`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`. This works in practice on Intel (implicit sync) but may
fail on drivers with explicit sync only.

### E. Queue family ownership (wgpu #2948)

The `copy_planes` barrier uses `VK_QUEUE_FAMILY_EXTERNAL` → wgpu queue family for ownership
transfer. This is correct per the Vulkan spec, but wgpu has no API to coordinate queue family
ownership (issue #2948). The current approach works because we submit the barrier on the same
queue wgpu uses, but this could break if wgpu changes its internal queue management.

### F. Per-frame bind group creation (render.rs:187-204, 306-323)

Both `render_imported_nv12` and `render_nv12` create a new wgpu bind group every frame.
These could be cached when the texture dimensions haven't changed, avoiding per-frame
descriptor set allocation.

### G. `unsafe impl Send/Sync` for VaapiGpuFrame (vaapi/decoder.rs:86-87)

`VaapiGpuFrame` wraps `Arc<PooledVideoFrame<GenericDmaVideoFrame>>` which contains `Vec<File>`
(file descriptors). The `Send` impl is sound because the frame is immutable after creation and
file descriptors are OS-global. The `Sync` impl is sound because all access is read-only
(via `Arc`). Still worth a closer look if the cros-codecs frame pool ever mutates frames
in-place.

### H. Display::open() called per-frame for derive_nv12_planes (vaapi/decoder.rs:95)

`VaapiGpuFrame::derive_nv12_planes()` opens a new VAAPI Display connection on every call.
This is the fallback path (when DMA-BUF import fails), so it's called 30 times/second.
Opening a Display involves opening `/dev/dri/renderD128` and initializing the VA driver.

**Fix**: Cache the Display in VaapiGpuFrame or use a thread-local.
