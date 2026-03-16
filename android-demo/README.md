# iroh-live Android Demo

Minimal Android app demonstrating live video over iroh. Captures frames from
the device camera, encodes with Android MediaCodec (hardware H.264), and
sends/receives through iroh-live sessions.

## Prerequisites

- Android NDK (install via Android Studio SDK Manager or `sdkmanager`)
- Rust toolchain with Android targets:
  ```sh
  rustup target add aarch64-linux-android x86_64-linux-android
  ```
- [cargo-ndk](https://github.com/niclas-van-eyk/cargo-ndk-rs):
  ```sh
  cargo install cargo-ndk
  ```
- Android Studio (for Gradle builds and device deployment)

## Building the Rust library

Build the native `.so` for the target architectures and place it where
Gradle expects it:

```sh
cargo ndk -t arm64-v8a -t x86_64 \
  -o android-demo/app/src/main/jniLibs \
  build -p iroh-live-android --release
```

To just check that it compiles (without producing the `.so`):

```sh
cargo ndk -t aarch64-linux-android check -p iroh-live-android
```

## Building the APK

From the `android-demo/` directory:

```sh
cd android-demo
./gradlew assembleDebug
```

The debug APK is at `app/build/outputs/apk/debug/app-debug.apk`.

## Installing on a device

With a device connected via USB (or an emulator running):

```sh
adb install app/build/outputs/apk/debug/app-debug.apk
```

Or use Android Studio's Run button.

## Architecture

```
android-demo/
  rust/          # Rust JNI bridge crate (iroh-live-android)
    src/lib.rs   # JNI entry points, camera frame source, session handle
  app/           # Kotlin Android app
    src/main/java/com/n0/irohlive/demo/
      MainActivity.kt   # UI and lifecycle
      IrohBridge.kt     # Kotlin-side JNI declarations
      CameraHelper.kt   # Camera2 camera capture
```

The Kotlin side captures camera frames via Camera2 and pushes them into the
Rust layer through JNI. The Rust side uses `moq-media` to encode (Android
MediaCodec H.264) and publish through `iroh-live` sessions.

## Status

This is a skeleton demo. The Rust codec and JNI bridge compile and pass
cross-compilation checks, but the app has not been tested end-to-end on a
real device yet. See `plans/android-demo-app.md` for the full roadmap.
