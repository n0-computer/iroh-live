# Linux decode→render pipeline review

Comparative analysis of our VA-API decode → DMA-BUF → wgpu render
pipeline against mpv (libplacebo), GStreamer (VA plugin), Chromium
(VaapiVideoDecoder), and FFmpeg (hwcontext). Reviewed on Intel Meteor
Lake with Mesa ANV.

## Reference implementations studied

- **mpv + libplacebo** — the gold standard for Linux video rendering.
  libplacebo's Vulkan backend is the most sophisticated open-source
  implementation of GPU resource management, DMA-BUF import, and
  NV12 rendering. mpv wraps it with VA-API hwdec, format probing,
  and multi-vendor fallback paths.

- **GStreamer VA plugin** — the most widely deployed pipeline framework.
  Lock-free surface pool recycling, caps-based format negotiation,
  and the clearest separation between decode and render stages.

- **Chromium** — the largest real-world deployment of VA-API decode on
  Linux. Generous pool sizing for deep compositor pipelines,
  SharedImage-mediated zero-copy to the GPU compositor, and
  extensive per-vendor workaround tables.

- **FFmpeg hwcontext** — the common foundation under mpv and many
  other players. Defines the surface pool model everyone else builds
  on, plus the DRM frame descriptor format used for cross-API interop.

## Findings worth adopting

### 1. Texture and resource reuse across frames

**The gap:** Our `DmaBufImporter::import_nv12` creates fresh Vulkan
resources every frame — `vkCreateImage` for the NV12 import,
`vkAllocateMemory`, `vkCreateImage` × 2 for Y/R8 and UV/RG8 copy
targets, `vkAllocateCommandBuffers`, and a fence. At 30 fps, that is
~150 Vulkan object create/destroy cycles per second, each hitting the
kernel via the DRM ioctl path.

**What the references do:**
- libplacebo caches textures in per-stage FBO arrays and reuses them
  when dimensions and format match. The slab allocator pools
  `VkDeviceMemory` across frames, avoiding per-frame `vkAllocateMemory`.
- mpv's EGL path creates textures once and rebinds them per frame via
  `EGLImageTargetTexture2DOES` (GLES) or destroys and recreates
  (desktop GL with immutable storage — unavoidable).
- GStreamer's `GstVaMemoryPool` recycles VA surfaces via lock-free
  `GstAtomicQueue`, returning freed surfaces to the pool instead of
  destroying them.
- Chromium's `MailboxVideoFrameConverter` caches `ClientSharedImage`
  objects and reuses them when format and dimensions match.

**Action:** Pool the R8 and RG8 copy-target textures, the command
buffer, and the fence in `DmaBufImporter`. On each frame:

1. If cached textures match the incoming width/height, reuse them.
2. Reuse the command buffer (reset, not reallocate).
3. Reuse the fence (reset, not recreate).
4. Only the imported NV12 `VkImage` must be created per-frame (it
   wraps a new DMA-BUF fd), but its `VkDeviceMemory` could be pooled
   if the allocation size is stable.

This reduces per-frame Vulkan overhead from ~6 create/destroy pairs to
~1 (the imported image only). libplacebo's approach of checking
`(width, height, format)` as the cache key is the right granularity.

### 2. AMD DMA-BUF size=0 workaround

**The gap:** AMD's VA-API driver returns `size=0` in the
`VADRMPRIMESurfaceDescriptor` object. Our `DmaBufImporter` passes
this size to Vulkan's `VkMemoryDedicatedAllocateInfo`, which will fail
on AMD hardware.

**What the references do:**
- mpv's libplacebo interop (`dmabuf_interop_pl.c:50-63`) detects
  `size == 0` and recovers with `lseek(fd, 0, SEEK_END)` to get the
  actual buffer size from the kernel.
- GStreamer's `_va_create_surface_and_export_to_dmabuf` also uses
  `lseek(fd, 0, SEEK_END)` to determine object sizes.
- FFmpeg's DRM PRIME import path handles this transparently.

**Action:** In `extract_dma_buf_info` and `DmaBufImporter::import_nv12`,
if the object size is 0, use `lseek(fd, 0, SEEK_END)` to determine
the actual size. This is a one-line fix that unblocks AMD hardware.

