# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

`Live` is a node ready for live media: it binds the `iroh-moq` transport to an
iroh `Endpoint`, publishes broadcasts at paths that name this node, and
resolves other nodes' broadcasts over whichever link serves them. The media
comes from [`iroh-live-media`](../iroh-live-media), re-exported here as
`iroh_live::media`, and the transport as `iroh_live::moq`.

## Publishing

```rust
use iroh_live::{EndpointOptions, Live, LocalBroadcast, media::{audio, video}, moq::net::broadcast};

let live = Live::builder(EndpointOptions::default().bind().await?).with_router().spawn();
let broadcast = LocalBroadcast::new(broadcast::Info::new().produce())?;
broadcast.video().set(video::capture::Config::default())?;
broadcast.audio().set(audio::capture::Config::default());

let publication = live.publish("hello", broadcast.consume())?;
println!("{}", publication.ticket().expect("a live path"));
```

`Live::publish` offers the broadcast to everyone, attached relays included. For
another audience, publish through `live.moq()`.

## Subscribing

```rust
let remote = live.subscribe(&ticket).await?;
let tracks = remote.media().await;
```

`Live::remote_broadcast` wraps a subscription from a room or from
`Moq::subscribe` the same way. `iroh_live::network::signals` feeds a video
track's adaptation from the link serving a subscription.

## Rooms

Rooms live in [`iroh-rooms`](../iroh-rooms), re-exported as `iroh_live::rooms`
behind the `rooms` feature. Mount the room protocol on the router with
`LiveBuilder::accept`.

## Feature flags

All pass through to `iroh-live-media`: `capture` and `render` by default, plus
`playback`, `aec`, `pipewire`, `vaapi`, and `nvidia`.

## Examples

`examples/publish.rs` publishes a camera and a microphone, with a simulcast
ladder behind `--simulcast`. `examples/subscribe_test.rs` is the test helper the
browser end-to-end suite uses.
