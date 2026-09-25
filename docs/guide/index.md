# Getting started

iroh-live sends audio and video over [iroh](https://github.com/n0-computer/iroh),
with [Media over QUIC](https://moq.dev/) as the wire protocol. Peers connect
directly, without a media server. Browsers need a relay.

## System dependencies

The codecs need no system libraries. Device access and graphics do:

```sh
# Debian and Ubuntu
sudo apt install libasound2-dev libpipewire-0.3-dev libclang-dev \
                 libegl-dev libgbm-dev libdrm-dev libfontconfig-dev libva-dev nasm

# Arch
sudo pacman -S alsa-lib pipewire clang mesa fontconfig libva nasm
```

macOS needs `libtool` and `automake` from Homebrew.

A build with `--no-default-features` needs none of these. It encodes and
decodes, but it cannot open a device or draw.

## Building

```sh
cargo build --workspace                  # default features
cargo build --workspace --all-features   # every feature, VA-API and NVIDIA included
```

The workspace patches the moq crates to the `iroh-live-5` branch of
`Frando/moq`, which adds one Windows build fix to the released versions.
`Cargo.lock` pins the revision, so a clean clone builds as is.

## First stream

Install the CLI and publish your camera and microphone:

```sh
cargo install --path iroh-live-cli

irl publish              # prints a ticket and a QR code
irl watch <TICKET>       # on another machine
```

Without a camera, `irl publish --test-source` publishes a test pattern and a
tone. [The CLI reference](../cli.md) lists every flag.

## Using the library

A publisher binds an endpoint, creates a broadcast, opens its sources, and
publishes the broadcast under a name:

```rust
use iroh_live::{
    EndpointOptions, Live, LocalBroadcast,
    media::{
        AudioEncoding, AudioSource, MicrophoneConfig, VideoEncoding, VideoRendition, VideoSource,
        video,
    },
};

let live = Live::builder(EndpointOptions::default().bind().await?)
    .with_router()
    .spawn();
let broadcast = LocalBroadcast::new();

let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(camera, VideoEncoding::single(VideoRendition::p720()))?;
let microphone = AudioSource::microphone(MicrophoneConfig::default()).await?;
broadcast.set_audio(microphone, AudioEncoding::voice())?;

live.publish("hello", &broadcast)?;
println!("{}", live.ticket("hello"));
```

The broadcast is at `live/<endpoint id>/hello`, and the ticket names that path.
A subscriber resolves the ticket, starts a player, and reads decoded frames:

```rust
use iroh_live::{AudioOutput, PlayerConfig};

let live = Live::builder(EndpointOptions::default().bind().await?).spawn();
let subscription = live.subscribe(&ticket).await?;
let output = AudioOutput::open(None).await?;
let player = live.remote_broadcast(&subscription).play(PlayerConfig {
    audio: Some(output),
    ..Default::default()
})?;

let mut frames = player.video();
while let Some(frame) = frames.next().await {
    // hand `frame` to a renderer
}
```

`AudioOutput::open` needs the `playback` feature. A player with `audio: None`
does not subscribe to the audio track.

`VideoSource::capture` fails if the camera cannot be opened.
`AudioSource::microphone` only checks that the device exists. The microphone
opens when the broadcast first has a subscriber, and a failure then shows in
`LocalBroadcast::status()`.

`Live::subscribe` returns once a route to the broadcast is found. The catalog
arrives later: `RemoteBroadcast::catalog()` is a watcher that turns `Some`
when it does. After the publisher ends the broadcast,
`RemoteBroadcast::closed()` resolves about three seconds later, because the
broadcast first looks for another route.

`iroh-live/examples/publish.rs` is a complete publisher, with a two-rung
ladder behind `--simulcast`.

## Where to go next

- [The CLI](../cli.md) for every `irl` command.
- [Desktop rendering](desktop.md) for drawing frames in your own application.
- [Tickets](tickets.md) for how a broadcast is named and shared.
- [Architecture](../architecture/index.md) for how the crates fit together.
- [Raspberry Pi](raspberry-pi.md), [Android](android.md), and [the browser
  relay](browser-relay.md) for other platforms.