### 3. Composed vs. separate layer export

**The gap:** Our `export_prime()` call uses default flags. The
`VADRMPRIMESurfaceDescriptor` can return layers in two layouts:
- **Separate layers** (`VA_EXPORT_SURFACE_SEPARATE_LAYERS`): one layer
  per plane, each with its own DRM fourcc. Better for EGL, which
  imports planes individually.
- **Composed layers** (`VA_EXPORT_SURFACE_COMPOSED_LAYERS`): all planes
  in one layer with a single multi-plane DRM fourcc. Better for Vulkan,
  which imports a single `VK_FORMAT_G8_B8R8_2PLANE_420_UNORM` image.

**What the references do:**
- mpv uses `COMPOSED_LAYERS` for the libplacebo/Vulkan path and
  `SEPARATE_LAYERS` for the EGL path.
- Chromium uses `COMPOSED_LAYERS` for its NativePixmap path.
- GStreamer uses `SEPARATE_LAYERS` by default (oriented toward EGL).

**Action:** Use `VA_EXPORT_SURFACE_COMPOSED_LAYERS` explicitly in
`export_prime()` when the downstream consumer is our Vulkan import
path. This simplifies the descriptor handling (one layer, one fourcc)
and matches the Vulkan multi-plane image format we import as.

### 4. Format probing at init time

**The gap:** We attempt DMA-BUF import on every frame and fall back to
CPU download on failure, tracking failures with a counter
(`dmabuf_failures`). The first few frames always attempt import even
when the modifier is known to be incompatible, and the fallback path
(VPP retile) is discovered reactively.

**What the references do:**
- mpv probes formats at init time: creates 128×128 test surfaces in
  every supported VA profile, attempts the full export→import→map
  cycle, and builds a whitelist of working format+modifier combos.
  Probing errors are logged at DEBUG level to avoid noise.
- FFmpeg probes `vaDeriveImage` at init to determine whether zero-copy
  mapping works or copy-based readback is needed.
- Chromium validates VA-API capability at decoder creation, failing
  early if the profile or format is unsupported.

**Action:** At `DmaBufImporter` creation, probe the decode→import
path with a small test surface. Create a 64×64 VA surface, export it,
and attempt Vulkan import with the resulting modifier. Record whether
import succeeds directly or requires VPP retile. This lets us choose
the right path from the first frame instead of discovering it through
failure. Log the probe result at `info!` level — "DMA-BUF import:
direct (modifier 0x{:x})" or "DMA-BUF import: VPP retile required
(Y_TILED→CCS)".

### 5. Surface pool sizing

**The gap:** Our decoder adds 8 extra surfaces beyond the codec
minimum (`info.min_num_frames += extra` where `extra = 8`). This
number was chosen to match mpv's `hwdec_extra_frames` default of 6,
rounded up. But we don't account for actual downstream pipeline depth.

**What the references do:**
- mpv: `initial_pool_size = codec_required + 6`. The comment explains:
  "increase to hold more surfaces if the video output increases the
  number of reference surfaces for interpolation."
- Chromium: `codec_refs + 1 + renderer_depth`. For low-latency
  (WebRTC): renderer_depth = 5, giving 22 total for H.264. For
  standard playback: renderer_depth = 16, giving 33.
- FFmpeg: H.264 pool adds 16 surfaces (DPB maximum).
- GStreamer: `dpb_size + 4`.

Our real downstream depth is: 1 frame in the decode output channel +
1 in the playout buffer + 1 held by the renderer = 3. Adding 8 extra
surfaces is generous but not wasteful — each VA surface is a few
hundred KB of VRAM. The current sizing is fine.

**Action:** No change needed, but document the reasoning: 8 extra =
3 downstream + 5 headroom for burst decoding. This matches Chromium's
WebRTC configuration.

### 6. `vaSyncSurface` placement

**The gap:** We skip `vaSyncSurface` at decode output time (the
comment says "all consumer paths sync before accessing frame data"),
relying on downstream code to sync. This is correct but fragile — if
a new consumer path is added without syncing, it will read stale or
partially-decoded data.

