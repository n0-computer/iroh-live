# Getting Started

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | iroh-live, moq-media, rusty-codecs, rusty-capture |
| Platforms | Linux, macOS, Windows |

## What is iroh-live

iroh-live is a Rust library for real-time video and audio over [iroh](https://github.com/n0-computer/iroh), using Media over QUIC for transport. It handles the full pipeline from camera and screen capture through encoding, QUIC transport, decoding, and rendering. Connections are peer-to-peer by default, with an optional relay server for browser bridging. No media servers are required.

## System dependencies

The default build compiles all codecs (H.264 via openh264, AV1 via rav1e/rav1d, Opus) from bundled source. No system libraries are needed for a basic build.

For hardware-accelerated codecs and native capture on Linux, install the development headers:

```sh
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev libva-dev

# Arch
sudo pacman -S pipewire libclang libva
```

## Building

Build the entire workspace with default features (software codecs only):

```sh
cargo build --workspace
```

To include hardware acceleration and all capture backends:

```sh
cargo build --workspace --all-features
```

Run the test suite:

```sh
cargo test --workspace
```

## Publishing a stream

The `publish` example captures your camera and microphone, encodes video and audio, and prints a ticket string that viewers can use to connect:

```sh
cargo run --release --example publish
```

The output includes a line like `publishing at iroh-live:ABC.../my-stream`. Copy the full ticket string.

## Watching a stream

In a second terminal, pass the ticket to the `watch` example. This opens an egui window that decodes and renders the incoming video with audio:

```sh
cargo run --release --example watch -- <TICKET>
```

Replace `<TICKET>` with the string printed by the publisher. The viewer connects directly to the publisher over QUIC, with no intermediary.

## Multi-user rooms

Rooms use iroh-gossip for peer discovery. The first participant creates the room, and others join with the printed ticket:

```sh
# First participant creates the room
cargo run --release --example rooms

# Others join with the ticket
cargo run --release --example rooms -- <TICKET>
```

Each participant publishes their own broadcast and automatically subscribes to every other participant. See [rooms.md](rooms.md) for details on the room API.

## Other examples

All examples live in `iroh-live/examples/`:

| Example | Description |
|---------|-------------|
| `publish` | Broadcast camera and microphone, print a ticket |
| `watch` | egui viewer with video and audio |
| `watch-wgpu` | Minimal fullscreen wgpu viewer without egui |
| `call` | 1:1 peer-to-peer video call |
| `rooms` | Multi-user room with dynamic participant discovery |
| `split` | Local split-screen encode/decode loop on localhost |
| `push` | Stream a video file or transcode via ffmpeg |
| `room-publish-file` | Publish a video file into a room |

The `demos/opengl` crate is a standalone GLES2 viewer using glutin and winit, without any egui or wgpu dependency.

## Next steps

- [Desktop rendering](desktop.md) covers the three rendering paths (wgpu, OpenGL ES, CPU fallback) and GUI framework integration.
- [Tickets](tickets.md) explains how connection tickets work and how to share them.
- [MoQ protocol overview](moq.md) describes the wire protocol and broadcast structure.
- [Raspberry Pi](raspberry-pi.md), [Android](android.md), and [Browser relay](browser-relay.md) cover platform-specific deployment.
