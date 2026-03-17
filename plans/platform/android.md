# Android platform

Hardware-accelerated H.264 via Android MediaCodec, camera capture via CameraX/JNI,
and a reusable `moq-media-android` SDK crate. Tested on real devices with
bidirectional video/audio calls (Android to desktop).

## Status

- [x] ByteBuffer H.264 encoder (`AndroidEncoder`)
- [x] ByteBuffer H.264 decoder (`AndroidDecoder`)
- [x] ImageReader HW decoder with `HardwareBuffer` output (`AndroidHwDecoder`)
- [x] CameraX NV12 capture via JNI
- [x] Demo app: bidirectional call mode (dial, camera+mic publish, remote A/V playback)
- [x] EGL `HardwareBuffer` to texture rendering pipeline
- [x] `moq-media-android` SDK crate (camera source, EGL helpers, logcat, JNI handle utils)
- [x] End-to-end device testing (Android to Linux desktop)
- [ ] Surface mode for zero-copy encode (currently CPU NV12 to RGBA to scale to NV12 round-trip)
- [ ] `SurfaceControl` rendering (bypass EGL, direct HardwareBuffer display)
- [ ] Audio echo cancellation on Android

## Details

### Codec integration

The encoder and decoder use the NDK `AMediaCodec` API via the `ndk` crate (0.9,
`api-level-26`). The `android` feature flag gates all Android-specific code in
`rusty-codecs`.

Two decoder paths exist:

- **ByteBuffer mode** (`AndroidDecoder`): dequeues output buffers and copies NV12 to
  CPU memory. Works everywhere but involves a CPU copy per frame.
- **ImageReader mode** (`AndroidHwDecoder`): configures the decoder with an
  `ImageReader` surface. Decoded frames arrive as `HardwareBuffer` references that
  can be imported into EGL/Vulkan without copying. Preferred when the GPU rendering
  path is available.

### Zero-copy rendering

The `NativeFrameHandle::HardwareBuffer` variant in `rusty-codecs` carries the
`HardwareBufferRef` across the decode-render boundary. On the Kotlin side, the
JNI bridge exposes `nextHardwareBuffer` instead of pixel arrays. The Kotlin
renderer imports the buffer via `EGL_ANDROID_image_hardwarebuffer` and binds it
as a GL external texture.

Future work: `SurfaceControl` rendering bypasses EGL entirely by handing the
`HardwareBuffer` to the system compositor. This eliminates the GL import step
and reduces latency by one composition pass.

### Demo app

`android-demo/` contains a Kotlin app with a Rust JNI bridge. The app supports
subscribe-only viewing, camera publishing with hardware encode, and bidirectional
calls. Built with `cargo-ndk` targeting `aarch64-linux-android` (API 26+).

### SDK crate

`moq-media-android` packages reusable Android integration code:

- Camera source wrapping CameraX with NV12 output
- EGL helpers for `HardwareBuffer` texture import
- Logcat tracing subscriber
- JNI handle utilities for safe reference counting

## Implementation history

- `ab45135` — initial MediaCodec encoder and decoder
- `676bb5c` — ImageReader HW decoder, encoder scaling, NV12 `rgba_image()`
- `cdd9831` — demo app with JNI bridge and Kotlin UI
- `b8789e1` — Android call mode with camera publishing
- `917e1d9` — extract `moq-media-android` SDK crate
