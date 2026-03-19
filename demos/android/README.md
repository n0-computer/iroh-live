# iroh-live Android Demo

Bidirectional video and audio calling on Android. Captures from the device camera with CameraX, encodes with MediaCodec hardware H.264, sends and receives through iroh-live sessions, and renders incoming video with zero-copy EGL `AHardwareBuffer` import.

## Prerequisites

- `ANDROID_HOME` pointing at the Android SDK (e.g. `~/Android/Sdk`)
- Android NDK 28+ (install via Android Studio SDK Manager or `sdkmanager`)
- Rust toolchain with the Android target:
  ```sh
  rustup target add aarch64-linux-android
  ```
- [cargo-ndk](https://github.com/niclas-van-eyk/cargo-ndk-rs) and
  [cargo-make](https://github.com/sagiegurari/cargo-make):
  ```sh
  cargo install cargo-ndk cargo-make
  ```
- JDK 17+ (for Gradle)

## Quick start

From the `demos/android/` directory with a device connected via USB:

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
| `cargo make apk` | Full pipeline: cargo-ndk -> clean -> strip -> Gradle APK |
| `cargo make install` | Build + install on connected device |
| `cargo make logcat` | Stream logs filtered to app tags |
| `cargo make logcat-pid` | Stream all logs for the running app PID |
| `cargo make ndk-build` | Build the Rust `.so` only |
| `cargo make clean-jni` | Remove extra `.so` files from jniLibs |
| `cargo make strip` | Strip debug symbols from native lib |

### Manual build (without cargo-make)

```sh
# 1. Build native library (API 26 for AAudio, libc++_shared for C++ runtime)
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.0.12674087  # adjust version
cargo ndk -t arm64-v8a -P 26 --link-libcxx-shared \
  -o demos/android/app/src/main/jniLibs \
  build -p iroh-live-android --release

# 2. Remove extra .so files cargo-ndk copies from dependencies
find demos/android/app/src/main/jniLibs -name "*.so" \
  ! -name "libiroh_live_android.so" ! -name "libc++_shared.so" -delete

# 3. Strip debug symbols (~500 MB -> ~21 MB)
$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip \
  demos/android/app/src/main/jniLibs/arm64-v8a/libiroh_live_android.so

# 4. Build APK
cd demos/android
ANDROID_HOME=~/Android/Sdk ./gradlew assembleDebug

# 5. Install
$ANDROID_HOME/platform-tools/adb install -r app/build/outputs/apk/debug/app-debug.apk
```

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
- `iroh_live` --Rust-side `tracing` output
- `IrohBridge` --Kotlin JNI bridge
- `AndroidRuntime` --Java/Kotlin crash stack traces

## Feature configuration

The Rust crate builds with H.264 + Opus only (no AV1, no desktop capture).
This is controlled by `demos/android/rust/Cargo.toml`:

- `moq-media`: `default-features = false, features = ["h264", "opus", "android"]`
- `rusty-codecs`: `default-features = false, features = ["h264", "opus", "hang", "android"]`
- `iroh-live`: `default-features = false` (no AV1, no capture backends)

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

The Kotlin side captures camera frames via Camera2 and pushes them into the Rust layer through JNI. The Rust side uses `moq-media` to encode with Android MediaCodec H.264 and publish through `iroh-live` sessions.

## Requirements

- **minSdk 26** (Android 8.0) --required for AAudio (audio backend)
- **targetSdk 34**, **compileSdk 35**
- arm64-v8a only (x86_64 not currently built)

## Status

Tested end-to-end on a real Android device with bidirectional video and audio between Android and Linux desktop. See [`plans/platforms.md`](../../plans/platforms.md) for the full platform support matrix.
