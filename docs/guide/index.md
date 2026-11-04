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

`moq-ext-publisher` and `moq-ext-player` have no default features and need none of these. They
encode and decode, but they cannot open a device or draw until you turn on `capture`, `playback`
or `render`.

## Building

```sh
cargo build --workspace                  # default features
cargo build --workspace --all-features   # every feature, VA-API and NVIDIA included
```

`iroh-live` itself has no default features. Ask for `full` to get the media
stack with capture, rendering, the sound server and recording, or pick from
`media`, `capture`, `render`, `playback`, `rooms` and the rest. Without
`media`, `iroh-live` is the node, its tickets and its grant, which is what a
relay needs.

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

A publisher binds an endpoint, mounts the node on a router so viewers can
connect, creates a broadcast, opens its sources, and publishes the broadcast
under a name:

```rust
use iroh::protocol::Router;
use iroh_live::{
    EndpointOptions, Live,
    media::{
        AudioEncoding, AudioSource, LocalBroadcast, MicrophoneConfig, VideoEncoding,
        VideoRendition, VideoSource, video,
    },
};

let endpoint = EndpointOptions::default().bind().await?;
let live = Live::new(endpoint.clone());
// Keep the router: dropping it stops accepting viewers.
let router = live.mount(Router::builder(endpoint)).spawn();
// Created in the node's route table, published while `broadcast` lives.
let broadcast: LocalBroadcast = live.publish("hello")?;

let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(camera, VideoEncoding::single(VideoRendition::p720()))?;
let microphone = AudioSource::microphone(MicrophoneConfig::default()).await?;
broadcast.set_audio(microphone, AudioEncoding::voice())?;
println!("{}", live.ticket("hello"));

// On exit: end the sessions, then close the endpoint through the router.
live.shutdown().await;
router.shutdown().await?;
```

The router is the application's, so it can carry other protocols next to the
ones `mount` adds. The broadcast is at `live/<endpoint id>/hello`, and the ticket names that path.
A subscriber resolves the ticket, starts a player, and reads decoded frames:

```rust
use iroh_live::{
    EndpointOptions, Live,
    media::{AudioTarget, PlayerConfig, audio::playback::Engine},
};

// Only dials, so it needs no router.
let live = Live::new(EndpointOptions::default().bind().await?);
let subscription = live.subscribe(&ticket).await?;
let engine = Engine::open(Default::default()).await?;
let player = live.remote_broadcast(&subscription).play(PlayerConfig {
    audio: Some(AudioTarget::from(engine)),
    ..Default::default()
})?;

let mut frames = player.video();
while let Some(frame) = frames.next().await {
    // hand `frame` to a renderer
}
```

`Engine::open` and `AudioTarget::from(engine)` need the `playback` feature.
`AudioTarget::null()` discards audio, for headless use. A player with
`audio: None` does not subscribe to the audio track.

`VideoSource::capture` fails if the camera cannot be opened.
`AudioSource::microphone` only checks that the device exists. The microphone
opens when the broadcast first has a subscriber, and a failure then shows in
`LocalBroadcast::status()`.

`Live::subscribe` returns once a route to the broadcast is found. The catalog
arrives later: `RemoteBroadcast::catalog()` is a watcher that turns `Some`
when it does.

The paths, the grant and the endpoint are defaults, and each can be replaced:

- `Live::builder_with(endpoint, NodeConfig)` sets the admission and the grant.
- `Live::publish_to(name, Audience)` publishes under the layout to an
  audience other than everyone, such as `Audience::peers([peer])`.
- `Live::publish_at(path, Audience)` and
  `Live::subscribe_path(path, Reach)` work on any path, next to the ticket
  calls. `Live::remote_broadcast(&subscription)` reads the media of the result.
- `Live::node()` is the escape hatch to the whole transport.
- `EndpointOptions::apply(builder)` adds the preset, the key and mDNS to your
  own endpoint builder, so you can set the relay mode or another address
  lookup. `bind()` does the same on a default one.
- `iroh_live::paths` builds and parses the `live/<id>/<name>` paths.

A node that needs none of `Live` can mount `Node` on a router itself, as
`iroh-live/examples/moq_only.rs` does.

`iroh-live/examples/publish.rs` is a complete publisher, with a two-rung
ladder behind `--simulcast`.

## Where to go next

- [Using iroh-live](usage.md) for complete examples: a call, rooms, your own
  paths, a relay, and raw MoQ tracks.
- [The CLI](../cli.md) for every `irl` command.
- [Desktop rendering](desktop.md) for drawing frames in your own application.
- [Tickets](tickets.md) for how a broadcast is named and shared.
- [Architecture](../architecture/index.md) for how the crates fit together.
- [Raspberry Pi](raspberry-pi.md), [Android](android.md), and [the browser
  relay](browser-relay.md) for other platforms.
