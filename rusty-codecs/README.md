# rusty-codecs

Audio and video codec implementations for the iroh-live media pipeline: encoding, decoding, format conversion, and GPU rendering.

This crate defines the core traits (`VideoEncoder`, `VideoDecoder`, `AudioEncoder`, `AudioDecoder`, `VideoSource`) and frame types (`VideoFrame`, `AudioFrame`) that the rest of the stack builds on. Codec backends are selected via feature flags, so downstream crates pull in only what they need.

## Codec support

All software codecs compile from bundled source with no system dependencies. Hardware backends use platform APIs directly.

| Codec | Backend | Platform | Feature |
|-------|---------|----------|---------|
| H.264 | openh264 (bundled C) | All | `h264` |
| H.264 | VA-API | Linux (Intel, AMD) | `vaapi` |
| H.264 | VideoToolbox | macOS, iOS | `videotoolbox` |
| H.264 | V4L2 (stateful) | Linux (RPi, Rockchip) | `v4l2` |
| H.264 | MediaCodec (NDK) | Android | `android` |
| AV1 | rav1e + rav1d (pure Rust) | All | `av1` |
| Opus | unsafe-libopus (Rust port) | All | `opus` |
| PCM | Raw samples (no codec) | All | `pcm` |

See [`plans/platforms.md`](../plans/platforms.md) for tested hardware and the full support matrix.

## GPU rendering

The `wgpu` feature enables `WgpuVideoRenderer`, which converts decoded frames to display-ready textures using wgpu. This runs on Vulkan (Linux), Metal (macOS), and DX12 (Windows). The `gles` feature provides `GlesRenderer` for OpenGL ES 2.0 contexts, useful on Android and platforms without wgpu support. A CPU-based RGBA conversion path is always available as a software fallback.

Zero-copy rendering is available on most platforms. On Linux with `dmabuf-import`, decoded frames from VA-API are imported as Vulkan textures without any CPU copy. On macOS with `metal-import`, VideoToolbox decoder output can be imported directly as Metal textures. On Android, `AHardwareBuffer` frames from MediaCodec are imported via EGL.

| Feature | Backend | Platform |
|---------|---------|----------|
| `wgpu` | Vulkan, Metal, DX12 | Cross-platform |
| `gles` | OpenGL ES 2.0 | Android, Linux, others |
| `dmabuf-import` | Vulkan + VPP retiler | Linux (Intel tested) |
| `metal-import` | Metal IOSurface | macOS |

## High-level concepts

A **video frame** (`VideoFrame`) is the central data type. It can be backed by CPU memory in various pixel formats (RGBA, NV12, I420) or by a GPU handle such as a DMA-BUF file descriptor or an Android `AHardwareBuffer`. Frames carry their dimensions, pixel format, and presentation timestamp.

**Video sources** implement the `VideoSource` trait, which provides a `pop_frame()` method that returns the latest captured frame. Cameras, screen capturers, and test pattern generators all implement this trait.

**Encoders and decoders** (`VideoEncoder`, `VideoDecoder`, `AudioEncoder`, `AudioDecoder`) are trait objects that each codec backend implements. The trait interface is the same regardless of whether the backend is software or hardware-accelerated, so switching backends is a feature flag change rather than a code change.

**Presets** (`VideoPreset`) define common resolution and bitrate combinations (360p, 720p, 1080p) that simplify encoder configuration.

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `h264` | yes | H.264 via openh264 |
| `opus` | yes | Opus via unsafe-libopus |
| `av1` | | AV1 via rav1e/rav1d |
| `vaapi` | | Linux VA-API hardware H.264 |
| `v4l2` | | V4L2 stateful hardware H.264 |
| `videotoolbox` | | macOS/iOS VideoToolbox H.264 |
| `android` | | Android MediaCodec H.264 |
| `wgpu` | | GPU rendering (Vulkan/Metal/DX12) |
| `gles` | | OpenGL ES 2.0 rendering |
| `dmabuf-import` | | Zero-copy DMA-BUF texture import (Linux) |
| `metal-import` | | Zero-copy Metal texture import (macOS) |
| `pcm` | | Raw PCM audio (no encoding/decoding) |
| `gles-dmabuf` | | GLES DMA-BUF zero-copy import |
| `test-util` | | Test helpers and pattern generators |
| `hang` | | Integration with hang catalog format |
