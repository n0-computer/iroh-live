# Codec Architecture

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | rusty-codecs |

## Traits

rusty-codecs defines four core encoding/decoding traits in `src/traits.rs`:

- `VideoEncoder`: push `VideoFrame`, pop `EncodedFrame`. Push/pop streaming interface.
- `VideoDecoder`: push `MediaPacket`, pop `VideoFrame`. Created from catalog `VideoConfig`.
- `AudioEncoder`: push interleaved f32 PCM samples, pop `EncodedFrame`.
- `AudioDecoder`: push `MediaPacket`, pop decoded f32 sample slices.

Each encoder trait has a companion factory trait (`VideoEncoderFactory`, `AudioEncoderFactory`) that provides static construction from config or presets, plus a `config_for()` method that returns the catalog entry without building an encoder.

`VideoSource` and `AudioSource` traits abstract capture devices and synthetic sources. `PreEncodedVideoSource` handles cases where the capture device produces compressed output directly (hardware RTSP cameras, `rpicam-vid` on Raspberry Pi).

## Codec enum and auto-selection

`VideoCodec` enumerates all compiled-in video codec backends. `VideoCodec::best_available()` returns the preferred encoder for the current platform, favoring hardware over software: VideoToolbox on macOS, VAAPI on Linux desktop, V4L2 on Linux ARM, MediaCodec on Android, then openh264 as the universal software fallback.

`DynamicVideoDecoder` implements auto-selection on the decode side. Given a `VideoConfig` from the catalog, it tries hardware decoders first (VAAPI, V4L2, VideoToolbox, MediaCodec) and falls back to software if all fail. The `DynamicAudioDecoder` follows the same pattern for Opus.

## Software codecs

All software codecs compile from bundled source, with no system library dependencies:

- **H.264**: openh264 via `cc` build (C source compiled at `cargo build` time). Encoder produces Baseline profile. Decoder handles all profiles the encoder emits.
- **AV1**: rav1e (encoder, pure Rust) and rav1d (decoder, pure Rust port of dav1d). Both compile without external dependencies.
- **Opus**: unsafe-libopus, a c2rust transpilation of libopus. Provides the full Opus feature set (SILK, CELT, hybrid modes) as Rust code with unsafe blocks.

## Hardware acceleration

Each hardware backend is gated behind a feature flag and a target OS cfg:

| Backend | Feature | Platform | API | Crate/binding |
|---------|---------|----------|-----|---------------|
| VAAPI | `vaapi` | Linux | libva | cros-codecs (Google/ChromeOS) |
| VideoToolbox | `videotoolbox` | macOS | VTCompressionSession | objc2 bindings |
| V4L2 | `v4l2` | Linux ARM | V4L2 M2M ioctls | raw libc ioctls |
| MediaCodec | `android` | Android | NDK AMediaCodec | ndk crate (0.9) |

See [linux/vaapi.md](linux/vaapi.md), [macos/videotoolbox.md](macos/videotoolbox.md), [pi/v4l2.md](pi/v4l2.md), and [android/mediacodec.md](android/mediacodec.md) for platform-specific details.

## Image processing

rusty-codecs replaces FFmpeg's image processing utilities with focused Rust crates:

- **Color conversion**: yuvutils-rs for NV12/I420/RGBA conversion.
- **Scaling**: pic-scale for frame resizing. The `Scaler` struct caches the destination buffer across calls to avoid per-frame allocation.
- **Audio resampling**: rubato for sample rate conversion between device rates and the 48 kHz internal processing rate.

## Frame types

`VideoFrame` carries decoded video data in one of several backing stores: `Packed` (RGBA/BGRA byte buffer), `Nv12` (Y + UV plane pair), `I420` (Y + U + V planes), or `Gpu` (platform-specific handle via `NativeFrameHandle`). The `Gpu` variant wraps DMA-BUF fds on Linux and `HardwareBuffer` refs on Android, enabling zero-copy paths from decoder to renderer.

`AudioFormat` specifies sample rate, channel count, and sample format. All internal processing uses 48 kHz stereo f32.
