# Platform Support

Matrix of feature support across hardware–OS combinations for `rusty-codecs`
and `rusty-capture`. "Impl" means code exists; "Tested" means we have
confirmed it works on real hardware.

## Codec support

| Feature | Backend | Platform | Impl | Tested |
|---------|---------|----------|------|--------|
| H.264 encode | openh264 (software) | All | Yes | Linux only |
| H.264 decode | openh264 (software) | All | Yes | Linux only |
| H.264 encode | VAAPI | Linux + Intel/AMD GPU | Yes | Intel MTL |
| H.264 decode | VAAPI | Linux + Intel/AMD GPU | Yes | Intel MTL |
| H.264 encode | VideoToolbox | macOS / iOS | Yes | — |
| H.264 encode | V4L2 (stateful) | Linux + RPi / Rockchip | Yes | — |
| H.264 decode | V4L2 (stateful) | Linux + RPi / Rockchip | Yes | — |
| H.264 encode | Media Foundation | Windows | — | — |
| H.264 decode | Media Foundation | Windows | — | — |
| H.264 decode | VideoToolbox | macOS / iOS | Yes | — |
| H.264 encode | MediaCodec (NDK) | Android | Yes | — |
| H.264 decode | MediaCodec (NDK) | Android | Yes | — |
| AV1 encode | rav1e (software) | All | Yes | Linux only |
| AV1 decode | rav1d (software) | All | Yes | Linux only |
| AV1 encode/decode | VAAPI | Linux + Intel/AMD GPU | — | — |
| Opus encode | unsafe-libopus | All | Yes | Linux only |
| Opus decode | unsafe-libopus | All | Yes | Linux only |

### Missing codec work

- **Windows**: Media Foundation H.264 encoder and decoder. Design exists in
  `plans/media-pipeline/phase-2b-windows-media-foundation.md`, no code yet.
- **macOS**: VideoToolbox H.264 encoder and decoder both implemented, not yet tested.
- **Android**: MediaCodec H.264 encoder and decoder via `ndk` crate (0.9) in
  synchronous ByteBuffer mode. Implemented, cross-compilation verified, not
  yet tested on device.
- **VAAPI AV1**: cros-codecs supports AV1 stateless decode on Intel Gen12+.
  Encoder would require a `libva` AV1 encode entrypoint (Intel Arc).
- **V4L2 stateless decoder**: for Rockchip/Allwinner/MediaTek SBCs. Plan
  exists in `plans/media-pipeline/v4l2-future.md`.

## Capture support

| Feature | Backend | Platform | Impl | Tested |
|---------|---------|----------|------|--------|
| Screen capture | PipeWire ScreenCast | Linux (Wayland) | Yes | Intel MTL |
| Camera capture | PipeWire | Linux | Yes | Intel MTL |
| Camera capture | V4L2 | Linux | Yes | Intel MTL |
| Screen capture | ScreenCaptureKit | macOS | Yes | — |
| Camera capture | AVFoundation | macOS / iOS | Yes | — |
| Screen capture | — | Windows | — | — |
| Camera capture | — | Windows | — | — |
| Screen capture | xcap | Linux, macOS, Windows | Yes | Linux only |
| Camera capture | nokhwa | Linux, macOS, Windows | Yes | Linux only |
| Camera capture | Camera2 via JNI | Android | Yes | — |

### Missing capture work

- **Windows**: Screen capture via DXGI Desktop Duplication or Windows.Graphics.Capture.
  Camera capture via Media Foundation. No plan or code exists.
- **Android**: Camera2 implemented via JNI + Kotlin helper. Not tested on device.
  NDK-only `AImageReader` path possible on API 24+.
- **Linux X11**: `x11rb` SHM capture is partially wired (`x11` feature in
  rusty-capture) but not integrated into the main capture path.

## GPU rendering

| Feature | Backend | Platform | Impl | Tested |
|---------|---------|----------|------|--------|
| wgpu NV12 shader | Vulkan | Linux | Yes | Intel MTL |
| DMA-BUF import | Vulkan + VPP retiler | Linux + Intel | Yes | Intel MTL |
| wgpu NV12 shader | Metal | macOS | Yes | — |
| wgpu NV12 shader | DX12 | Windows | Yes | — |
| AHardwareBuffer import | Vulkan | Android (API 26+) | — | — |

The wgpu renderer is cross-platform by design (Vulkan, Metal, DX12). The
DMA-BUF zero-copy path is Linux-only. macOS could use IOSurface import for
VideoToolbox→Metal zero-copy, but this is not implemented.

## Platform-specific hardware notes

### Linux + Intel (tested: Meteor Lake Arc Graphics)

Full pipeline tested: PipeWire capture → VAAPI H.264 encode → moq transport →
VAAPI H.264 decode → DMA-BUF import → wgpu Vulkan render. Zero-copy
decode→render works via VPP retiler (Y-tiled→CCS modifier conversion).
Vulkan Video decode is *not* supported on MTL (ANV Mesa 26.0.1).

### Linux + NVIDIA

VAAPI works through `nvidia-vaapi-driver` (translation layer to NVDEC/NVENC).
Not tested. DMA-BUF import likely requires different modifier handling than
Intel.

### Linux + AMD

VAAPI is native via Mesa RADV. Not tested. DMA-BUF import should work
similarly to Intel but with different modifiers.

### Raspberry Pi 4 (BCM2711)

V4L2 stateful H.264 encoder and decoder. Tested by upstream `v4l2r` crate
authors. Our V4L2 backend compiles but has not been tested on RPi hardware.

### Raspberry Pi 5 (BCM2712)

Same V4L2 path as RPi 4. The VideoCore VII GPU supports Vulkan 1.2 via the
`v3dv` driver, so wgpu rendering should work. Not tested.

### macOS (Apple Silicon)

VideoToolbox H.264 encoder and decoder both compile. Capture (ScreenCaptureKit +
AVFoundation) compiles. Neither has been tested on hardware. Metal rendering via
wgpu should work out of the box. The decoder produces NV12 `GpuFrame` outputs
for deferred CPU readback, matching the VAAPI decoder pattern.

### Windows

No codec or capture backends implemented. See
`plans/media-pipeline/phase-2b-windows-media-foundation.md` for the MF encoder
design. Screen capture should use DXGI Desktop Duplication; camera capture
should use Media Foundation.

### Android

Codec and capture code exists. Cross-compilation verified with `cargo ndk`.
Not yet tested on a real device.

- **Codecs**: `ndk` crate (0.9, `media` feature) for H.264 encode and decode
  via `AMediaCodec` in synchronous ByteBuffer mode. Surface mode planned for
  zero-copy. Minimum API 21.
- **Camera**: Camera2 via JNI (`jni` 0.21) with Kotlin helper class. Pure-NDK
  `AImageReader` path possible on API 24+.
- **Demo app**: Kotlin/Gradle app in `android-demo/` with JNI bridge.
  See `android-demo/README.md` for build instructions.
- **Screen capture**: `MediaProjection` requires Activity context and user
  permission grant — inherently Java/Kotlin. Lower priority.
- **GPU rendering**: wgpu works on Android via Vulkan. Zero-copy decode→render
  possible via `AHardwareBuffer` Vulkan import (API 26+,
  `VK_ANDROID_external_memory_android_hardware_buffer`).

### iOS

AVFoundation camera capture compiles (shared with macOS). VideoToolbox encoder
compiles (shared with macOS). Neither tested. Screen capture is limited to
ReplayKit broadcast extensions, which is a different architecture from
ScreenCaptureKit.
