# MediaCodec on Android

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | rusty-codecs, moq-media-android |
| Platforms | Android (API 26+) |

## NDK API

The encoder and decoder use the NDK `AMediaCodec` API via the `ndk` crate (0.9, api-level-26). The `android` feature flag gates all Android-specific code in rusty-codecs.

## Decoder paths

Two decoder implementations exist, optimized for different use cases:

**ByteBuffer mode** (`AndroidDecoder`): dequeues output buffers from MediaCodec and copies NV12 pixel data to CPU memory. Works on all devices but involves a CPU copy per frame. Used as the fallback when the zero-copy path is unavailable.

**ImageReader mode** (`AndroidHwDecoder`): configures the decoder with an `ImageReader` surface as the output target. Decoded frames arrive as `HardwareBuffer` references that can be imported into EGL or Vulkan without copying. This is the preferred path when GPU rendering is available.

`DynamicVideoDecoder` tries `AndroidHwDecoder` first, then falls back to `AndroidDecoder` if the hardware decoder fails to initialize.

## Zero-copy rendering

The `NativeFrameHandle::HardwareBuffer` variant carries the `HardwareBufferRef` across the decode-render boundary. On the Kotlin side, the rendering pipeline imports the buffer as an EGL external texture:

1. `eglGetNativeClientBufferANDROID()` converts the `AHardwareBuffer` to an `EGLClientBuffer`.
2. `eglCreateImageKHR()` creates an `EGLImage` from the client buffer.
3. `glEGLImageTargetTexture2DOES()` binds the image to a `GL_TEXTURE_EXTERNAL_OES` texture.

The EGL extension function pointers are not available at link time; they are resolved at runtime via `dlopen` on `libEGL.so` and `libGLESv2.so`.

## Camera capture

CameraX provides camera frames as NV12 byte arrays delivered to Rust via JNI (`pushCameraNv12`). The JNI bridge copies the NV12 data from the Java `ByteBuffer` into a Rust-owned buffer. The `CameraFrameSource` in `moq-media-android` wraps this as a push-based `VideoSource`.

## SDK crate

`moq-media-android` packages reusable Android integration code:

- **`CameraFrameSource`**: wraps CameraX NV12 delivery as a `VideoSource` with push semantics. The Java side calls `pushCameraNv12` via JNI; the Rust side buffers the frame for the encoder thread to pull.
- **EGL helpers**: function pointer resolution, `HardwareBuffer` to texture import, EGL context management.
- **`AndroidRenderer`**: manages the full EGL lifecycle (display, context, surface creation and teardown).
- **JNI handle utilities**: safe reference counting for JNI global/local references, preventing leaks across the JNI boundary.
- **Logcat tracing subscriber**: routes Rust `tracing` output to Android's logcat.

## Demo app

`android-demo/` contains a Kotlin app with a Rust JNI bridge. It supports subscribe-only viewing, camera publishing with hardware encode, and bidirectional calls (Android to desktop). Built with `cargo-ndk` targeting `aarch64-linux-android` (API 26+).

The app has been tested end-to-end on real Android hardware with bidirectional video and audio calls between an Android device and a Linux desktop.

## Open areas

**Surface mode encoder**: the current encoder accepts CPU NV12 buffers, requiring a round-trip from camera NV12 through RGBA conversion (for scaling) back to NV12. A Surface mode encoder would accept camera frames directly, eliminating the CPU conversion.

**`SurfaceControl` rendering**: bypasses EGL entirely by handing the `HardwareBuffer` to the system compositor via `ASurfaceControl`. Eliminates one composition pass and reduces latency.

**Android AEC**: the sonora AEC (used on desktop) has not been tested on Android. Android's built-in AEC via `AudioEffect` requires JNI calls from the audio callback thread, which conflicts with real-time constraints.
