# moq-media-android

Android-specific integration for moq-media: camera frame source, EGL/GLES rendering, and JNI helpers.

This crate provides the glue between Android's Java/Kotlin APIs and the Rust media pipeline. It is used by the [Android demo app](../demos/android/) but can be used independently in any Android Rust project.

## Modules

### `camera`

`CameraFrameSource` is a push-based `VideoSource`. Android camera callbacks (from CameraX or Camera2) push NV12 frames into the source via JNI, and the encoder thread pulls the latest frame when ready. `SharedCameraSource` wraps it in `Arc<Mutex<_>>` for safe cross-thread JNI access.

### `egl`

Safe wrappers around EGL/GLES extension function pointers for zero-copy `AHardwareBuffer` rendering. The three functions involved are `eglGetNativeClientBufferANDROID` (get EGL client buffer from `AHardwareBuffer`), `eglCreateImageKHR` (create EGL image), and `glEGLImageTargetTexture2DOES` (bind to `GL_TEXTURE_EXTERNAL_OES`). Function pointers are resolved at runtime via `dlopen(libEGL.so)` and `eglGetProcAddress`.

### `renderer`

`AndroidRenderer` manages the full EGL lifecycle (display, context, surface) and renders frames with GLES2 shaders. Supports both `GL_TEXTURE_EXTERNAL_OES` (hardware buffer) and NV12 CPU paths, with sensor rotation handling.

### `handle`

JNI pointer arithmetic helpers for passing `Arc<Mutex<T>>` across the JNI boundary as `jlong` values.
