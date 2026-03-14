# Android Demo App

Status: planned. No code exists.

## Goal

A minimal Android application that demonstrates iroh-live video calling
on Android. Kotlin UI with JNI bridge to the Rust library.

## Architecture

```
android-demo/
├── app/
│   ├── src/main/
│   │   ├── java/com/n0/irohlive/demo/
│   │   │   ├── MainActivity.kt        # UI, permissions, lifecycle
│   │   │   ├── IrohBridge.kt          # JNI declarations
│   │   │   └── CameraHelper.kt        # Camera2 session management
│   │   ├── res/layout/
│   │   │   └── activity_main.xml       # SurfaceView + controls
│   │   └── AndroidManifest.xml
│   └── build.gradle.kts
├── rust/
│   ├── Cargo.toml                      # cdylib crate depending on iroh-live
│   └── src/lib.rs                      # JNI exports via jni-rs
├── build.gradle.kts                    # project-level, cargo-ndk plugin
└── gradle/
    └── libs.versions.toml
```

## JNI bridge

The Rust side exposes a small set of `#[no_mangle] extern "system"` functions:

```rust
// rust/src/lib.rs
use jni::JNIEnv;
use jni::objects::{JClass, JString};

#[no_mangle]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_connect(
    mut env: JNIEnv,
    _class: JClass,
    ticket: JString,
) -> jlong {
    // Parse ticket, create Endpoint, subscribe to broadcast.
    // Return pointer to session handle as jlong.
}

#[no_mangle]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_nextFrame(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    buffer: JByteBuffer,
) -> jboolean {
    // Poll VideoTrack::current_frame(), copy NV12 into direct ByteBuffer.
    // Return true if a new frame was written.
}

#[no_mangle]
pub extern "system" fn Java_com_n0_irohlive_demo_IrohBridge_disconnect(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    // Drop session handle, close endpoint.
}
```

Kotlin side:

```kotlin
// IrohBridge.kt
object IrohBridge {
    init { System.loadLibrary("iroh_live_android") }

    external fun connect(ticket: String): Long
    external fun nextFrame(handle: Long, buffer: ByteBuffer): Boolean
    external fun disconnect(handle: Long)
}
```

## Rendering

Two options, in order of preference:

1. **wgpu on Android**: Use wgpu with Vulkan backend. The `SurfaceView`
   provides an `ANativeWindow` that wgpu can target. The NV12→RGBA shader
   from `WgpuVideoRenderer` works unchanged. This gives the best
   performance and code sharing with the desktop viewer.

2. **GLSurfaceView fallback**: If wgpu proves difficult on older devices,
   render NV12 via a simple OpenGL ES 2.0 shader on a `GLSurfaceView`.
   Less code sharing but wider device support.

## Build system

- `cargo-ndk` plugin for Gradle builds the Rust cdylib for target ABIs
  (`arm64-v8a`, `armeabi-v7a`, `x86_64` for emulator).
- CI: GitHub Actions with `setup-android` action + Rust cross-compilation
  toolchains (`aarch64-linux-android`, etc.).
- Minimum SDK: 21 (Android 5.0) for `AMediaCodec` NDK availability.
- Target SDK: 34 (Android 14) for Play Store compliance.

## Permissions

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.CAMERA" />
<uses-permission android:name="android.permission.RECORD_AUDIO" />
```

Camera and microphone permissions requested at runtime via
`ActivityResultContracts.RequestMultiplePermissions`.

## Lifecycle

- `onResume`: connect to broadcast (or resume capture)
- `onPause`: pause capture, keep connection alive
- `onDestroy`: disconnect, drop Rust session handle
- Tokio runtime created once in `Application.onCreate()` and shared
  via a singleton. The runtime must not be created on the UI thread.

## Phased implementation

| Phase | Scope |
|-------|-------|
| 1 | Subscribe-only viewer: connect via ticket, decode H.264 (software), render via wgpu |
| 2 | Add hardware decode via MediaCodec, camera publish via Camera2+JNI |
| 3 | Bidirectional call: publish + subscribe in same session |
