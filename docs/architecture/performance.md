# Performance

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | moq-media, rusty-codecs |

## Per-frame allocation budget

The decode path for a 1080p H.264 stream at steady state allocates twice per frame:

1. `length_prefixed_to_annex_b()` allocates a `Vec` for NAL unit conversion.
2. The decoder output allocates a pixel buffer (YUV or RGBA depending on backend).

SPS/PPS extraction is zero-alloc (returns borrowed slices). Viewport scaling reuses a cached destination buffer. At 30 fps, the total is 60 allocations per second.

The encode path is more expensive because of format conversion:

1. `pixel_format_to_yuv420()` allocates three `Vec`s for Y, U, and V planes. At 1080p, the combined YUV420 data is roughly 2.3 MB per frame.
2. The encoder output buffer is a `Vec` (`.to_vec()` on the bitstream).
3. NAL format conversion (Annex B to length-prefixed or vice versa) allocates one `Vec`.
4. Frame scaling reuses the cached destination buffer (zero alloc).

Total: four to five allocations per frame on the encode path.

## Completed optimizations

These changes are already in the codebase:

- Per-frame allocations eliminated in Opus encoder, VAAPI encode path, and render upload.
- DMA-BUF export changed from per-frame to on-demand (export at render time, not decode time).
- `Scaler` caches its destination buffer and reuses it when dimensions match.
- `extract_sps_pps()` returns borrowed `(&[u8], &[u8])` slices instead of owned `Vec`s.
- V4L2 decoder `copy_plane()` returns `Cow<[u8]>` to avoid copying when stride equals width.
- H.264 decoder stores raw `Vec<u8>` directly instead of wrapping in `RgbaImage`.

## Highest-impact remaining work

**NV12 direct encode path**: the current pipeline converts NV12 capture frames to RGBA, then back to YUV420 for encoding. An NV12-direct input path for the encoder would eliminate the three YUV plane allocations and the double color conversion. This is the single largest allocation per frame (2.3 MB at 1080p).

**Encoder plane borrowing**: the H.264 encoder copies I420 plane data via `.to_vec()` even when the memory layout already matches the encoder's expected format. Borrowing the planes directly would eliminate this copy.

## Lock contention

**`PlayoutClock` mutex**: the playout clock's `Mutex` is acquired on every frame to update jitter tracking. Moving `smoothed_jitter` to an `AtomicU64` would eliminate this contention point.

**`PlayoutBuffer` polling**: the playout buffer uses a 1 ms polling loop (`recv_deadline`) to wait for the next frame's playout time. Replacing this with a condvar or `recv_deadline()` on the channel would reduce CPU wake-ups.

## GPU and DMA-BUF path

The DMA-BUF import pipeline has been optimized with resource pooling: R8/RG8 copy-target textures, the command buffer, and the fence are all reused across frames. Only the imported NV12 `VkImage` is created per-frame (it wraps a new DMA-BUF fd). The previous frame's import resources are kept alive via deferred fence wait, overlapping GPU copy with CPU work.

Remaining items: VAAPI decoder opens four VA Displays per instance (should share), and single-plane-per-image import (importing Y and UV as separate DMA-BUF images to eliminate the GPU copy entirely) is driver-dependent and needs per-vendor testing.
