# Platform support

Software codecs (H.264 via openh264, AV1 via rav1e/rav1d, Opus) work on every
platform Rust targets. The tables below cover hardware acceleration, capture
backends, and GPU rendering.

For the full matrix with implementation status and hardware-specific notes, see
[plans/platforms.md](../plans/platforms.md).

## Summary

| Platform | Codecs | Capture | GPU render | Status |
|----------|--------|---------|------------|--------|
| Linux (Intel/AMD) | Software and VAAPI HW | PipeWire, V4L2, X11 | Vulkan and DMA-BUF zero-copy | Mostly working |
| macOS | Software and VideoToolbox | ScreenCaptureKit, AVFoundation | Metal via wgpu | Mostly working |
| Android | Software and MediaCodec HW | CameraX via JNI | EGL and HardwareBuffer zero-copy | Mostly working |
| Raspberry Pi | Software and V4L2 HW | V4L2 camera | Vulkan (RPi 5) | Mostly working |
| Windows | Software only | Not yet implemented | DX12 via wgpu | Missing |
| iOS | Software and VideoToolbox | AVFoundation | Metal via wgpu | Compiles, untested |

## What "mostly working" means

All platforms marked as mostly working have been tested on at least one device
and handle the core publish/subscribe flow. Testing coverage is still limited,
especially for edge cases like device hot-plug, resolution changes, and error
recovery. Known issues are tracked in the
[issue tracker](https://github.com/n0-computer/iroh-live/issues).

## Next steps

- **Windows**: Media Foundation H.264 encoder and decoder. No code yet.
- **VAAPI AV1**: Decode on Intel Gen12+. Encode on Intel Arc.
- **V4L2 stateless**: For Rockchip/Allwinner/MediaTek SBCs.
- **iOS**: Untested but compiles. Needs on-device validation.
