# Zero-Copy Capture → Encode Pipeline

## Status: Phase 1 Done, Phase 2 Done

## Background

PipeWire can deliver frames as DMA-BUF file descriptors instead of shared
memory, keeping pixel data on the GPU. The VAAPI encoder can import DMA-BUF
NV12 surfaces directly — no CPU round-trip. The goal is to connect these two
capabilities into an end-to-end zero-copy path.

## Phase 1: DMA-BUF Negotiation (Done)

**Problem**: PipeWire defaulted to shared memory because our stream never
declared DMA-BUF support.

**Fix**: Added `SPA_PARAM_BUFFERS_dataType` pod requesting
`DmaBuf | MemFd | MemPtr` in `build_buffer_params_pod()`. Both camera and
screen capture now receive DMA-BUF frames (`is_gpu=true`).

**Also fixed**:
- `GpuPixelFormat::Bgrx` added for compositor screen capture (BGRx format)
- VAAPI encoder checks DRM fourcc before importing (was debug_assert only)
- All DMA-BUF formats preserved as GPU frames (not just NV12)

**Current state after Phase 1**:

| Source → Encoder | Buffer type | GPU frame? | Encoder import |
|------------------|-------------|-----------|----------------|
| Camera (USB YUYV) → VAAPI | DMA-BUF | Yes | CPU mmap → RGBA → NV12 upload |
| Screen (Mutter BGRx) → VAAPI | DMA-BUF | Yes | CPU mmap → RGBA → NV12 upload |
| Camera (NV12, e.g. Pi) → VAAPI | DMA-BUF | Yes | **Zero-copy VA surface import** |

The NV12 path is already zero-copy. YUYV and BGRx DMA-BUFs still go through
CPU because the VAAPI encoder only imports NV12 directly.

## Phase 2: VAAPI VPP Color-Space Conversion (This Plan)

**Goal**: Use VAAPI Video Processing Pipeline (VPP) to convert YUYV/BGRx
DMA-BUF → NV12 VA surface on the GPU, then feed that to the encoder.
Eliminates all CPU pixel touching.

### Architecture

```
PipeWire DMA-BUF (BGRx/YUYV)
    │
    ▼
VA surface import (BGRx/YUYV format)
    │
    ▼
VPP vaProcPipelineParameterBuffer (color convert + optional scale)
    │
    ▼
NV12 VA surface (encoder input)
    │
    ▼
VAAPI H.264/AV1 encode
```

### Implementation Steps

#### Step 1: VPP Color Converter in VAAPI Encoder

File: `rusty-codecs/src/codec/vaapi/encoder.rs`

1. Add a `VppColorConverter` struct alongside the existing encoder:
   - Holds a VA config + context for VPP
   - Input format: BGRx (VA_RT_FORMAT_RGB32) or YUYV (VA_FOURCC_YUY2)
   - Output format: NV12 (VA_RT_FORMAT_YUV420)
   - Reuses the same `VADisplay` as the encoder

2. Import non-NV12 DMA-BUF as VA surface:
   - Use `vaCreateSurfaces` with `VASurfaceAttribExternalBuffers` for the
     input DMA-BUF FD, specifying the source DRM fourcc
   - For BGRx: `VA_RT_FORMAT_RGB32`, fourcc `VA_FOURCC_BGRX`
   - For YUYV: `VA_RT_FORMAT_YUV422`, fourcc `VA_FOURCC_YUY2`

3. Run VPP pipeline:
   - `vaBeginPicture(vpp_context, output_nv12_surface)`
   - `vaRenderPicture` with `VAProcPipelineParameterBuffer` pointing to input surface
   - `vaEndPicture` + `vaSyncSurface`
   - Output NV12 surface is ready for the encoder

4. Feed NV12 surface to existing encode path (same as current zero-copy NV12 DMA-BUF import)

#### Step 2: Integrate into push_frame

In `push_frame()`, change the non-NV12 DMA-BUF branch:

```rust
// Before (current):
// Non-NV12 DMA-BUF → CPU mmap → RGBA → NV12 upload
self.build_nv12_input(frame)?

// After:
// Non-NV12 DMA-BUF → VPP import → GPU color convert → NV12 surface
self.vpp_convert_dmabuf(&info)?
```

#### Step 3: Optional Scaling in VPP

VPP can also scale in the same pass. If the source dimensions differ from
the encoder target, set `output_region` in `VAProcPipelineParameterBuffer`
to the target size. This replaces the CPU-side `pic-scale` step for
DMA-BUF frames.

