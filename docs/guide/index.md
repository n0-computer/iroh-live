# Getting Started

iroh-live is a Rust library for real-time video and audio over [iroh](https://github.com/n0-computer/iroh), using Media over QUIC for transport. It handles the full pipeline from camera and screen capture through encoding, QUIC transport, decoding, and rendering. Connections are peer-to-peer by default, with an optional relay server for browser bridging. No media servers are required.

This page walks through building the workspace, running your first publish and subscribe, and finding the right example for your use case.

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

## The CLI tool

The `iroh-live-cli` crate (binary name `irl`) provides publish, play, call, and room commands:

```sh
cargo build --release -p iroh-live-cli
```

### Publishing a stream

Capture your camera and microphone, encode, and print a ticket:

```sh
cargo run --release -p iroh-live-cli -- publish
```

The output includes a line like `iroh-live:ABC.../my-stream`. Copy the full ticket string.

### Playing a stream

In a second terminal, pass the ticket to the play command:

```sh
cargo run --release -p iroh-live-cli -- play <TICKET>
```

The viewer connects directly to the publisher over QUIC, with no intermediary.

### Multi-user rooms

Rooms use iroh-gossip for peer discovery. The first participant creates the room, and others join with the printed ticket:

```sh
# First participant creates the room
cargo run --release -p iroh-live-cli -- room

# Others join with the ticket
cargo run --release -p iroh-live-cli -- room <TICKET>
```

Each participant publishes their own broadcast and automatically subscribes to every other participant. See [rooms.md](rooms.md) for details on the room API.

## Examples

A few standalone examples remain in the workspace:

| Example | Crate | Description |
|---------|-------|-------------|
| `split` | iroh-live | Local split-screen encode/decode loop on localhost |
| `watch-wgpu` | iroh-live | Minimal fullscreen wgpu viewer without egui |
| `frame_dump` | iroh-live | Capture frames from a broadcast, save as PNGs |
| `viewer` | moq-media | Standalone media viewer |

The `demos/opengl` crate is a standalone GLES2 viewer using glutin and winit, without any egui or wgpu dependency.

## Next steps

- [Desktop rendering](desktop.md) covers the three rendering paths (wgpu, OpenGL ES, CPU fallback) and GUI framework integration.
- [Tickets](tickets.md) explains how connection tickets work and how to share them.
- [MoQ protocol overview](moq.md) describes the wire protocol and broadcast structure.
- [Raspberry Pi](raspberry-pi.md), [Android](android.md), and [Browser relay](browser-relay.md) cover platform-specific deployment.
