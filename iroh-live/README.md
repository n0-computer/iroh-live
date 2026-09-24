# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

`Live` is a node ready for live media: it binds the `iroh-moq` transport to an
iroh `Endpoint`, publishes broadcasts at paths that name this node, and
resolves other nodes' broadcasts over whichever link serves them. The media
comes from [`iroh-live-media`](../iroh-live-media), re-exported here as
`iroh_live::media`, and the transport as `iroh_live::moq`.

## Publishing

```rust
use iroh_live::{
    EndpointOptions, Live, LocalBroadcast,
    media::{
        AudioEncoding, AudioSource, MicrophoneConfig, VideoEncoding, VideoRendition, VideoSource,
        video,
    },
};

let live = Live::builder(EndpointOptions::default().bind().await?).with_router().spawn();
let broadcast = LocalBroadcast::new();

let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(
    camera,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
let microphone = AudioSource::microphone(MicrophoneConfig::default()).await?;
broadcast.set_audio(microphone, AudioEncoding::voice())?;

let publication = live.publish("hello", &broadcast)?;
println!("{}", publication.ticket().expect("a live path"));
```

`Live::publish` offers the broadcast to everyone, attached relays included. For
another audience, publish through `live.moq()`.

## Subscribing

```rust
use iroh_live::media::PlayerConfig;

let remote = live.subscribe(&ticket).await?;
let player = remote.play(PlayerConfig::default())?;
let mut frames = player.video();
```

The broadcast follows its path through the route table, so a change of route
shows as a switch rather than an end, and the serving session's link is
attached to it: every player started from it adapts to the downlink without
further wiring. `Live::remote_broadcast` wraps a subscription from a room or
from `Moq::subscribe` the same way.

## Rooms

Rooms live in [`iroh-rooms`](../iroh-rooms), re-exported as `iroh_live::rooms`
behind the `rooms` feature. Mount the room protocol on the router with
`LiveBuilder::accept`.

## Feature flags

All pass through to `iroh-live-media`: `capture`, `render`, and `sound-server`
by default, plus `playback`, `aec`, `pipewire`, `rpicam`, `vaapi`, `nvidia`, and
`v4l2`.

## Examples

`examples/publish.rs` publishes a camera and a microphone, with a simulcast
ladder behind `--simulcast`. `examples/subscribe_test.rs` is the test helper the
browser end-to-end suite uses.
