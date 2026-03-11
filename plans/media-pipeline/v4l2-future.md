# V4L2 Future Work

## Current State

Stateful V4L2 H.264 encoder/decoder implemented via `v4l2r` 0.0.7 behind the
`v4l2` feature flag. Targets `bcm2835-codec` on Raspberry Pi (Zero 2 W, 3, 4).
Also covers Qualcomm venus, Samsung s5p-mfc, and some MediaTek devices that
expose stateful V4L2 interfaces.

## Stateless V4L2 Decoder

### What it unlocks

| Platform | Driver | Codec support |
|---|---|---|
| Rockchip (RK3588, RK3399) | hantro / rkvdec | H.264, VP9, HEVC, AV1 (rkvdec2) |
| Allwinner (A64, H6) | cedrus | H.264, HEVC, VP9 |
| MediaTek (some SoCs) | mtk-vcodec (stateless mode) | H.264, VP9 |

These are popular SBC platforms (Orange Pi 5, Rock 5B, Pine64, Lichee).

### Why it's hard

Stateless V4L2 requires the caller to:

1. **Parse the bitstream** — extract SPS/PPS (H.264), frame headers, reference
   frame info for every frame
2. **Manage the DPB** (decoded picture buffer) — track which decoded frames are
   needed as references, evict when no longer needed
3. **Submit per-frame controls** via the V4L2 Request API — atomically attach
   codec-specific controls (SPS, PPS, slice params, decode params) to each
   buffer before queueing
4. **Handle the Request API lifecycle** — create request FDs, attach controls
   and buffers, submit, wait for completion

v4l2r 0.0.7 has low-level primitives for this (`QBuffer::set_request()`,
stateless control structs in `controls::codec`) but **no high-level stateless
abstraction**. The stateful decoder explicitly rejects stateless devices.

Estimated effort: 2-3x the stateful implementation. Essentially writing a
partial H.264 parser + reference frame manager.

### Alternative: cros-codecs

The `cros-codecs` crate (from ChromeOS) already implements stateless V4L2
decoding with full bitstream parsing and DPB management. However, it has an
**aarch64 cross-compilation bug** — the build.rs / bindgen setup doesn't handle
cross-compilation properly (wrong include paths, host vs target confusion).
Since `cros-codecs` is developed for same-arch ChromeOS builds, this isn't
well-tested upstream.

Options:
- Fix the cross-compilation issue upstream in `cros-codecs` and use it directly
- Wait for upstream to fix it
- Implement stateless from scratch using v4l2r primitives

### No stateless encoder

V4L2 stateless encoders don't really exist in practice. Encoding is always
stateful (the hardware manages rate control, reference frames, etc.).

## True Zero-Copy DMA-BUF Encode

Current V4L2 encoder accepts NV12/GPU frames efficiently but still copies data
into MMAP buffers. True zero-copy requires:

1. Allocate the encoder's OUTPUT queue with `V4L2_MEMORY_DMABUF` at init time
2. Pass `DmaBufHandle` wrapping the capture FD directly when queueing
3. Encoder needs to know at construction whether to use MMAP or DMA-BUF mode
   (can't switch mid-stream)
4. Capture pipeline must produce `DmaBufInfo`-carrying frames

This is straightforward once the capture pipeline exports DMA-BUFs (e.g. from
V4L2 camera capture via libcamera on Pi). The v4l2r `DmaBufHandle<File>` and
`allocate_output_buffers_generic::<GenericBufferHandles>(DmaBuf, N)` APIs
support this directly.
