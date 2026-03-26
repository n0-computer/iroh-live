# Performance improvements

Per-frame allocation elimination and hot-path optimization. See
[docs/architecture/performance.md](../docs/architecture/performance.md)
for the allocation budget and completed optimizations.

## Remaining items

### Allocations
- [ ] **PF6**: NAL Annex B / length-prefixed conversion allocates Vec — consider
  in-place or arena (`h264/annexb.rs`)
- [ ] **PF9**: AV1 encoder RGBA fallback for GPU frames — try NV12 direct
  access first (`av1/encoder.rs`)

### Lock contention
- [ ] **PL3**: V4L2 decoder format state Mutex in callback — requires
  restructuring V4L2 callback model; low priority (ARM-only)

### GPU / DMA-BUF
- [ ] **PG1**: Per-frame Vulkan TextureView + BindGroup for imported NV12 —
  cache when fd/modifier match (`render.rs`)
- [ ] **PG2**: Per-frame DMA-BUF export in VAAPI decoder — cache surface export
  metadata (`vaapi/decoder.rs`)
- [ ] **PG4**: VAAPI decoder three VA Displays — share pool/mapping/export
  displays (`vaapi/decoder.rs`)

### Transport
- [ ] **PT1**: Double YUV->RGB->YUV conversion on NV12 encode — add NV12-direct
  encoder input path

## Priority

PT1 (NV12 direct encode) eliminates the most bytes allocated per second.
GPU items (PG1, PG2, PG4) matter primarily for the VAAPI zero-copy pipeline.
