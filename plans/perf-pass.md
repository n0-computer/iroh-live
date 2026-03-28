# Performance pass (2026-03-27)

Full-pass audit of hot paths across the workspace. Focused on per-frame
allocations, overflow bugs, and correctness issues in the encode/decode
pipelines.

## Fixed

### Scaler double-buffer (DR20)

`Scaler::scale_rgba` previously cloned its entire destination buffer every
frame: scale into `buf`, then `buf.clone()` to hand out, then cache `buf`
for reuse. At 1080p RGBA that is 8 MB allocated and copied per frame, 240
MB/s at 30 fps. Replaced with a double-buffer scheme where two buffers
alternate: one is handed to the caller, the other is ready for the next
call. Eliminates the clone entirely.

Files: `rusty-codecs/src/processing/scale.rs`

### Decoder pixel buffer reuse (H.264 + AV1)

Both software decoders allocated a fresh `Vec<u8>` for YUV-to-RGBA/BGRA
conversion every frame via `yuv420_to_rgba_from_slices` /
`yuv420_to_bgra_from_slices`. At 1080p that is ~2.6 MB per frame. Added
`yuv420_to_rgba_into` / `yuv420_to_bgra_into` variants that write into a
caller-provided buffer. Both decoders now retain a `pixel_buf` field that
is reused when dimensions are stable, and recycled after scaling when the
scaler produces a separate output.

Files: `rusty-codecs/src/processing/convert.rs`,
`rusty-codecs/src/codec/h264/decoder.rs`,
`rusty-codecs/src/codec/av1/decoder.rs`

### Audio decode tick overflow (NR2)

`audio_decode_loop` computed `TICK * tick_num as u32` where `tick_num` is
`u64`. The `as u32` truncation overflowed after ~12 hours (4.3 billion
ticks at 10 ms each), causing the pacing target to wrap to zero. The loop
then spins at CPU speed with zero sleep. Fixed by computing the target in
milliseconds directly: `Duration::from_millis(tick_num *
TICK.as_millis())`.

Files: `moq-media/src/pipeline/audio_decode.rs`

### Audio encode tick type (NR3)

`for tick in 0..` inferred `i32`, which overflows after ~248 days at 20 ms
intervals. Changed to `for tick in 0u64..` and switched the duration
calculation to use millisecond arithmetic to avoid `Duration * u32`
limitations.

Files: `moq-media/src/pipeline/audio_encode.rs`

### SharedVideoSource atomic ordering (ER28)

`start()` used `Relaxed` while `stop()` used `SeqCst`. Standardized on
`AcqRel`/`Acquire` for the subscriber count, and `Release`/`Acquire` for
the running flag. This ensures the source thread sees all state written
before `start()` and makes `running=true` visible after `unpark()`.

Files: `moq-media/src/publish.rs`

## Not fixed (needs design decisions or larger refactors)

### NV12 deep copy on clone

`Nv12Planes` uses `Vec<u8>` for `y_data` and `uv_data`, making
`FrameData::Nv12` clone a deep copy. Converting to `bytes::Bytes` would
make clone O(1) via refcount bump, matching the `Packed` and `I420`
variants. Deferred because it touches construction sites across eight
files in four crates (vaapi, v4l2, pipewire, android).

### Double YUV-RGB-YUV on NV12 encode (P16/PT1)

NV12 sources go NV12 -> RGBA -> YUV420 on the encode path. The NV12 data
could be split into I420 planes directly, saving ~5 ms/frame at 1080p.
Requires adding an NV12-to-I420 conversion or accepting NV12 input
directly in the encoder trait. Tracked as PT1 in `plans/perf.md`.

### Per-frame DMA-BUF re-export (P18/PG2)

VAAPI decoder exports DMA-BUF metadata every frame. Surface metadata
could be cached when the surface pool is stable. Hardware-specific, needs
VA-API testing.

### Per-frame Vulkan resources in DMA-BUF import (B6/PG1)

TextureView and BindGroup created per frame for imported NV12. Should
cache when fd/modifier match. Requires careful Vulkan lifetime management.

### Encoder-side YUV buffer reuse

`pixel_format_to_yuv420` allocates fresh `YuvData` (three `Vec<u8>`)
every frame on the encode path. The H.264 encoder's I420 fast path avoids
this when the frame is already I420, but RGBA frames still allocate.
Adding a reusable buffer to the encoder struct would eliminate this.
Deferred because the conversion happens inside the `yuv` crate's
`YuvPlanarImageMut::alloc()`, which owns its buffers.

### AEC mutex on real-time thread (AB3)

`AecProcessor` holds `Arc<Mutex<AudioProcessing>>` acquired on the cpal
input callback. Should use `try_lock` with passthrough fallback. Deferred
to phase 3 AEC work.
