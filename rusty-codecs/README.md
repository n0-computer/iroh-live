# rusty-codecs

Audio and video codec implementations: encoding, decoding, and format conversion.

Defines the core traits (`VideoSource`, `VideoEncoder`, `VideoDecoder`, `AudioEncoder`, `AudioDecoder`) and frame types (`VideoFrame`, `AudioFrame`) used across the iroh-live media pipeline. Codec backends are selected via feature flags, so downstream crates pull in only what they need.

## Codecs

| Codec | Encoder | Decoder | Feature |
|-------|---------|---------|---------|
| H.264 | openh264 (bundled C) | openh264 | `h264` |
| H.264 | VideoToolbox (macOS) | VideoToolbox | `videotoolbox` |
| H.264 | VA-API (Linux) | VA-API | `vaapi` |
| AV1 | rav1e (pure Rust) | rav1d (pure Rust) | `av1` |
| Opus | unsafe-libopus (Rust port) | unsafe-libopus | `opus` |

All software backends are either pure Rust or compiled from bundled source — no system ffmpeg or codec libraries required. Hardware backends (VideoToolbox, VA-API) use platform APIs directly.

## Feature flags

- **`h264`** — H.264 via openh264, built from bundled source with `cc`.
- **`av1`** — AV1 via rav1e (encode) and rav1d (decode), both pure Rust.
- **`opus`** — Opus via unsafe-libopus with rubato for sample rate conversion.
- **`videotoolbox`** — macOS hardware H.264 via VideoToolbox.
- **`vaapi`** — Linux hardware H.264 via VA-API (Intel, AMD).
- **`wgpu`** — GPU-accelerated video rendering and format conversion.
- **`dmabuf-import`** — Zero-copy DMA-BUF texture import on Linux/Vulkan. Implies `wgpu`.
- **`hang`** — Integration with the [hang](https://github.com/kixelated/moq) catalog format.

## Key types

- `VideoFrame` — a decoded video frame, backed by CPU memory (RGBA, NV12, I420) or a GPU texture.
- `VideoSource` — trait for frame producers (cameras, screen capturers, test patterns).
- `VideoEncoder` / `VideoDecoder` — traits for codec implementations.
- `AudioEncoder` / `AudioDecoder` — traits for audio codec implementations.
- `PixelFormat`, `VideoFormat`, `VideoPreset` — format metadata and presets.
