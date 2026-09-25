# iroh-live Android demo

An Android app that watches a broadcast, publishes the camera and microphone,
or runs a two-way call. It captures with CameraX, encodes and decodes H.264
with MediaCodec, and draws decoded pictures through EGL `AHardwareBuffer`
import, without a copy.

## Prerequisites

- `ANDROID_HOME` pointing at the Android SDK, for example `~/Android/Sdk`.
- The Android NDK, installed through the Android Studio SDK Manager or
  `sdkmanager`. The tasks pick the newest version under `$ANDROID_HOME/ndk/`.
- JDK 17 or newer, for Gradle.
- The Rust target, [cargo-ndk](https://github.com/bbqsrc/cargo-ndk), and
  [cargo-make](https://github.com/sagiegurari/cargo-make):

  ```sh
  rustup target add aarch64-linux-android   # x86_64-linux-android for the emulator
  cargo install cargo-ndk cargo-make
  ```

## Quick start

With a device connected over USB:

```sh
cd demos/android
cargo make run-on-device
```

This builds the native library and the debug APK, installs it, starts the app,
and streams its logs.

## Tasks

Run these from `demos/android/`. [Makefile.toml](Makefile.toml) has the full
list.

| Task | What it does |
|------|-------------|
| `cargo make apk` | Builds the native library with cargo-ndk, removes the extra `.so` files, strips it, and builds the debug APK |
| `cargo make install` | `apk`, then installs it on the connected device |
| `cargo make run-on-device` | `install`, then starts the app and streams its logs |
| `cargo make apk-release`, `install-release`, `run-on-device-release` | The same with a release Gradle build |
| `cargo make logcat` | Clears the log and streams the app's tags |
| `cargo make logcat-pid` | Streams every log line of the running app |
| `cargo make ndk-build` | Builds the native library only |

The native library is built for `arm64-v8a`. Set `ABI=x86_64` to build for the
emulator, for example `ABI=x86_64 cargo make install`.

The logs use the tags `iroh_live` (Rust `tracing`), `IrohBridge` (the Kotlin
JNI bridge), `IrohLiveDemo` (the app), and `AndroidRuntime` (crashes).

## Layout

```
demos/android/
  Makefile.toml     # the build tasks
  rust/             # the JNI bridge crate, iroh-live-android
    src/lib.rs      # JNI entry points and the session handle
  app/              # the Kotlin app
    src/main/java/com/n0/irohlive/demo/
      IrohBridge.kt # the JNI declarations
      Session.kt    # camera capture and the session lifecycle
      ui/           # the Compose screens: home, watch, publish, call
```

Kotlin captures camera frames with CameraX and pushes them into Rust through
`IrohBridge.pushCameraNv12`. The Rust side publishes them with `iroh-live`, and
a call follows the same convention as `irl call`. The home screen also has two
diagnostics that need no network: camera passthrough, and an H.264 encode and
decode loop.

Codecs come from `moq-video` and `moq-audio`, which choose a backend at
runtime: MediaCodec for H.264, with openh264 in software as the fallback. The
bridge crate turns on `aec` in `iroh-live`, which brings in the
microphone and the speaker. Echo cancellation keeps a phone on speaker from
sending the peer's audio back to it.

## Requirements

- minSdk 26 (Android 8.0), for AAudio. targetSdk 34, compileSdk 35.
- arm64-v8a on a device, x86_64 on the emulator.

## Status

Tested on a device, with two-way video and audio between Android and a Linux
desktop. See [docs/guide/android.md](../../docs/guide/android.md) for how the
pieces fit together, and [docs/platforms.md](../../docs/platforms.md) for the
support matrix.