**What the references do:**
- mpv calls `vaSyncSurface` at export time (in `mapper_map`), right
  before `vaExportSurfaceHandle`. This is the canonical placement.
- FFmpeg calls `vaSyncSurface` before read-access exports.
- GStreamer does not call `vaSyncSurface` explicitly, relying on
  `vaDeriveImage` to synchronize implicitly (which is documented as
  not guaranteed by the VA-API spec but works on all current drivers).

Our `extract_dma_buf_info` calls `surface.sync()` via `derive_nv12_planes`
for the CPU path, but the DMA-BUF export path (`export_prime()`) does
not sync. The VA-API spec says `vaExportSurfaceHandle` does not
synchronize — the caller must ensure the surface is idle.

**Action:** Call `surface.sync()` before `surface.export_prime()` in
`extract_dma_buf_info`. Without this, the exported DMA-BUF may
reference an in-progress decode, causing visual glitches on frames
where the GPU hasn't finished decoding before Vulkan imports the
buffer.

### 7. NV12 shader conversion vs. YCbCr sampler

**The gap:** We use separate R8/RG8 textures with a custom NV12→RGBA
fragment shader. This works but requires a GPU copy from the imported
multi-plane `VkImage` to separate single-plane `VkImage`s before the
shader can sample them.

**What the references do:**
- libplacebo: same approach — separate R8/RG8 textures, shader-based
  conversion. They explicitly avoid `VK_KHR_sampler_ycbcr_conversion`
  because it imposes usage constraints (no storage, limited filtering)
  and has poor driver support for DMA-BUF-imported images.
- GStreamer: uses `vkCmdBlitImage` for basic rendering, no NV12 shader.
- Chromium: relies on the compositor's GL or Vulkan backend for
  conversion, using per-plane `EGLImage` imports.

**Conclusion:** Our approach matches libplacebo (the best renderer).
No change needed. The GPU copy is an acceptable cost — a single
`vkCmdCopyImage` per plane per frame is ~0.1 ms on modern GPUs.

### 8. Command buffer management

**The gap:** We create and destroy a command buffer per frame for the
GPU copy operation.

**What the references do:**
- libplacebo maintains a single "current" command buffer that
  accumulates all GPU work for a frame. Command buffers are recycled
  from a pool, and the pool is garbage-collected by polling timeline
  semaphore values.
- GStreamer creates command buffers per-fence and recycles them via
  `GstVulkanTrashList`.
- Chromium amortizes command submission across the compositor's draw
  phase.

**Action:** Create the command buffer once in `DmaBufImporter::new`
with `RESET_COMMAND_BUFFER` flag on the pool. Reset and reuse on each
frame instead of allocating and freeing. This eliminates one
`vkAllocateCommandBuffers` / `vkFreeCommandBuffers` pair per frame.

### 9. Timeline semaphores instead of fences

**The gap:** We use `VkFence` for GPU synchronization in the DMA-BUF
import path — create a fence, submit the copy, wait on the fence, then
destroy the fence.

**What the references do:**
- libplacebo uses timeline semaphores (`VK_KHR_timeline_semaphore`) as
  the primary synchronization primitive. One timeline semaphore per
  queue, values monotonically incremented per submission. Polling is
  `vkWaitSemaphores` with a value, not `vkWaitForFences`.
- GNOME Remote Desktop also uses timeline semaphores for explicit sync.

Fences are per-submission objects that must be created, waited, reset,
and destroyed. Timeline semaphores are persistent objects that track
progress via monotonic counters — no per-frame creation overhead.

**Action:** For a future iteration, consider replacing the per-frame
fence with a persistent timeline semaphore on the import queue. The
current fence approach works correctly and the performance difference
is small (~1 ioctl saved per frame), so this is low priority.

### 10. Memory type selection

**The gap:** Our Vulkan memory allocation in `DmaBufImporter` uses
`VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT` as the requirement. We do not
query `VkMemoryDedicatedRequirements` for the imported image.

**What the references do:**
- libplacebo queries `VkMemoryDedicatedRequirements` via
  `vkGetImageMemoryRequirements2` and uses dedicated allocation when
  `prefersDedicatedAllocation` is true. For imported DMA-BUFs, it
  always uses dedicated allocation.
