# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

`Live` binds an iroh `Endpoint` to a MoQ transport. `Live::publish` hands back a
broadcast every connected peer can subscribe to, `Live::subscribe` reaches one a
peer publishes, and `Call` is 1:1 sugar over the two. The media comes from
[`moq-media`](../moq-media), re-exported here as `iroh_live::media`.

## Publishing

```rust
use iroh_live::{Live, media::{audio, video}, ticket::LiveTicket};

let live = Live::from_env().await?.with_router().spawn();
let broadcast = live.publish("hello")?;

broadcast.video().set(video::capture::Config::default())?;
broadcast.audio().set(audio::capture::Config::default());

println!("{}", LiveTicket::new(live.endpoint().addr(), "hello"));
```

Publishing is node-wide. A broadcast is created on the endpoint's origin and
announced to every peer with a session, so connecting to a relay later is enough
to reach it there.

## Subscribing

```rust
let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name).await?;
let tracks = sub.media().await;
```

`Subscription` bundles the MoQ session, the `RemoteBroadcast`, and a receiver of
transport signals, with the stats recorder and the signal producer already wired
up. Hand the signals to `VideoTrack::enable_adaptation` to follow the downlink.

## Rooms

Rooms live in [`iroh-rooms`](../iroh-rooms). What they need from here is the
gossip instance, which `LiveBuilder::with_gossip()` creates and
`Live::gossip()` returns.

## Feature flags

All pass through to `moq-media`: `capture` and `render` by default, plus
`playback`, `aec`, `pipewire`, `vaapi`, and `nvidia`.

## Examples

`examples/publish.rs` publishes a camera and a microphone, with a simulcast
ladder behind `--simulcast`. `examples/subscribe_test.rs` is the test helper the
browser end-to-end suite uses.
