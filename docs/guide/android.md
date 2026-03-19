# Android

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | iroh-live, moq-media, moq-media-android |
| Platforms | Android (arm64-v8a, minSdk 26) |

The `demos/android` directory contains a full Kotlin/Rust Android app with bidirectional video and audio calling. It captures from the device camera with CameraX, encodes with MediaCodec hardware H.264, sends and receives through iroh-live sessions, and renders incoming video with zero-copy EGL `AHardwareBuffer` import.

## Prerequisites

- `ANDROID_HOME` pointing at the Android SDK (e.g., `~/Android/Sdk`)
- Android NDK 28+ (install via Android Studio SDK Manager or `sdkmanager`)
- Rust toolchain with the Android target:
  ```sh
  rustup target add aarch64-linux-android
  ```
- [cargo-ndk](https://github.com/niclas-van-eyk/cargo-ndk-rs) and [cargo-make](https://github.com/sagiegurari/cargo-make):
  ```sh
  cargo install cargo-ndk cargo-make
  ```
- JDK 17+ (for Gradle)

## Quick start

With a device connected via USB:

```sh
cd demos/android
export ANDROID_HOME=~/Android/Sdk
cargo make install     # builds everything and installs the APK
cargo make logcat      # stream filtered logs in another terminal
```

## Build tasks

All tasks auto-detect the NDK path from `$ANDROID_HOME/ndk/` and pick the highest installed version. See `Makefile.toml` for the full list.

| Task | Description |
|------|-------------|
| `cargo make apk` | Full pipeline: cargo-ndk, clean, strip, Gradle APK |
| `cargo make install` | Build and install on connected device |
| `cargo make logcat` | Stream logs filtered to app tags |
| `cargo make logcat-pid` | Stream all logs for the running app PID |
| `cargo make ndk-build` | Build the Rust `.so` only |
| `cargo make strip` | Strip debug symbols from native lib |

## Architecture

```
demos/android/
  Makefile.toml  # cargo-make build pipeline
  rust/          # Rust JNI bridge crate (iroh-live-android)
    src/lib.rs   # JNI entry points, camera frame source, session handle
  app/           # Kotlin Android app
    src/main/java/com/n0/irohlive/demo/
      MainActivity.kt   # UI and lifecycle
      IrohBridge.kt     # Kotlin-side JNI declarations
      CameraHelper.kt   # Camera2 camera capture
```

The Kotlin side captures camera frames via Camera2 and pushes them into the Rust layer through JNI. The Rust side uses `moq-media` to encode with Android MediaCodec H.264 and publish through iroh-live sessions. Incoming video is decoded and rendered via EGL with `AHardwareBuffer` zero-copy import.

The `moq-media-android` crate provides reusable components for Android integration: camera frame sources, EGL rendering, and MediaCodec bindings. The demo app's JNI bridge crate (`iroh-live-android`) builds on top of it.

## Feature configuration

The Rust crate builds with H.264 and Opus only. AV1 and desktop capture backends are disabled. This is controlled by `demos/android/rust/Cargo.toml`:

- `moq-media`: `default-features = false, features = ["h264", "opus", "android"]`
- `rusty-codecs`: `default-features = false, features = ["h264", "opus", "hang", "android"]`
- `iroh-live`: `default-features = false` (no AV1, no capture backends)

## Debugging

```sh
export ADB=$ANDROID_HOME/platform-tools/adb

# Filtered to relevant tags (Rust tracing, JNI bridge, crashes):
$ADB logcat "iroh_live:V" "IrohBridge:V" "AndroidRuntime:E" "System.err:W" "*:S"

# All logs for the running app process:
$ADB logcat --pid=$($ADB shell pidof -s com.n0.irohlive.demo)

# Clear old logs first:
$ADB logcat -c
```

Key log tags:

- `iroh_live`: Rust-side `tracing` output
- `IrohBridge`: Kotlin JNI bridge
- `AndroidRuntime`: Java/Kotlin crash stack traces

## Requirements

- **minSdk 26** (Android 8.0), required for AAudio
- **targetSdk 34**, **compileSdk 35**
- arm64-v8a only (x86_64 is not currently built)

## Status

Tested end-to-end on a real Android device with bidirectional video and audio between Android and Linux desktop. See `plans/platforms.md` for the full platform support matrix.