- FFmpeg uses dedicated allocations for all external memory imports.

Dedicated allocations tell the driver that a `VkDeviceMemory` backs
exactly one image, enabling driver-level optimizations (e.g., alias
detection, cache policy). For DMA-BUF imports, dedicated allocation
is effectively required by most drivers.

**Action:** Use `VkMemoryDedicatedAllocateInfo` when allocating memory
for imported DMA-BUF images. Chain it into `VkMemoryAllocateInfo` with
the target `VkImage` handle. This is a small change that improves
correctness on drivers that require or optimize for dedicated
allocations.

### 11. Emulation detection

**The gap:** We don't check whether VA-API is running through an
emulation layer (e.g., `nvidia-vaapi-driver` translating to NVDEC, or
`vdpau-driver-va-gl` translating to VDPAU).

**What the references do:**
- mpv's `va_guess_if_emulated()` checks the VA-API vendor string for
  "VDPAU backend" or "NVDEC driver" and skips the hwdec during
  auto-probing when emulation is detected.
- Chromium explicitly blocks VA-API on NVIDIA unless a feature flag is
  set, citing stability issues.

Emulation layers have different performance characteristics and
limitations. The `nvidia-vaapi-driver` does not support all formats,
and its DMA-BUF exports have different modifier behavior. On VDPAU
translation, decode works but export may not.

**Action:** At `VaapiDecoder::new`, query the VA-API vendor string via
`vaQueryVendorString`. Log it at `info!` level. If it contains "NVDEC"
or "VDPAU", log a warning that hardware decode is running through an
emulation layer and DMA-BUF zero-copy may not work. Do not block
operation — let the existing fallback paths handle failures.

### 12. GEM handle reference counting for DRM output

**Not applicable to us.** mpv's `drm_prime.c` reference-counts GEM
handles because `drmPrimeFDToHandle` reuses the same handle for the
same fd, and premature `GEM_CLOSE` would break other references. This
only matters for DRM overlay output, which we don't use (we go through
wgpu/Vulkan).

## Patterns we already handle well

### VPP retiler for modifier incompatibility

**Why it exists:** On Intel Meteor Lake, the VA-API H.264 decoder
outputs NV12 surfaces with `I915_FORMAT_MOD_Y_TILED` (0x2). Vulkan's
ANV driver cannot import images with this modifier — it requires
`I915_FORMAT_MOD_4_TILED_MTL_RC_CCS` (0x100000000000009) or linear.
Without a fix, the DMA-BUF import fails and falls back to CPU NV12
download+upload, adding ~2-4 ms per frame.

The VPP retiler (commit `906396f`) uses a VA-API Video Post Processing
identity blit to re-tile the decoded surface from Y_TILED to a
Vulkan-compatible modifier. This keeps the data on the GPU: decode
surface → VPP blit (same GPU, different tiling) → export with
compatible modifier → Vulkan import. Three FD leak bugs were fixed in
follow-up commits (`34d2ab6`, `3586f5d`, `13acb80`) as the VPP export
produces CCS aux planes whose FDs must be explicitly closed.

**Why others don't need it:** mpv probes at init time and excludes
incompatible formats — it never attempts to import a modifier that
won't work, falling back to software decode instead. GStreamer
negotiates formats via caps features and falls back to CPU copy.
Chromium uses Ozone's format negotiation to request compatible
modifiers from the compositor. None of them re-tile on the fly.

**Why we need it:** We don't control the decoder's output modifier. The
VA-API decoder picks its preferred tiling format (Y_TILED on Intel) and
we must accept it. We can't fall back to software decode because the
VAAPI decoder is already selected and producing frames. The VPP retiler
is the only GPU-resident solution that avoids a CPU readback.

**Is it still needed:** Yes, on current Intel hardware with Mesa ANV.
The underlying issue is a mismatch between libva's preferred decode
output tiling and Vulkan's importable tiling formats. This is specific
to the `i915` DRM driver's Y_TILED format on Gen12+ hardware. It may
be resolved in future Mesa versions if ANV adds Y_TILED import support,
or if the VA-API decoder starts producing CCS-tiled output directly.
Until then, the retiler is required for zero-copy on Intel.

