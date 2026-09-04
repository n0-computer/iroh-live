# Android

`demos/android` is a Kotlin application with a Rust core that makes two-way
calls: Camera2 capture, hardware H.264 through MediaCodec, iroh transport, and
zero-copy rendering of the decoded frames through EGL.

## Where the pieces live

The MediaCodec encoder and decoder are upstream in `moq-video`, behind
`cfg(target_os = "android")` alongside the objc2 and Windows backend families.
They were ported out of this repository during the v2 rewrite. Backend selection
finds them automatically, and `moq_video::encode::Kind::Named("mediacodec")` asks
for one by name. Nothing in this repository implements a codec.

`moq-media-android` carries the two things that are not a moq-video concern.
`camera(size)` returns a `CameraSink` and a `VideoSource::Frames`: Kotlin pushes
NV12 or RGBA into the sink and the publisher reads frames out. It is a
latest-wins slot, so a newer frame replaces an unconsumed older one, which is
what a camera wants. `AndroidRenderer` owns the whole EGL lifecycle (display,
context, surface) and draws either an `AHardwareBuffer` through
`GL_TEXTURE_EXTERNAL_OES`, which is the zero-copy path out of MediaCodec's
`ImageReader`, or an NV12 buffer through two `sampler2D` units. Both apply sensor
rotation in the shader. Kotlin hands over an `android.view.Surface` and nothing
else.

`demos/android/rust` is the JNI bridge on top: one tokio runtime, one
`SessionHandle` per session passed to Kotlin as a `jlong`, and a logcat layer for
`tracing`. Its entry points cover connecting, dialling, answering, publishing,
pushing camera frames, driving the surface, listing and switching renditions, and
reading a status line. `IrohBridge.kt` declares the matching `external fun`s.

The app has three modes. **Watch** scans or pastes a ticket and plays it.
**Publish** sends this camera and microphone and shows a ticket as a QR code.
**Call** does both at once against one peer, and is the mode with two ways in:
scan the other device's code to dial it, or show a code of your own and wait.

Two of them are offline: `startDirect` runs camera to preview with no encode and
no network, and `startH264` runs camera to MediaCodec to a local loopback
broadcast and back, which smoke-tests the codec on a device without needing a
peer. Both exist because "is it the codec or is it the network" is the first
question when a device misbehaves.

A call uses `iroh_live::Call`, so each peer publishes under `calls/<its own
endpoint id>` and subscribes to the other's. Which side dialed stops mattering
once the session is up.

`dial` blocks until the call is established, because the caller already has
somewhere to connect to. `answer` cannot: the code has to be on screen before
any peer exists, so it returns as soon as this node's own side is published and
leaves a task on the handle waiting for the first inbound session that turns out
to be a caller. `callConnected` is what the screen polls to know it happened.
Every other session arrives the same way and an ordinary subscriber never
publishes the call path, so one that does not is skipped rather than treated as
a failure.

## Prerequisites

- `ANDROID_HOME` pointing at the SDK, for example `~/Android/Sdk`
- Android NDK 28 or newer, installed through the SDK manager
- `rustup target add aarch64-linux-android`
- `cargo install cargo-ndk cargo-make`
- JDK 17 or newer for Gradle

The app is `minSdk 26`, `targetSdk 34`, `compileSdk 35`, and builds `arm64-v8a`
only.

## Building

Run these from `demos/android`. The NDK path is detected from
`$ANDROID_HOME/ndk/` and the highest installed version wins.

```sh
export ANDROID_HOME=~/Android/Sdk
cargo make install     # build everything and install the APK
cargo make logcat      # filtered logs, in another terminal
```

The full task list is in `demos/android/Makefile.toml`. The ones worth knowing:
`ndk-build` builds only the Rust `.so`, `strip` removes its debug symbols, `apk`
runs the whole pipeline through Gradle, `install` adds the install step,
`run-on-device` launches it, and the `-release` variants of each do the same
against the release profile. `logcat-pid` follows every log line from the running
process rather than filtering by tag.

## Feature configuration

`demos/android/rust/Cargo.toml` selects `moq-media` with the `aec` feature, which
implies `capture` and `playback`. A handset on speakerphone without echo
cancellation publishes its own output back to the peer, which is the one audio
failure everybody notices.

Video is pushed from Kotlin, so the camera never goes through `moq_video::capture`.
Audio is not: the Rust side opens the microphone itself through
`moq_audio::capture`.

## Debugging

```sh
export ADB=$ANDROID_HOME/platform-tools/adb

# Rust tracing, the JNI bridge, and crashes
$ADB logcat "iroh_live:V" "IrohBridge:V" "AndroidRuntime:E" "System.err:W" "*:S"

# Everything from the running process
$ADB logcat --pid=$($ADB shell pidof -s com.n0.irohlive.demo)
```

`iroh_live` is the Rust `tracing` output, `IrohBridge` is the Kotlin side, and
`AndroidRuntime` carries Java and Kotlin stack traces. The Rust filter defaults
to `warn`, with the iroh, moq, and audio crates at `debug`.

## Status

Tested on device with two-way video and audio between an Android handset and a
Linux desktop.
