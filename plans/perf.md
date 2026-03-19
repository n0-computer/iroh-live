# Performance improvements

Per-frame allocation elimination and hot-path optimization across the media
pipeline. Most items are independent and can be addressed incrementally.

## Status

### Allocations in encode/decode paths

- [x] **PF1**: Opus encoder `.to_vec()` on output — inherent copy for owned packet, minimal gain (encode buffer already reused)
- [ ] **PF2**: H.264 encoder I420 plane `.to_vec()` — borrow planes when layout matches (`h264/encoder.rs:221-228`)
- [x] **PF3**: H.264 decoder `RgbaImage::from_raw()` per frame — removed `RgbaImage` wrapper, store raw `Vec<u8>` directly
- [x] **PF4**: V4L2 decoder `copy_plane()` allocates when stride == width — returns `Cow<[u8]>` to avoid copy on fast path
- [x] **PF5**: Scaler `ImageStoreMut::alloc()` per call — caches destination buffer in `Scaler` struct, reuses when dimensions match
- [ ] **PF6**: NAL Annex B / length-prefixed conversion allocates Vec per packet — consider in-place conversion or arena (`h264/annexb.rs`)
- [x] **PF7**: SPS/PPS extraction `.to_vec()` per keyframe — returns borrowed `(&[u8], &[u8])` slices instead of owned Vecs
- [ ] **PF8**: `parse_annex_b()` returns `Vec<NalUnit>` — could return iterator (`h264/annexb.rs`)
- [ ] **PF9**: AV1 encoder frame copy through RGBA fallback for GPU frames — try NV12 direct access first (`av1/encoder.rs:225-227`)
- [ ] **PF10**: FFmpeg encoder `sws_frame.clone()` per frame — needs double-buffer restructuring (`ffmpeg/encoder.rs:422`)

### Lock contention

- [ ] **PL1**: PlayoutClock mutex locked on every frame — move `smoothed_jitter` to AtomicU64 (`playout.rs:206-259`)
- [ ] **PL2**: PlayoutBuffer 1 ms polling loop — replace with condvar or `recv_deadline()` (`playout.rs:385-404`)
- [ ] **PL3**: V4L2 decoder format state Mutex in callback hot path — consider RwLock or atomic state machine (`v4l2/decoder.rs:160-161`)

### GPU and DMA-BUF path

- [ ] **PG1**: Per-frame Vulkan TextureView + BindGroup for imported NV12 — cache when fd/modifier match (`render.rs:225-244`)
- [ ] **PG2**: Per-frame DMA-BUF export in VAAPI decoder — cache surface export metadata (`vaapi/decoder.rs:198`)
- [x] **PG3**: VAAPI encoder `query_image_formats` — verified: already called once per surface creation, not per-frame
- [ ] **PG4**: VAAPI decoder opens four VA Displays per instance — share pool/mapping/export displays (`vaapi/decoder.rs`)
- [x] **PG5**: DmaBufImporter Drop — verified: already has proper cleanup (pool, fence, images destroyed)

### Transport and pipeline

- [ ] **PT1**: Double YUV-RGB-YUV conversion on NV12 encode path — add NV12-direct encoder input path (`publish.rs`, `vaapi/encoder.rs`)
- [ ] **PT2**: `frame.raw.to_vec()` in video source passthrough — zero-copy passthrough when no scaling needed (`subscribe.rs:396`)
- [x] **PT3**: BufList `copy_to_bytes()` — verified: single-chunk case already zero-copy (`Bytes::split_to`)
- [ ] **PT4**: AudioBackendOpts cloned entirely for device switching — clone only modified fields (`audio_backend.rs:697-699`)

## Details

### Allocation budget

A 1080p@30fps H.264 stream touches these allocations per frame on the decode
path today:

1. `BufList::copy_to_bytes()` — 0 alloc (single-chunk is zero-copy)
2. `length_prefixed_to_annex_b()` — 1 alloc (Vec for converted NALs)
3. Decoder output — 1 alloc (YUV→RGBA pixel buffer)
4. Viewport scaling — 0 or 1 alloc (cached destination buffer reused)

Total: 2 allocations per frame at steady state (down from 2–3). At 30 fps
that is 60 allocs/sec. SPS/PPS extraction is now zero-alloc.

The encode path is worse because of format conversion:

1. `pixel_format_to_yuv420()` — 3 allocs (Y, U, V planes)
2. Encoder bitstream — 1 alloc (output buffer `.to_vec()`)
3. NAL conversion — 1 alloc (Annex B to length-prefixed or vice versa)
4. Frame scaling — 0 alloc (cached destination buffer reused)

Total: 4–5 allocations per frame. The YUV plane allocations (item 1) are the
largest — 1080p YUV420 is ~2.3 MB per frame.

### Approach

The remaining highest-impact changes are PT1 (NV12 direct encode path) and
PF2 (encoder plane borrowing). These eliminate the most bytes allocated per
second. Lock contention items (PL1, PL2) are lower priority but easy wins.

GPU path items (PG1, PG2, PG4) matter primarily for the VAAPI zero-copy
pipeline where per-frame overhead directly impacts the benefit of avoiding
CPU copies.

## Implementation history

- `278a49e` — eliminated per-frame allocs in Opus, VAAPI, render
- `ab3d63b` — eliminated per-frame allocs in Opus encoder, VAAPI encode path
- `3943135` — on-demand DMA-BUF export (was per-frame)
- *pending* — PF3: removed RgbaImage wrapper from H.264 decoder
- *pending* — PF4: V4L2 decoder copy_plane returns Cow (zero-copy when stride==width)
- *pending* — PF5: Scaler caches destination buffer across calls
- *pending* — PF7: extract_sps_pps returns borrowed slices