### On-demand DMA-BUF export

Our pattern of exporting DMA-BUF fds at render time (not at decode
time) matches FFmpeg and GStreamer best practices. Transient FDs are
cheaper than persistent ones, and the export cost is dominated by the
`ioctl` call, not the fd lifecycle.

### Separate VA display for frame operations

We open a second VA display for frame mapping and DMA-BUF export,
separate from the decoder's display. This avoids driver serialization
when mapping frames while the decoder is active. mpv and GStreamer
don't do this — they use a single display with lock contention. Our
approach is better for throughput.

### CPU fallback path

Our automatic fallback from DMA-BUF import to CPU NV12 download +
upload matches mpv's fallback strategy. The `dmabuf_failures` counter
provides exponential backoff (currently falling back permanently after
a threshold), which is appropriate.

## Patterns that do not apply

### libplacebo slab allocator

libplacebo's slab allocator (64 pages per slab, bitset tracking,
exponential growth) is designed for a general-purpose Vulkan renderer
that allocates many heterogeneous resources. Our pipeline allocates a
small, fixed set of resources per frame (2-3 textures, 1 command
buffer). A simple pool of reusable resources is sufficient.

### GStreamer caps negotiation

GStreamer's caps-based format negotiation (VA > DMA-BUF > system memory,
with downstream queries for VideoMeta support) solves a problem we
don't have. Our pipeline has a fixed topology: VAAPI decoder →
DMA-BUF → Vulkan → wgpu. We don't need to negotiate between
alternative paths at the framework level.

### Chromium SharedImage / Mailbox

Chromium's `SharedImage` abstraction and `Mailbox` naming exist to
share GPU resources across process boundaries (GPU process ↔ renderer
process). We run in a single process with direct Vulkan access.

## Implementation checklist

Ranked by impact on real-world performance and hardware compatibility:

- [x] **AMD size=0 workaround** — trivial fix (`lseek` fallback), unblocks
  AMD GPUs entirely.
- [x] **`vaSyncSurface` before export** — trivial fix, prevents visual
  glitches from unsynchronized decode on frames where GPU timing
  is tight.
- [x] **Command buffer reuse** — trivial, eliminates per-frame alloc/free.
- [x] **Dedicated memory allocations** — small fix, improves correctness
  and potential driver optimizations for DMA-BUF imports.
- [x] **Composed layer export** — small fix, simplifies descriptor
  handling for the Vulkan import path.
- [x] **Emulation detection** — small effort, better diagnostics and
  user-facing warnings on NVIDIA/VDPAU.
- [x] **Texture and resource reuse** — medium effort, eliminates ~150
  Vulkan create/destroy cycles per second, measurable CPU and kernel
  overhead reduction.
- [x] **Format probing at init** — medium effort, eliminates first-frame
  failures and reactive VPP discovery.
- [ ] **Timeline semaphores** — medium effort, small perf win, lower
  priority until we need more sophisticated sync (e.g., multi-queue).
- [x] **Deferred fence wait** — instead of blocking on the fence
  immediately after queue submit, defer the wait to the start of the
  *next* frame's import. This overlaps GPU copy with CPU work, saving
  ~0.5 ms per frame. Requires keeping the previous frame's NV12 import
  resources alive until the fence signals.
- [ ] **Single-plane-per-image import** — import Y and UV planes as
  separate single-plane DMA-BUF images (R8 + RG8) instead of importing
  as multi-plane NV12 and GPU-copying. Eliminates the copy entirely.
  Driver-dependent: works on Intel ANV, inconsistent on AMD RADV.
  Needs per-driver testing before adoption.
- [ ] **Multi-aspect barrier split** — some older AMD RADV versions have
  bugs with multi-aspect (`PLANE_0 | PLANE_1`) barriers. If corruption
  appears on AMD, split into two per-plane barriers. Not needed until
  AMD testing.
- [ ] **VPP retile cleanup pattern** — the `cleanup()`-then-`forget()`
  pattern in `VppRetiler::retile` is correct but non-obvious. Low
  priority — the VA-API destroy calls are idempotent and the exported
  `OwnedFd` is moved out before cleanup runs.