### Existing Code to Reuse

- `VppRetiler` in `rusty-codecs/src/render/dmabuf_import.rs` already wraps
  raw libva VPP FFI (`VaProcPipelineParameterBuffer`). It does identity blit
  for modifier retiling. The color-convert VPP uses the same API but with
  different input/output formats.
- `cros-libva` types for VA display, config, context, surface management.

### Risk: VA Format Support

Not all VAAPI drivers support all input formats for VPP. Check at init:
- `vaQuerySurfaceAttributes` for VPP input capabilities
- Fall back to CPU path if BGRx/YUYV input isn't supported

### Testing

1. Unit test: Create a test BGRx buffer, import as VA surface, VPP to NV12,
   verify NV12 output is valid
2. Integration: Run viewer with screen capture, verify log shows
   "VAAPI encode: zero-copy VPP convert (BGRx → NV12)" instead of CPU upload
3. Perf: Compare frame times before/after with `tracing` spans

### Result After Phase 2

| Source → Encoder | Path |
|------------------|------|
| Camera (USB YUYV) → VAAPI | DMA-BUF → VPP YUYV→NV12 → encode (**zero-copy**) |
| Screen (Mutter BGRx) → VAAPI | DMA-BUF → VPP BGRx→NV12 → encode (**zero-copy**) |
| Camera (NV12) → VAAPI | DMA-BUF → direct import → encode (**zero-copy**, unchanged) |
| Any source → software encoder | CPU path (unchanged) |

### Phase 2 Implementation Notes

**File**: `rusty-codecs/src/codec/vaapi/encoder.rs`

**`VppColorConverter`**: Opens its own `VADisplay` (cros-libva's `Display::handle()`
is `pub(crate)`, so can't reuse the encoder's). Creates a `VAProfileNone +
VAEntrypointVideoProc` config. The `convert()` method:

1. Maps input DRM fourcc → VA RT format (`XR24`/`AR24` → `VA_RT_FORMAT_RGB32`,
   `YUYV` → `VA_RT_FORMAT_YUV422`)
2. Imports input DMA-BUF as VA surface via `DRM_PRIME_2` with the correct RT format
3. Creates NV12 output surface at encoder target dimensions (handles scaling)
4. Runs VPP pipeline (vaBeginPicture/vaRenderPicture/vaEndPicture/vaSyncSurface)
5. Exports output as `DRM_PRIME_2` → new `DmaBufInfo` (NV12)
6. Existing encoder zero-copy DMA-BUF import path handles the rest

**Integration**: `vpp_convert_or_cpu()` lazy-inits the converter on first non-NV12
DMA-BUF frame. Falls back to CPU path if VPP init or conversion fails (disables
VPP on failure to avoid retrying every frame).

**`VaProcPipelineParameterBuffer`**: Manually defined from `va/va_vpp.h` (not in
cros-libva bindings). Same struct used by `VppRetiler` in `dmabuf_import.rs`.

### Phase 3 TODO: Packed SPS with VUI `max_dec_frame_buffering`

**Problem**: cros-codecs sets `max_num_ref_frames=1` in the SPS but does NOT
set VUI `bitstream_restriction_flag` or `max_dec_frame_buffering`. Decoders
fall back to the H.264 level-derived DPB size (e.g. Level 4.0 at 640×360 →
DPB=16 → 533ms of buffering before first output).

**Current workaround**: `min_dpb_h264_level()` picks the lowest level where
DPB≥2 (e.g. Level 1.2 for 640×360). Same approach used by wl-screenrec
(hardcodes Level 3.0).

**Proper fix (what FFmpeg and GStreamer do)**: Generate packed SPS headers
with VUI fields and submit as `VAEncPackedHeaderSequence`:

- FFmpeg (`hw_base_encode_h264.c`): Sets `sps->vui.bitstream_restriction_flag=1`,
  `max_dec_frame_buffering = max_b_depth + 1` (= 1 for IP-only streams).
- GStreamer (`gstvah264enc.c`): Same — writes `max_dec_frame_buffering` into
  SPS VUI via `gst_h264_bit_writer_sps()`, submits as packed header.

Both generate the *entire* SPS from scratch rather than patching the
driver-generated one.

**Applies to H.264 and H.265** (HEVC has analogous VPS/SPS DPB signaling).

**Implementation path**: Either contribute VUI support upstream to cros-codecs,
or generate our own packed SPS NALU with correct VUI and submit it alongside
the encoder's output.
