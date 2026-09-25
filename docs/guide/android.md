# Android

`demos/android` is a Kotlin app with a Rust core. It captures with CameraX,
encodes and decodes H.264 with MediaCodec, sends over iroh, and draws decoded
frames through EGL without a copy.

## Where the pieces live

The MediaCodec encoder and decoder are in `moq-video`. Automatic backend
selection finds them, and `moq_video::encode::Kind::Named("mediacodec")` asks
for one by name.

`iroh-live-media-android` has the parts that are specific to the app side:

- `renderer::AndroidRenderer` owns the EGL display, context, and surface.
  `render_hardware_buffer` draws an `AHardwareBuffer` through
  `GL_TEXTURE_EXTERNAL_OES`, which is the zero-copy path out of MediaCodec.
  `render_nv12` draws an NV12 buffer. Both apply the sensor rotation in the
  shader.

`demos/android/rust` is the JNI bridge. It runs one tokio runtime, passes one
`SessionHandle` per session to Kotlin as a `jlong`, and sends `tracing` output
to logcat. `IrohBridge.kt` declares the matching `external fun`s.

## Modes

**Watch** plays a scanned or pasted ticket. **Publish** sends the camera and
microphone and shows the ticket as a QR code. **Call** does both with one peer:
scan the other device's code to dial it, or show your own and wait.

Two entry points need no network. `startDirect` shows the camera without
encoding. `startH264` sends the camera through MediaCodec to a local broadcast
and back, which tests the codec without a peer.

A call follows the same convention as `irl call`, so a phone and a desktop can
call each other. Each side offers its broadcast as `iroh_live::CALL` to the
other peer only, and subscribes to the other's. `dial` offers this side to the
ticket's peer and returns once the peer answers. `answer` opens this side and
returns, so the QR code can go on screen. A task then waits for a peer's `call`
path to appear in the route table, and answers it. The screen polls
`callConnected` to learn when a caller arrived.

## Prerequisites

- `ANDROID_HOME` pointing at the SDK, for example `~/Android/Sdk`
- Android NDK 28 or newer, installed through the SDK manager
- `rustup target add aarch64-linux-android`
- `cargo install cargo-ndk cargo-make`
- JDK 17 or newer for Gradle

The app is `minSdk 26`, `targetSdk 34`, `compileSdk 35`. It packages
`arm64-v8a` and `x86_64` libraries, whichever were built.

## Building

Run these from `demos/android`. The highest NDK version under
`$ANDROID_HOME/ndk/` is used.

```sh
export ANDROID_HOME=~/Android/Sdk
cargo make install     # build everything and install the APK
cargo make logcat      # filtered logs, in another terminal
```

The build is for `arm64-v8a`. Set `ABI=x86_64` to build for the emulator.
`demos/android/Makefile.toml` lists every task. `ndk-build` builds only the
Rust library, `apk` runs the whole build, `install` also installs it, and
`run-on-device` launches it and follows the logs. The `-release` variants use
the release Gradle build. `logcat-pid` shows every log line of the running
process.

## Features and audio

`demos/android/rust/Cargo.toml` enables the `aec` feature of `iroh-live`,
which implies `capture` and `playback`. Without echo cancellation, a phone on
speaker sends its own output back to the peer.

Video comes from Kotlin, but the Rust side opens the microphone with
`AudioSource::microphone`. It opens one `AudioOutput` for the speaker and
passes it to every player. In a call it also sets that output as
`MicrophoneConfig::echo_reference`, so the canceller knows what to remove.

## Debugging

```sh
export ADB=$ANDROID_HOME/platform-tools/adb

# Rust tracing, the JNI bridge, and crashes
$ADB logcat "iroh_live:V" "IrohBridge:V" "AndroidRuntime:E" "System.err:W" "*:S"

# Everything from the running process
$ADB logcat --pid=$($ADB shell pidof -s com.n0.irohlive.demo)
```

`iroh_live` is the Rust `tracing` output, `IrohBridge` is the Kotlin side, and
`AndroidRuntime` has Java and Kotlin stack traces. The Rust filter defaults to
`warn`, with the iroh, moq, media, and audio crates at `debug`.

## Status

Tested on a handset with two-way audio and video to a Linux desktop.
