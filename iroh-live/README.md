# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

`Live` is a node for live media. It runs the `iroh-moq` transport on an iroh
endpoint, publishes broadcasts at `live/<endpoint id>/<name>`, and resolves
other nodes' broadcasts over a direct session or an attached relay. The media
crate is re-exported as `iroh_live::media` and the transport as
`iroh_live::moq`, so an application needs this one dependency.

## Example

Publish a camera, then share the ticket:

```rust
use iroh_live::{
    EndpointOptions, Live, LocalBroadcast,
    media::{VideoEncoding, VideoRendition, VideoSource, video},
};

let live = Live::builder(EndpointOptions::default().bind().await?).with_router().spawn();
let broadcast = LocalBroadcast::new();
let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(
    camera,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
live.publish("hello", &broadcast)?;
println!("{}", live.ticket("hello"));
```

On the other side, subscribe and play:

```rust
use iroh_live::PlayerConfig;

let subscription = live.subscribe(&ticket).await?;
let player = live
    .remote_broadcast(&subscription)
    .play(PlayerConfig::default())?;
let mut frames = player.video();
```

## Main types

- `Live` is the node. `Live::builder(endpoint)` creates it, `with_router()`
  lets peers connect, and `accept` mounts another protocol on the same router.
  `shutdown` closes the endpoint too.
- `Live::publish` publishes to everyone, attached relays included, and returns
  a `Publication`. The broadcast stays published until
  `Publication::unpublish`, the end of the broadcast, or shutdown. To publish
  for another audience, go through `live.moq()`.
- `BroadcastTicket` names a node and a broadcast. `Live::ticket(name)` returns
  the ticket for one of this node's broadcasts.
- `Live::subscribe(&ticket)` returns the `Subscription` once a route is found.
- `Live::remote_broadcast(&subscription)` reads its media without waiting for
  the catalog, for a subscription from a ticket, a room or `Moq::subscribe`.
  The broadcast follows its path through the route table, so a change of route
  is a switch, not an end. Its players adapt to the link that serves it.
  `RemoteBroadcast::closed()` resolves about three seconds after the publisher
  ends the broadcast.
- `CALL` is the broadcast name of the call convention `irl call` and the
  Android demo share.
- `grant` is the grant a live node gives each peer: subscribe to anything,
  publish only under the peer's own id. `moq_config` is the `MoqConfig` that
  uses it.
- `Error` is `Transport` (an `iroh_moq::Error`) or `Media` (an
  `iroh_live_media::Error`).

## Features

`media`, `capture`, `render`, and `sound-server` are on by default. `media` is
the media stack. Without it the crate is the `live/` layout only (tickets, the
grant, endpoint options), which is how the relay uses it. `playback`, `aec`,
`pipewire`, `rpicam`, `vaapi`, `nvidia`, and `v4l2` add devices and codecs,
imply `media`, and pass through to `iroh-live-media`.

`rooms` re-exports [`iroh-live-rooms`](../iroh-live-rooms) as
`iroh_live::rooms`, and adds the room paths to `grant`.

## Examples

`examples/publish.rs` publishes a camera and a microphone, with a simulcast
ladder behind `--simulcast`. `examples/publish-pi.rs` publishes a Raspberry Pi
camera and needs `rpicam`. `examples/subscribe_test.rs` is the helper the
browser test suite runs.
