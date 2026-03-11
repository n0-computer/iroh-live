# iroh-live

Real-time video and audio over [iroh](https://github.com/n0-computer/iroh), using [Media over QUIC](https://moq.dev/) for transport.

**Status: experimental / work in progress**

## Crates

| Crate | Description |
|-------|-------------|
| [`iroh-live`](iroh-live) | High-level API for live audio/video sessions and rooms |
| [`iroh-moq`](iroh-moq) | Media-over-QUIC transport — creates and accepts [moq-lite](https://github.com/kixelated/moq/tree/main/rs/moq) sessions over iroh |
| [`moq-media`](moq-media) | Media capture, encoding, decoding, and processing pipelines |
| [`rusty-codecs`](rusty-codecs) | Video and audio codecs (H.264, AV1, Opus) with hardware acceleration support |
| [`rusty-capture`](rusty-capture) | Cross-platform screen and camera capture with DMA-BUF zero-copy support |
| [`moq-media-egui`](moq-media-egui) | egui integration for video rendering (software and wgpu) |
| [`moq-media-dioxus`](moq-media-dioxus) | dioxus-native integration for video rendering |
| [`web-transport-iroh`](web-transport-iroh) | WebTransport implementation over iroh/quinn |

### How the pieces fit together

`iroh-live` is the entry point for most applications. It depends on `iroh-moq` for QUIC transport and `moq-media` for the media pipeline. `moq-media` uses `rusty-codecs` for encoding and decoding, and `rusty-capture` for camera and screen input. The egui and dioxus crates are optional rendering integrations — `iroh-live` itself is not coupled to any GUI framework.

## Building

No system dependencies are required for basic codec support. All codecs are either pure Rust or compiled from bundled C source (openh264 via `cc`, rav1e for AV1).

```sh
cargo build --workspace
```

For hardware-accelerated codecs and capture on Linux, you need PipeWire and VA-API development headers:

```sh
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev libva-dev

# Arch
sudo pacman -S pipewire libclang libva
```

Build with all features:

```sh
cargo build --workspace --all-features
```

## Demo

The [`rooms`](iroh-live/examples/rooms.rs) example runs a multi-peer video chat:

```sh
cargo run --release --example rooms
```

This prints a room ticket. Copy it to another device and run:

```sh
cargo run --release --example rooms -- <TICKET>
```

Use `room-publish-file` to publish a video file into a room. The [`publish`](iroh-live/examples/publish.rs) and [`watch`](iroh-live/examples/watch.rs) examples demonstrate one-way streaming without a GUI room.

The examples use [egui](https://github.com/emilk/egui), but `iroh-live` works with any framework that can render raw images.

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
