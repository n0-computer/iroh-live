# iroh-live

Real-time video and audio over [iroh](https://github.com/n0-computer/iroh), using [Media over QUIC](https://moq.dev/) for transport.

iroh-live handles the full pipeline from camera and screen capture through encoding, QUIC transport, decoding, and rendering. It works peer-to-peer by default and bridges to browsers through an optional relay server. No media servers required.

## Quick start

Start a video call in two terminals:

```sh
# Terminal 1: publish camera + mic
cargo run --release --example publish

# Terminal 2: watch the stream (paste the ticket from terminal 1)
cargo run --release --example watch -- <TICKET>
```

For a multi-user room:

```sh
# First participant creates the room
cargo run --release --example rooms

# Others join with the printed ticket
cargo run --release --example rooms -- <TICKET>
```

### System dependencies

The default build compiles all codecs from bundled source, so no system libraries are required.

For hardware-accelerated codecs and native capture on Linux, install development headers:

```sh
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev libva-dev

# Arch
sudo pacman -S pipewire libclang libva
```

Then build with all features:

```sh
cargo build --workspace --all-features
```

## How it works

`iroh-live` is the entry point for most applications. It depends on `iroh-moq` for QUIC transport and `moq-media` for the media pipeline, which in turn uses `rusty-codecs` for encoding/decoding and `rusty-capture` for camera and screen input. The egui, dioxus, and Android crates provide optional rendering integrations; `iroh-live` itself is not coupled to any GUI framework.

The transport uses Media over QUIC (MoQ), where each video rendition and audio track travels as an independent QUIC stream. A dropped packet on the video side does not block audio delivery, and keyframe boundaries let the receiver skip ahead when falling behind rather than buffering stale data.

All codec work runs on dedicated OS threads, so encoding and decoding never block the async runtime. On the publish side, you point a `LocalBroadcast` at a camera or screen source and it handles encoding, packetization, and catalog management. On the subscribe side, a `RemoteBroadcast` spawns decoder threads and delivers decoded frames ready for rendering.

When multiple quality renditions are available, `AdaptiveVideoTrack` switches between them automatically based on bandwidth, packet loss, and RTT (partially implemented: the selection algorithm works, but seamless switching without visual glitches is not yet wired up). `PlayoutClock` synchronizes audio and video, measuring jitter and skipping late frames to maintain a target latency.

## Crates

| Crate | Description |
|-------|-------------|
| [`iroh-live`](iroh-live) | High-level API: sessions, calls, rooms, tickets |
| [`iroh-moq`](iroh-moq) | MoQ transport layer over iroh/QUIC |
| [`moq-media`](moq-media) | Media pipelines: capture, encode, decode, playout, adaptive bitrate |
| [`rusty-codecs`](rusty-codecs) | Video and audio codecs with hardware acceleration |
| [`rusty-capture`](rusty-capture) | Cross-platform screen and camera capture |
| [`moq-media-egui`](moq-media-egui) | egui video rendering widget |
| [`moq-media-dioxus`](moq-media-dioxus) | dioxus-native video rendering |
| [`moq-media-android`](moq-media-android) | Android camera, EGL rendering, JNI bridge |
| [`iroh-live-relay`](iroh-live-relay) | Relay server for browser WebTransport bridging |

## Platform support

Software codecs (H.264 via openh264, AV1 via rav1e/rav1d, Opus) work on every platform Rust targets. Hardware acceleration and capture depend on the OS:

| Platform | Codecs | Capture | GPU render | Status |
|----------|--------|---------|------------|--------|
| Linux (Intel/AMD) | Software + VAAPI HW | PipeWire, V4L2, X11 | Vulkan + DMA-BUF zero-copy | Tested |
| macOS | Software + VideoToolbox | ScreenCaptureKit, AVFoundation | Metal via wgpu | Compiles, untested |
| Android | Software + MediaCodec HW | CameraX via JNI | EGL + HardwareBuffer zero-copy | Tested |
| Raspberry Pi | Software + V4L2 HW encoder | V4L2 camera | Vulkan (RPi 5) | Partial |
| Windows | Software only | Not yet | DX12 via wgpu | Minimal |
| iOS | Software + VideoToolbox | AVFoundation | Metal via wgpu | Compiles, untested |

See [`plans/platforms.md`](plans/platforms.md) for the full matrix with backend details.

## Running on different devices

### Android

The [`demos/android`](demos/android) directory contains a full Kotlin/Rust Android app with bidirectional video calling. It uses CameraX for capture, MediaCodec for hardware H.264 encoding, and EGL for zero-copy rendering.

```sh
cd demos/android
rustup target add aarch64-linux-android
cargo install cargo-ndk cargo-make
cargo make install   # builds and installs on connected device
```

See the [Android demo README](demos/android/README.md) for prerequisites and build details.

### Raspberry Pi

The [`demos/pi-zero`](demos/pi-zero) directory publishes a live camera stream from a Raspberry Pi Zero 2 W and displays the connection ticket as a QR code on an e-paper HAT.

Cross-compile from your host machine:

```sh
rustup target add aarch64-unknown-linux-gnu
./demos/pi-zero/build.sh
scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@<PI_IP>:~/
```

See the [Pi Zero demo README](demos/pi-zero/README.md) for sysroot setup and hardware wiring.

### Browser via relay

Browsers cannot speak iroh's QUIC protocol directly, so the [`iroh-live-relay`](iroh-live-relay) server bridges the gap. It accepts WebTransport connections from browsers, connects to iroh publishers on demand, and forwards the media stream.

```sh
# Start the relay (self-signed certs for local dev)
cargo run -p iroh-live-relay -- --dev

# Open in browser: https://localhost:4443?name=<TICKET>
```

The relay serves a built-in web viewer that uses the `@moq/watch` web component with WebCodecs for decoding. Any iroh-live ticket can be watched in the browser this way: the relay pulls the stream from the publisher and re-serves it over WebTransport.

## Examples

All examples live in [`iroh-live/examples/`](iroh-live/examples/):

| Example | Description |
|---------|-------------|
| `publish` | Broadcast camera + mic, print a ticket for viewers |
| `watch` | egui viewer, connects to a ticket and renders video + audio |
| `watch-wgpu` | Minimal fullscreen wgpu viewer (no egui) |
| `call` | 1:1 peer-to-peer video call (dial/accept) |
| `rooms` | Multi-user room with dynamic participant discovery |
| `split` | Local split-screen: full encode, transport, decode loop on localhost |
| `push` | Stream a video file or transcode via ffmpeg |
| `room-publish-file` | Publish a video file into a room |

The [`demos/opengl`](demos/opengl) crate is a standalone GLES2 viewer using glutin/winit, without any egui or wgpu dependency. It serves as a minimal rendering reference.

## License

Copyright 2025 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
