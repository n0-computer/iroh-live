# Getting started

iroh-live is real-time audio and video over [iroh](https://github.com/n0-computer/iroh),
using [Media over QUIC](https://moq.dev/) as the wire protocol. Connections are
peer-to-peer by default and no media server is involved. A relay is optional, and
only browsers need one.

## System dependencies

Codecs need nothing: openh264 is vendored and statically linked, and the audio
codecs and DSP are Rust. What needs system libraries is device access and
graphics.

```sh
# Debian and Ubuntu
sudo apt install libasound2-dev libpipewire-0.3-dev libclang-dev \
                 libegl-dev libgbm-dev libdrm-dev libfontconfig-dev libva-dev nasm

# Arch
sudo pacman -S alsa-lib pipewire clang mesa fontconfig libva nasm
```

macOS needs `libtool` and `automake` from Homebrew and nothing else: CoreAudio,
AVFoundation, ScreenCaptureKit, and VideoToolbox ship with the OS.

A build with `--no-default-features` needs none of this. It still encodes and
decodes, it just cannot open a device or draw.

## Building

```sh
cargo build --workspace                  # default features
cargo build --workspace --all-features   # everything, including VAAPI and NVIDIA
```

The workspace patches the moq crates to `Frando/moq@iroh-live` for five changes
that have not reached a release yet. See [the media
stack](../architecture/media-stack.md#what-we-contributed-upstream). A clean
clone builds without further setup; `Cargo.lock` pins the revision.

## First stream

Install the CLI and publish your camera and microphone:

```sh
cargo install --path iroh-live-cli

irl publish              # prints a ticket and a QR code
irl watch <TICKET>       # in another terminal, on another machine
```

No camera on the machine? `irl publish --test-source` publishes a generated
pattern and a tone instead, which is also the fastest way to check that the
transport works.

The full flag reference is in [the CLI page](../cli.md).

## Using the library

A publisher binds an endpoint, creates a broadcast, and points it at a device:

```rust
use iroh_live::{
    Live,
    media::{audio, video},
    ticket::LiveTicket,
};

let live = Live::from_env().await?.with_router().spawn();
let broadcast = live.publish("hello")?;

broadcast.video().set(video::capture::Config::default())?;
broadcast.audio().set(audio::capture::Config::default());

println!("{}", LiveTicket::new(live.endpoint().addr(), "hello"));
```

A subscriber connects with the ticket and reads decoded frames:

```rust
let live = Live::from_env().await?.spawn();
let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name).await?;
let tracks = sub.media().await;

if let Some(video) = tracks.video {
    while let Some(frame) = video.recv().await {
        // hand `frame` to a renderer
    }
}
```

`iroh-live/examples/publish.rs` is the compilable version of the first snippet,
including a two-rung simulcast ladder behind `--simulcast`.

## Where to go next

- [The CLI](../cli.md) for `irl publish` and `irl watch` in full.
- [Desktop rendering](desktop.md) for drawing frames in your own application.
- [Tickets](tickets.md) for how connection information is shared.
- [Architecture](../architecture/index.md) for how the crates fit together.
- [Raspberry Pi](raspberry-pi.md), [Android](android.md), and [the browser
  relay](browser-relay.md) for the platform-specific paths.
