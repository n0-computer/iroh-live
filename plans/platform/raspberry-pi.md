# Raspberry Pi platform

V4L2 stateful H.264 encoder and decoder for Raspberry Pi hardware, plus a
headless demo with DRM-based video playback. Tested on Pi Zero 2 WH with
OV5647 camera module.

## Status

- [x] V4L2 stateful H.264 encoder (`V4l2Encoder` in `rusty-codecs`)
- [x] V4L2 stateful H.264 decoder (`V4l2Decoder` in `rusty-codecs`)
- [x] Raw ioctl encoder rewrite for Pi Zero 2 driver quirks
- [x] `pi-zero-demo` crate with headless publishing and DRM playback
- [x] DRM rendering with dedicated render thread and audio
- [x] FFmpeg H.264 backend with V4L2 M2M probing on ARM
- [ ] Stateless V4L2 decoder (for Rockchip, Allwinner, MediaTek SBCs)
- [ ] True zero-copy DMA-BUF encode (V4L2 DMABUF mode)
- [ ] libcamera library integration (currently shells out to `rpicam-vid`)
- [ ] E-paper display driver (SSD1680 controller, low priority)

## Details

### V4L2 encoder

The Pi Zero 2's `bcm2835-codec` V4L2 M2M driver has quirks that the `v4l2r`
Rust crate cannot accommodate: non-standard buffer negotiation, specific control
ordering, and NV12 plane layout differences. After multiple attempts with the
high-level API, the encoder was rewritten with raw ioctls using `libc`, giving
direct control over `VIDIOC_REQBUFS`, `VIDIOC_QBUF`/`DQBUF`, and
`VIDIOC_STREAMON` sequencing.

The encoder accepts NV12 frames (converted from RGBA/I420 in software) and
produces Annex B H.264 output with configurable bitrate and keyframe interval.

### DRM rendering

The Pi Zero 2 has no desktop compositor — rendering goes through the DRM/KMS
API directly. Page-flip ioctls block until vsync, so the renderer runs on a
dedicated OS thread separate from the decode loop. Audio playback uses ALSA
via `cpal`.

### Camera capture

Camera frames come from `rpicam-vid` piped as raw H.264 to stdin, which the
demo publishes directly as pre-encoded video. A future improvement would
integrate `libcamera` as a library for native frame capture with format
negotiation.

### Cross-compilation

Building for `aarch64-unknown-linux-gnu` from x86_64 requires:
- The `ffmpeg` feature cannot be cross-compiled (system FFmpeg headers are
  host-only). Use native codec features instead.
- The `libc.so` linker script on Debian/Bookworm references absolute paths
  that do not exist in the sysroot. Workaround: copy `libc.so.6` directly.

### Future: stateless V4L2

Rockchip (RK3588), Allwinner, and MediaTek SBCs expose stateless V4L2 decoders
that require the application to manage bitstream parsing and the decoded picture
buffer (DPB). The `cros-codecs` crate supports this but has cross-compilation
issues with its `libva` dependency. A standalone stateless decoder using raw
V4L2 ioctls (matching the encoder approach) is the likely path forward.

### Hardware tested

| Component | Model |
|-----------|-------|
| Board | Raspberry Pi Zero 2 WH |
| Camera | OV5647 (CSI) |
| Display | Waveshare e-Paper HAT (SSD1680) |
| OS | Raspberry Pi OS Bookworm (64-bit) |

## Implementation history

- `c0fb875` — V4L2 stateful H.264 encoder and decoder
- `4c12d41` — `pi-zero-demo` crate
- `3b6713c` — DRM rendering with dedicated render thread and audio
- `e4ea2b2` — raw ioctl encoder rewrite for Pi Zero 2
- `a875ab7` — FFmpeg backend with V4L2 M2M probing on ARM
