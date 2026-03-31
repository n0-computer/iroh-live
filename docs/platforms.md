# Platform support

Software codecs (H.264 via openh264, AV1 via rav1e/rav1d, Opus, PCM) work on
every platform Rust targets. The tables below cover hardware acceleration,
capture backends, and GPU rendering.

## Summary

| Platform | Codecs | Capture | GPU render | Status |
|----------|--------|---------|------------|--------|
| Linux (Intel/AMD) | Software + VAAPI HW | PipeWire, V4L2, X11, nokhwa, xcap | Vulkan + DMA-BUF zero-copy | Tested (Intel MTL) |
| macOS | Software + VideoToolbox | ScreenCaptureKit, nokhwa | Metal via wgpu | Partially tested |
| Android | Software + MediaCodec HW | CameraX via JNI | EGL + HardwareBuffer zero-copy | Tested on device |
| Raspberry Pi | Software + V4L2 HW | libcamera, V4L2 | GLES2 (Pi 4), Vulkan (Pi 5) | Tested (Pi Zero 2 W) |
| Windows | Software only | Not yet implemented | DX12 via wgpu | Missing |
| iOS | Software + VideoToolbox | AVFoundation | Metal via wgpu | Compiles, untested |

## Codec support

| Codec | Backend | Platform | Tested |
|-------|---------|----------|--------|
| H.264 encode/decode | openh264 (software) | All | Linux |
| H.264 encode/decode | VAAPI | Linux + Intel/AMD GPU | Intel MTL |
| H.264 encode/decode | VideoToolbox | macOS / iOS | Not yet |
| H.264 encode/decode | V4L2 (stateful) | Linux + RPi / Rockchip | Not yet |
| H.264 encode/decode | MediaCodec (NDK) | Android | Android device |
| AV1 encode/decode | rav1e/rav1d (software) | All | Linux |
| Opus encode/decode | unsafe-libopus | All | Linux |
| PCM encode/decode | Built-in (raw f32le) | All | Linux |

## Capture support

| Source | Backend | Platform | Tested |
|--------|---------|----------|--------|
| Screen | PipeWire ScreenCast | Linux (Wayland) | Intel MTL |
| Screen | X11 SHM | Linux (X11) | Intel MTL |
| Screen | ScreenCaptureKit | macOS | Yes |
| Screen | xcap | Linux, macOS, Windows | Linux |
| Camera | PipeWire | Linux | Intel MTL |
| Camera | V4L2 | Linux | Intel MTL |
| Camera | libcamera (rpicam-vid) | Raspberry Pi | Pi Zero 2 W |
| Camera | nokhwa | Linux, macOS, Windows | Linux, macOS |
| Camera | AVFoundation | macOS / iOS | Stubbed (use nokhwa) |
| Camera | CameraX via JNI | Android | Android device |

## GPU rendering

| Backend | Platform | Zero-copy | Tested |
|---------|----------|-----------|--------|
| wgpu (Vulkan) | Linux | DMA-BUF import via VPP retiler | Intel MTL |
| wgpu (Metal) | macOS | CVMetalTexture import | Not yet |
| wgpu (DX12) | Windows | Not yet | Not yet |
| GLES2 | Linux, Android | DMA-BUF via EGLImage | Android |
| Metal import | macOS | CVPixelBuffer zero-copy | Not yet |

## Platform notes

### macOS
Native AVFoundation camera capture is stubbed out (incomplete). Use the
nokhwa backend instead — it is enabled by default and works for camera
access on macOS. Screen capture via ScreenCaptureKit works.

### Raspberry Pi
libcamera capture is in rusty-capture under the `libcamera` feature.
It uses rpicam-vid as a subprocess (runtime dependency). The pre-encoded
H.264 path (`LibcameraH264Source`) feeds hardware-encoded packets directly
to the transport, bypassing the software encoder. Cross-compile with
`cargo make cross-build-aarch64`.

### Android
MediaCodec H.264 via NDK, CameraX capture via JNI bridge. The demo app
is in `demos/android/`. Surface-mode encoder (zero-copy GPU scaling) is
planned but not yet implemented.

## Next steps

- **Windows**: Media Foundation H.264 encoder and decoder.
- **VAAPI AV1**: Decode on Intel Gen12+, encode on Intel Arc.
- **V4L2 stateless**: For Rockchip/Allwinner/MediaTek SBCs.
- **iOS**: Compiles but untested. Needs on-device validation.
- **macOS camera**: Complete the AVFoundation camera backend or improve nokhwa integration.
