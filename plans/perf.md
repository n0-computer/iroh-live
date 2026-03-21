# Performance improvements

Per-frame allocation elimination and hot-path optimization. See
[docs/architecture/performance.md](../docs/architecture/performance.md)
for the allocation budget and completed optimizations.

## Remaining items

### Allocations
- [x] **PF2**: H.264 encoder I420 planes `.to_vec()` — zero-copy via `YuvSlices` borrow when layout matches
- [ ] **PF6**: NAL Annex B / length-prefixed conversion allocates Vec — consider in-place or arena (`h264/annexb.rs`)
- [ ] **PF9**: AV1 encoder RGBA fallback for GPU frames — try NV12 direct access first (`av1/encoder.rs:225`)
- [ ] **PF10**: FFmpeg encoder `sws_frame.clone()` per frame — needs double-buffer restructuring (`ffmpeg/encoder.rs:422`)

### Lock contention
- [x] **PL1**: PlayoutClock mutex on every frame — non-issue: clock is only accessed from the decode thread (single-threaded), zero contention in practice (see CC2 in REVIEW.md)
- [ ] **PL3**: V4L2 decoder format state Mutex in callback — consider RwLock or atomic state machine (`v4l2/decoder.rs`)

### GPU / DMA-BUF
- [ ] **PG1**: Per-frame Vulkan TextureView + BindGroup for imported NV12 — cache when fd/modifier match (`render.rs:225`)
- [ ] **PG2**: Per-frame DMA-BUF export in VAAPI decoder — cache surface export metadata (`vaapi/decoder.rs:198`)
- [ ] **PG4**: VAAPI decoder three VA Displays — share pool/mapping/export displays (`vaapi/decoder.rs`)

### Transport
- [ ] **PT1**: Double YUV→RGB→YUV conversion on NV12 encode — add NV12-direct encoder input path
- [x] **PT4**: AudioBackendOpts cloned — non-issue: struct is 3 fields (two Option<DeviceId> + bool), clone is trivially cheap

## Priority

PT1 (NV12 direct encode) and PF2 (encoder plane borrowing) eliminate
the most bytes allocated per second. GPU items (PG1, PG2, PG4) matter
primarily for the VAAPI zero-copy pipeline.
