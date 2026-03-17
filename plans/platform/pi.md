# Raspberry Pi Platform — Technical Notes

## Hardware tested

| Component | Model | Notes |
|-----------|-------|-------|
| SBC | Raspberry Pi Zero 2 WH | Quad Cortex-A53 @ 1 GHz, 512 MB RAM |
| Camera | OV5647 (Pi Camera v1.3) | CSI ribbon, max 2592×1944 @ 15 fps |
| Display | Waveshare 2.13" Touch e-Paper HAT | SPI, V4 controller (SSD1680), 250×122 px |
| OS | Raspberry Pi OS Bookworm (Debian 12) | Kernel 6.6.31, aarch64 |

## Camera: libcamera stack

On Bookworm the CSI camera goes through the libcamera stack. Direct V4L2
access to `/dev/video0` (Unicam) returns raw Bayer sensor data at
16376×16376 in GREY format — unusable without the ISP pipeline.

`rpicam-vid` handles the full ISP→encoder path internally:

| Mode | Command | Output | Throughput |
|------|---------|--------|------------|
| Raw YUV | `--codec yuv420 -o -` | I420 frames on stdout | ~10 MB/s at 360p30 |
| H.264 | `--codec h264 --inline -o -` | Annex-B H.264 on stdout | ~100–300 KB/s |

The H.264 mode is strongly preferred: eliminates 10 MB/s pipe copy,
avoids redundant NV12 conversion, and uses rpicam-vid's internal DMABUF
zero-copy path from ISP to encoder.

### Future: libcamera as library

Currently we spawn `rpicam-vid` as a subprocess and read H.264 from stdout.
This works well but has limitations:

- No programmatic bitrate control mid-stream
- No keyframe-on-demand signaling
- Subprocess management overhead
- No access to camera metadata (exposure, white balance)

Ideally we'd use libcamera as a library via Rust FFI. State of Rust bindings
(as of 2026-03):

- **libcamera-rs** — most mature, wraps libcamera C++ API via cxx. Provides
  `CameraManager`, `Camera`, `Request`, `FrameBuffer`. Should work for raw
  frame capture. Unclear if it exposes the ISP→encoder pipeline that
  rpicam-vid uses internally.
- **Direct FFI** — libcamera's C API (`libcamera-c`) is minimal. The C++
  API is the primary interface, making raw FFI through bindgen difficult.

The subprocess approach is good enough for the demo. Library integration
should wait until we need runtime encoder control or metadata access.

### rpicam-vid H.264 flags

```
--codec h264          H.264 via bcm2835-codec (/dev/video11)
--inline              Prepend SPS+PPS before every IDR
--bitrate 500000      Target bitrate (500 kbps)
--intra 60            Keyframe interval (frames)
--profile baseline    H.264 profile (baseline/main/high)
--level 3.1           H.264 level
--save-pts /dev/fd/2  Write per-frame PTS to stderr
--timeout 0           Run indefinitely
--nopreview           No preview window
-o -                  Output to stdout
```

## V4L2 M2M encoder

The `v4l2r` crate (0.0.7) has a bug: `enqueue_capture_buffers()` fails
immediately with `NoFreeBuffer` on bcm2835-codec, despite buffers being
properly allocated via `VIDIOC_REQBUFS`. No CAPTURE buffers are ever
queued, so the encoder accepts frames but never produces output.

Replaced with raw V4L2 ioctls matching ffmpeg's `h264_v4l2m2m` sequence
(verified via strace). Key gotchas:

- `V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE = 10` (not 8)
- `V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE = 9`
- `V4L2_CID_CODEC_BASE = 0x00990900` (not 0x00990000)
- `v4l2_format` is 208 bytes on aarch64 (4-byte padding before union)
- Must `G_FMT` before `S_FMT` — bcm2835-codec rejects zeroed format structs
- Queue first OUTPUT buffer before STREAMON (matches ffmpeg)

## E-paper: V4 controller

The Waveshare 2.13" Touch e-Paper HAT (product 20716) uses the V4
controller protocol (SSD1680), not V2/V3. The `epd-waveshare` crate's
`Epd2in13` type targets V2/V3 and doesn't work.

Custom V4 driver in `epd_v4.rs` with correct init sequence and BUSY
polarity.

### Precautions (from datasheet)

1. **Refresh interval**: minimum 180 s between refreshes
2. **Sleep mode**: put to sleep when not refreshing — prolonged high
   voltage damages the panel irreversibly
3. **24-hour refresh**: refresh at least once per 24 h (we do every 12 h)
4. **Clear before storage**: send all-white frame and sleep before power off
5. **No partial-only refresh**: mix with full refreshes
6. **Fragile FPC cable**: don't bend vertically or toward screen front

## Cross-compilation

From x86_64 to aarch64 using `build.sh`. Requires Pi sysroot with dev
headers.

### Known issues

**ffmpeg feature**: cannot cross-compile. `ffmpeg-sys-next`'s build
script runs a host-side feature-check binary using target headers.

**libc.so linker script**: Debian's `libc.so` has absolute paths. The
build script creates symlinks in the sysroot to resolve them.

## Network on Pi Zero 2 W

- SSH slow due to DNS reverse lookup → `UseDNS no`
- WiFi power management → `iw wlan0 set power_save off`
- GSO errors (`sendmsg: I/O error`, errno 5) — iroh recovers by disabling GSO
- Relay oscillation between EU/US → pin with `IROH_RELAY` env var
