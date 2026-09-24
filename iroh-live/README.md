# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

`Live` binds an iroh `Endpoint` to a MoQ transport. `Live::publish` hands back a
broadcast every connected peer can subscribe to, `Live::subscribe` reaches one a
peer publishes, and `Call` is 1:1 sugar over the two. The media comes from
[`iroh-live-media`](../iroh-live-media), re-exported here as `iroh_live::media`.

## Publishing

```rust
use iroh_live::{
    Live,
    media::{
        AudioEncoding, AudioSource, MicrophoneConfig, VideoEncoding, VideoRendition, VideoSource,
        video,
    },
    ticket::LiveTicket,
};

let live = Live::from_env().await?.with_router().spawn();
let broadcast = live.publish("hello")?;

let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(camera, VideoEncoding::single(VideoRendition::p720()))?;
let microphone = AudioSource::microphone(MicrophoneConfig::default()).await?;
broadcast.set_audio(microphone, AudioEncoding::voice())?;

println!("{}", LiveTicket::new(live.endpoint().id(), "hello"));
```

Publishing is node-wide. A broadcast is created on the endpoint's origin and
announced to every peer with a session, so connecting to a relay later is enough
to reach it there.

## Subscribing

```rust
use iroh_live::media::PlayerConfig;

let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name).await?;
let player = sub.broadcast().play(PlayerConfig::default())?;
let mut frames = player.video();
```

`Subscription` bundles the MoQ session, the `RemoteBroadcast`, and the
connection's `LinkSignals`. The signals are already attached to the broadcast,
so every player started from `sub.broadcast()` follows the downlink without
further wiring; `sub.signals()` is there for diagnostics. When a session and a
broadcast arrive separately, as an `iroh-rooms` event hands them over,
`Subscription::new(session, broadcast)` does the same attaching.

## Rooms

Rooms live in [`iroh-rooms`](../iroh-rooms). What they need from here is the
gossip instance, which `LiveBuilder::with_gossip()` creates and
`Live::gossip()` returns.

## Feature flags

All pass through to `iroh-live-media`: `capture`, `render`, and `sound-server`
by default, plus `playback`, `aec`, `pipewire`, `rpicam`, `vaapi`, `nvidia`, and
`v4l2`.

## Examples

`examples/publish.rs` publishes a camera and a microphone, with a simulcast
ladder behind `--simulcast`. `examples/subscribe_test.rs` is the test helper the
browser end-to-end suite uses.
