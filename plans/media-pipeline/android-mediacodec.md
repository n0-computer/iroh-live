# Android MediaCodec Integration

Status: planned. No code exists.

## Goal

Provide hardware-accelerated H.264 encode and decode on Android using the
NDK `AMediaCodec` API, and camera capture via the Camera2 API.

## Dependencies

| Crate | License | Purpose |
|-------|---------|---------|
| `mediacodec` (crates.io) | MIT | Thin NDK wrapper around `AMediaCodec` / `AMediaFormat` |
| `jni` (crates.io) | MIT/Apache-2.0 | JNI bindings for Camera2, `MediaProjection` |
| `ndk` (crates.io) | MIT/Apache-2.0 | Low-level NDK types (`AHardwareBuffer`, etc.) |

The `mediacodec` crate (early-stage, 0.x) wraps the C NDK surface directly,
avoiding JNI overhead for the codec path. Camera2 has no equivalent pure-NDK
crate, so JNI with `jni-rs` is the recommended approach.

Inspiration sources (all MIT or Apache-2.0 compatible):
- FFmpeg `mediacodec*.c` — buffer lifecycle, `INFO_OUTPUT_FORMAT_CHANGED` handling
- GStreamer `androidmedia` plugin — `amcviddec` / `amcvidenc` state machines
- WebRTC `modules/video_coding/codecs/android/` — async mode, Surface output

## Architecture

```
rusty-codecs/src/codec/android/
├── mod.rs          # feature gate, re-exports
├── encoder.rs      # MediaCodecEncoder implements VideoEncoder
├── decoder.rs      # MediaCodecDecoder implements VideoDecoder
└── format.rs       # AMediaFormat helpers (SPS/PPS, color format)
```

Feature flag in `rusty-codecs/Cargo.toml`:

```toml
[target.'cfg(target_os = "android")'.dependencies]
mediacodec = "0.1"
jni = "0.21"

[features]
android = []  # enables the android codec module
```

## Encoder design

The encoder accepts `VideoFrame` (I420 or NV12 packed data) and produces
H.264 NAL units wrapped in `EncodedFrame`.

### Phase 1 — synchronous ByteBuffer mode

Synchronous mode is simpler and sufficient for the first implementation.
The encoder runs on its own OS thread (matching the existing VAAPI/V4L2
pattern).

```
input: VideoFrame (I420/NV12 bytes)
  → dequeueInputBuffer(timeout)
  → copy frame into input buffer
  → queueInputBuffer(pts, flags)
  → dequeueOutputBuffer(timeout)
  → copy NAL units from output buffer
  → releaseOutputBuffer
output: EncodedFrame { data, pts, is_keyframe }
```

Key configuration:
- MIME type: `video/avc`
- Color format: `COLOR_FormatYUV420SemiPlanar` (NV12) preferred,
  fall back to `COLOR_FormatYUV420Planar` (I420)
- Bitrate mode: `BITRATE_MODE_VBR` with target from `VideoPreset`
- Keyframe interval: configurable via `EncoderConfig::keyframe_interval`
- Profile/level: Baseline for compatibility, Main if available

Error handling:
- `INFO_OUTPUT_FORMAT_CHANGED`: re-read SPS/PPS from output format
- `INFO_TRY_AGAIN_LATER`: retry with backoff (not a fatal error)
- Codec reset on persistent errors (3 consecutive failures)

### Phase 2 — hardening

- Color format negotiation: query codec capabilities, pick best match
- Adaptive bitrate: `setParameters()` with updated `KEY_BIT_RATE`
- SEI/SPS injection for IDR requests

### Phase 3 — Surface mode (future)

Surface input avoids the CPU copy for camera frames. Requires wiring
Camera2 output surface directly to encoder input surface via
`createInputSurface()`. Significant complexity; defer until ByteBuffer
mode is proven.

## Decoder design

The decoder accepts `EncodedFrame` (H.264 NAL units) and produces
`VideoFrame` (NV12 data) or, in future, `GpuFrame` via Surface output.

### Phase 1 — synchronous ByteBuffer mode

```
input: EncodedFrame { data (NAL units), pts, is_keyframe }
  → dequeueInputBuffer(timeout)
  → copy NAL data into input buffer
  → queueInputBuffer(pts, flags)
  → dequeueOutputBuffer(timeout)
  → copy NV12 frame from output buffer
  → releaseOutputBuffer(render=false)
output: VideoFrame { data: NV12, width, height, pts }
```

The decoder must handle `INFO_OUTPUT_FORMAT_CHANGED` to detect resolution
changes (adaptive streaming). SPS/PPS are passed as `csd-0` / `csd-1`
in the `AMediaFormat` at configuration time, matching the pattern used by
the openh264 and VAAPI decoders.

### Phase 2 — Surface output (future)

Render directly to a `SurfaceTexture` backed by `AHardwareBuffer`, then
import into wgpu via the `AHardwareBuffer` Vulkan extension
(`VK_ANDROID_external_memory_android_hardware_buffer`). This is the
Android equivalent of the DMA-BUF zero-copy path on Linux.

## Camera capture

Camera2 is a Java-framework API. The recommended approach:

1. Kotlin/Java helper class that opens the camera, creates a capture
   session, and writes frames to an `ImageReader` (YUV_420_888).
2. JNI callback from `ImageReader.OnImageAvailableListener` into Rust
   via a function pointer registered through `jni-rs`.
3. Rust side receives `AImage` pointer, extracts plane data, wraps as
   `VideoFrame`.

Alternative: `ndk-media`'s `AImageReader` works purely from NDK (no JNI)
but only on API 24+. If minimum API is raised to 24, this is cleaner.

## Screen capture

`MediaProjection` requires an Android `Activity` context and user
permission grant via `startActivityForResult`. This is inherently a
Java/Kotlin concern. The Rust library receives frames the same way as
camera — via `ImageReader` or `VirtualDisplay` → `Surface`.

Screen capture is lower priority than camera and codec work.

## Minimum API level

- API 21 (Android 5.0): `AMediaCodec` NDK functions available
- API 24 (Android 7.0): `AImageReader` NDK functions available
- API 26 (Android 8.0): `AHardwareBuffer` for zero-copy GPU import

Recommended minimum: API 21 for codec, API 24 for NDK camera capture.

## Testing strategy

- Unit tests: mock `AMediaCodec` responses (format changes, timeouts)
- Integration tests: require Android emulator or device in CI
  (`cargo ndk` + `adb` test runner)
- Codec conformance: encode→decode round-trip with known test vectors

## Phased rollout

| Phase | Scope | Effort |
|-------|-------|--------|
| 1 (MVP) | ByteBuffer encoder + decoder, I420/NV12 color | Medium |
| 2 (Hardening) | Color negotiation, adaptive bitrate, error recovery | Medium |
| 3 (Optimization) | Surface mode encode, AHardwareBuffer decode→wgpu | Large |
| 4 (Capture) | Camera2 via JNI, screen via MediaProjection | Medium |
