# iroh-live

Livestreaming video and audio over iroh

**Status: experimental / work in progress**

This repository implements a live streaming protocol over iroh with [Media over Quic](https://moq.dev/).
It uses [moq-rs](https://github.com/kixelated/moq) to transfer audio and video streams over iroh connections.

## Structure of the repository

* [**`web-transport-rs`**](web-transport-rs): Implements the [web-transport](https://github.com/kixelated/web-transport) traits for iroh connections
* [**`iroh-moq`**](iroh-moq): Adapters to create and accept [moq-lite](https://github.com/kixelated/moq/tree/main/rs/moq) sessions over iroh
* [**`iroh-live`**](iroh-live): Native capture, encoding and decoding of audio and video. This is an early preview of a high-level live streaming toolkit for iroh. Currently, it has these features, all subject to change:
  * Support for [hang](https://github.com/kixelated/moq/blob/main/rs/hang/) catalogs in MoQ sessions
  * Capture and playout audio (with [firewheel](https://github.com/BillyDM/Firewheel/))
  * Capture camera (with [nokwha](https://github.com/l1npengtul/nokhwa/))
  * Capture screens (with [xcap](https://github.com/nashaofu/xcap/))
  * Encode and decode video (h264) and audio (Opus) using [ffmpeg](https://docs.rs/ffmpeg-next/latest/ffmpeg_next/). Video encoding is hardware accelerated on supported platforms.
  * Support multiple renditions and on-demand switching of the encoding

There's still bugs and a lot of missing optimizations. This is an early, work-in-progress preview!

## Building

By default `ffmpeg` is dynamically linked. Enable the `static` feature to build ffmpeg from source and statically link it.

#### Build dependencies

*incomplete list, please file issues or PRs to expand this*

##### Linux

* For building with `static` feature: `apt install nasm pkg-config`

##### Windows

Windows builds require MSYS2 with the UCRT64 environment. You'll need to install MSYS2, Rust, and various build dependencies including FFmpeg, clang, and webrtc-audio-processing.

See [BUILD-WINDOWS.md](BUILD-WINDOWS.md) for detailed setup instructions.

## Demo and examples

Check out the [`rooms`](iroh-live/examples/rooms.rs) example:

```
cargo run --release --example rooms
```

This will print a *room ticket*. Copy this to another device, and run:

```
cargo run --release --example rooms -- <TICKET>
```

Now you're chatting! With video and audio! Over iroh!

Use the `room-publish-file` example to publish a video form a file into a room.

There's also a [`publish`](iroh-live/examples/publish.rs) example (publish only, no GUI), and a [`watch`](iroh-live/examples/watch.rs) example (watch a stream from the publish example).

The examples use [`egui`](https://github.com/emilk/egui), however `iroh-live` is not coupled to any GUI framework and should work with anything that can render raw images to the screen.

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
