# iroh-live

> **Early tech preview.** We are early in development and several parts remain
> unfinished. Among the most prominent gaps right now: audio and video play
> without synchronization, Windows support is mostly missing, on-device
> testing has been very limited, the room feature is functional but lightly
> tested, and the relay has no authentication yet.

Real-time audio and video over [iroh](https://github.com/n0-computer/iroh)
(QUIC), written in Rust. iroh-live handles the full pipeline from camera
capture through encoding, transport, decoding, and rendering. Connections are
peer-to-peer by default, and an optional relay server bridges to browsers via
WebTransport. The transport layer uses
[Media over QUIC (MoQ)](https://moq.dev/), where each video rendition and
audio track travels as an independent QUIC stream, so a dropped video packet
never blocks audio delivery.

## Quick start

Install the `irl` CLI and publish your camera and microphone:

```sh
cargo install --path iroh-live-cli --all-features

# Terminal 1: publish camera and mic, prints a ticket and QR code
irl publish

# Terminal 2: watch the stream
irl play <TICKET>
```

For a 1:1 call:

```sh
# Side A: wait for incoming call
irl call

# Side B: dial with the ticket from side A
irl call <TICKET>
```

To watch a stream in a browser, start a relay and open the URL it prints:

```sh
# Terminal 1: start a local relay (self-signed TLS, dev mode)
irl relay

# Terminal 2: publish to the relay
irl publish --relay <RELAY_ADDR>

# Open http://localhost:4443/?name=hello in a browser to watch
```

For a multi-party room (early stage, limited testing):

```sh
# First participant creates the room
irl room

# Others join with the printed ticket
irl room <TICKET>
```

## Using iroh-live in Rust

The `iroh-live` crate provides the high-level API. A minimal publisher that
captures camera and microphone, encodes them, and prints a ticket:

```rust
let live = Live::from_env().await?.with_router().spawn();
let broadcast = LocalBroadcast::new();

let camera = CameraCapturer::new()?;
broadcast.video().set_source(camera, VideoCodec::H264, [VideoPreset::P720])?;

let audio = AudioBackend::default();
let mic = audio.default_input().await?;
broadcast.audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

live.publish("hello", &broadcast).await?;
let ticket = LiveTicket::new(live.endpoint().addr(), "hello");
println!("{ticket}");
```

On the subscribe side, connect with a ticket and decode video frames:

```rust
let live = Live::from_env().await?.spawn();
let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name).await?;

let audio = AudioBackend::default();
let tracks = sub.media(&audio, Default::default()).await?;

if let Some(mut video) = tracks.video {
    while let Some(frame) = video.next_frame().await {
        // render the frame
        println!("frame {}x{}", frame.dimensions[0], frame.dimensions[1]);
    }
}
```

Both snippets are extracted from a compilable example at
[`examples/demo.rs`](iroh-live/examples/demo.rs).

Details: [docs/guide/desktop.md](docs/guide/desktop.md)

## Platform support

Software codecs (H.264 via openh264, AV1 via rav1e/rav1d, Opus) work on every
platform Rust targets. Hardware acceleration and capture depend on the OS:

| Platform | Codecs | Capture | GPU render | Status |
|----------|--------|---------|------------|--------|
| Linux (Intel/AMD) | Software and VAAPI HW | PipeWire, V4L2, X11 | Vulkan and DMA-BUF zero-copy | Mostly working |
| macOS | Software and VideoToolbox | ScreenCaptureKit, AVFoundation | Metal via wgpu | Mostly working |
| Android | Software and MediaCodec HW | CameraX via JNI | EGL and HardwareBuffer zero-copy | Mostly working |
| Raspberry Pi | Software and V4L2 HW | V4L2 camera | Vulkan (RPi 5) | Mostly working |
| Windows | Software only | Not yet implemented | DX12 via wgpu | Missing |
| iOS | Software and VideoToolbox | AVFoundation | Metal via wgpu | Compiles, untested |

Details: [docs/platforms.md](docs/platforms.md)

## CLI tool (`irl`)

The `irl` binary is in the `iroh-live-cli` crate. It is a demo application
that showcases iroh-live's capabilities, using egui for its GUI. It covers
the full workflow from device discovery through publishing, playback, calls,
rooms, and running a relay server.

| Command | Description |
|---------|-------------|
| `irl devices` | List cameras, screens, audio devices, and codecs |
| `irl publish` | Broadcast camera and microphone (default) |
| `irl publish file <FILE>` | Stream a media file |
| `irl play <TICKET>` | Subscribe and render a remote broadcast |
| `irl call [TICKET]` | 1:1 bidirectional video call |
| `irl room [TICKET]` | Multi-party room with participant grid |
| `irl relay` | Start a local relay server for browser access |

Common flags for `publish`: `--video cam`, `--video screen`, `--test-source`,
`--codec av1`, `--video-presets 360p,720p`, `--relay <endpoint>`,
`--room <ticket>`.

Details: [docs/cli.md](docs/cli.md)

## Demos

These demos are experimental and incomplete previews of what iroh-live can do.

- **`demos/android/`** — Kotlin and Rust Android app with bidirectional video calling, rendered with EGL ([README](demos/android/README.md)).
- **`demos/pi-zero/`** — Raspberry Pi Zero 2 W camera stream with e-paper QR code ([README](demos/pi-zero/README.md)).
- **`demos/opengl/`** — Minimal GLES2 viewer using glutin and winit, no egui or wgpu.
- **`moq-media/examples/viewer.rs`** — Standalone egui video viewer.
- **`iroh-live/examples/split.rs`** — Local split-screen with full encode, transport, and decode loop on localhost, rendered with egui.

## Workspace crates

| Crate | Description |
|-------|-------------|
| [`iroh-live`](iroh-live) | High-level API: live sessions, rooms, tickets. Depends on `moq-media` and `iroh`. |
| [`iroh-moq`](iroh-moq) | MoQ transport layer over iroh/quinn via `web-transport-iroh`. |
| [`moq-media`](moq-media) | Media pipelines: capture, encode, decode, publish, subscribe, playout, adaptive bitrate. No iroh dependency. |
| [`rusty-codecs`](rusty-codecs) | Codec implementations (H.264, AV1, Opus), VAAPI/V4L2/VideoToolbox HW accel, and wgpu rendering. |
| [`rusty-capture`](rusty-capture) | Cross-platform screen and camera capture: PipeWire, V4L2, X11, Apple ScreenCaptureKit/AVFoundation. |
| [`moq-media-egui`](moq-media-egui) | egui integration for video rendering (wgpu texture upload, DMA-BUF import). |
| [`moq-media-dioxus`](moq-media-dioxus) | dioxus-native integration for video rendering. |
| [`moq-media-android`](moq-media-android) | Android camera, EGL rendering, JNI bridge. |
| [`iroh-live-relay`](iroh-live-relay) | Relay server for browser WebTransport bridging (no auth yet). |
| [`iroh-live-cli`](iroh-live-cli) | CLI tool for publishing, playing, calls, and rooms. |

## Building

The default build compiles all codecs from bundled source, with no system
libraries required:

```sh
cargo build --workspace
```

For hardware-accelerated codecs and native capture on Linux, install development
headers:

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

### Feature flags

The workspace uses many feature flags for codecs, hardware backends, and capture
sources. Most are enabled by default in the respective crates. The table below
shows the flags relevant to `iroh-live` (which delegates to `moq-media` and
`rusty-codecs`):

| Flag | What it enables | Default |
|------|-----------------|---------|
| `h264` | H.264 codec (openh264) | Yes |
| `opus` | Opus audio codec | Yes |
| `av1` | AV1 codec (rav1e encoder, rav1d decoder) | No |
| `capture` | Camera and screen capture | Yes |
| `vaapi` | VAAPI hardware encode and decode (Linux) | Yes |
| `videotoolbox` | VideoToolbox hardware codecs (macOS) | Yes |
| `wgpu` | GPU rendering via wgpu | Yes |
| `dmabuf-import` | Zero-copy VAAPI decode to Vulkan render (Linux) | Yes |
| `metal-import` | Zero-copy VideoToolbox decode to Metal render (macOS) | Yes |
| `v4l2` | V4L2 hardware codecs (Raspberry Pi, embedded Linux) | No |

## Contributing

Open items and known issues are tracked in the
[issue tracker](https://github.com/n0-computer/iroh-live/issues) and in
[REVIEW.md](REVIEW.md). The [plans/](plans/PLANS.md) directory contains
explorations of potential next steps rather than a fixed roadmap. Architecture
documentation is in [docs/](docs/index.md).

```sh
# Full check (all feature combinations, clippy, fmt)
cargo make check-all

# Test
cargo test --workspace --all-features
```

## License

Copyright 2025 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
