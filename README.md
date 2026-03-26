# iroh-live

> **Early tech preview.** iroh-live is experimental and under active development.
> Key limitations: no Windows support, on-device testing limited to Linux (Intel)
> and Android, no A/V sync (removed — will return), no direct browser-to-browser
> (relay required), codec selection is H.264, Opus, and AV1 only.

iroh-live is a Rust library and CLI for real-time audio and video over
[iroh](https://github.com/n0-computer/iroh) (QUIC). It handles the full
pipeline from camera capture through encoding, transport, decoding, and
rendering. Connections are peer-to-peer by default; an optional relay server
bridges to browsers via WebTransport. The transport layer uses
[Media over QUIC (MoQ)](https://moq.dev/), where each video rendition and audio
track travels as an independent QUIC stream — a dropped video packet never
blocks audio delivery.

## Quick start

Install the `irl` CLI and publish your camera and microphone:

```sh
cargo install --path tools/iroh-live-cli --all-features

# Terminal 1: publish camera + mic, prints a ticket and QR code
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

For a multi-party room:

```sh
# First participant creates the room
irl room

# Others join with the printed ticket
irl room <TICKET>
```

## Using iroh-live in Rust

The `iroh-live` crate provides the high-level API. A minimal publish loop:

```rust
use iroh_live::{Live, media::publish::LocalBroadcast};

let live = Live::create().await?;
let broadcast = LocalBroadcast::new("hello");

// Attach a camera source and start encoding
broadcast.video().set(camera_source);
broadcast.audio().set(mic_source);

// Serve to subscribers — prints a ticket
live.serve(broadcast).await?;
```

On the subscribe side:

```rust
use iroh_live::{Live, ticket::LiveTicket};

let live = Live::create().await?;
let ticket: LiveTicket = "<TICKET>".parse()?;
let remote = live.subscribe(&ticket).await?;

// Decoded frames arrive on channels
let video = remote.video_track().await?;
while let Some(frame) = video.recv().await {
    // render frame
}
```

See [docs/guide/desktop.md](docs/guide/desktop.md) for rendering options
(wgpu, OpenGL, CPU fallback) and framework integrations (egui, dioxus).

## CLI tool (`irl`)

The `irl` binary lives in `tools/iroh-live-cli`. Commands:

| Command | Description |
|---------|-------------|
| `irl devices` | List cameras, screens, audio devices, and codecs |
| `irl publish` | Broadcast capture or file (subcommands: `capture`, `file`) |
| `irl play <TICKET>` | Subscribe and render a remote broadcast |
| `irl call [TICKET]` | 1:1 bidirectional video call |
| `irl room [TICKET]` | Multi-party room with participant grid |

Common flags for `publish`: `--video cam`, `--video screen`, `--test-source`,
`--codec av1`, `--video-presets 360p,720p`, `--relay <endpoint>`,
`--room <ticket>`.

See [plans/cli.md](plans/cli.md) for the full command reference and remaining
work.

## Demos

- **`iroh-live/examples/split.rs`** — local split-screen: full encode/transport/decode loop on localhost.
- **`demos/android/`** — Kotlin/Rust Android app with bidirectional video calling ([README](demos/android/README.md)).
- **`demos/pi-zero/`** — Raspberry Pi Zero 2 W camera stream with e-paper QR code ([README](demos/pi-zero/README.md)).
- **`iroh-live-relay/`** — relay server bridging iroh to browsers via WebTransport.
- **`demos/opengl/`** — minimal GLES2 viewer using glutin/winit, no egui or wgpu.

## Platform support

Software codecs (H.264 via openh264, AV1 via rav1e/rav1d, Opus) work on every
platform Rust targets. Hardware acceleration and capture depend on the OS:

| Platform | Codecs | Capture | GPU render | Status |
|----------|--------|---------|------------|--------|
| Linux (Intel/AMD) | Software + VAAPI HW | PipeWire, V4L2, X11 | Vulkan + DMA-BUF zero-copy | Tested |
| macOS | Software + VideoToolbox | ScreenCaptureKit, AVFoundation | Metal via wgpu | Compiles, untested |
| Android | Software + MediaCodec HW | CameraX via JNI | EGL + HardwareBuffer zero-copy | Tested |
| Raspberry Pi | Software + V4L2 HW | V4L2 camera | Vulkan (RPi 5) | Partial |
| Windows | Software only | Not yet | DX12 via wgpu | Minimal |
| iOS | Software + VideoToolbox | AVFoundation | Metal via wgpu | Compiles, untested |

See [plans/platforms.md](plans/platforms.md) for the full matrix with backend
details and hardware-specific notes.

## Workspace crates

| Crate | Description |
|-------|-------------|
| [`iroh-live`](iroh-live) | High-level API: live sessions, rooms, tickets. Depends on `moq-media` + `iroh`. |
| [`iroh-moq`](iroh-moq) | MoQ transport layer over iroh/quinn via `web-transport-iroh`. |
| [`moq-media`](moq-media) | Media pipelines: capture, encode, decode, publish, subscribe, playout, adaptive bitrate. No iroh dependency. |
| [`rusty-codecs`](rusty-codecs) | Codec implementations (H.264, AV1, Opus) + VAAPI/V4L2/VideoToolbox HW accel + wgpu rendering. |
| [`rusty-capture`](rusty-capture) | Cross-platform screen and camera capture: PipeWire, V4L2, X11, Apple ScreenCaptureKit/AVFoundation. |
| [`moq-media-egui`](moq-media-egui) | egui integration for video rendering (wgpu texture upload, DMA-BUF import). |
| [`moq-media-dioxus`](moq-media-dioxus) | dioxus-native integration for video rendering. |
| [`moq-media-android`](moq-media-android) | Android camera, EGL rendering, JNI bridge. |
| [`iroh-live-relay`](iroh-live-relay) | Relay server for browser WebTransport bridging. |

## Building

The default build compiles all codecs from bundled source — no system libraries
required:

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

Key feature flags: `vaapi` (Linux HW codecs), `v4l2` (V4L2 HW codecs),
`videotoolbox` (macOS HW codecs), `wgpu` (GPU rendering), `dmabuf-import`
(Linux zero-copy decode-to-render), `av1` (AV1 codec, on by default).

## Contributing

Open items and known issues are tracked in [REVIEW.md](REVIEW.md). The
development roadmap lives in [plans/PLANS.md](plans/PLANS.md). Architecture
documentation is in [docs/](docs/index.md).

```sh
# Lint
cargo clippy --locked --workspace --all-targets --all-features
cargo fmt --check

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
