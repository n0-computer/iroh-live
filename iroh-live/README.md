# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

A `Node` publishes broadcasts at paths and subscribes to paths, over a direct
session with a peer or an attached relay. The transport is re-exported as
`iroh_live::node`, and the moq-net it builds on as `iroh_live::net`. The media
crates are gathered in `iroh_live::media`.

The crate builds against the unreleased `main` branch of
[moq](https://github.com/moq-dev/moq), pinned by git rev, so it is not on
crates.io. Depend on it through git:

```toml
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full"] }
```

The [top-level README](../README.md#the-moq-dependency) says why.

## Paths and layouts

The node takes any path: `Node::publish` and `Node::subscribe` know nothing
about how paths are chosen. iroh-live ships one convention, `LiveLayout`,
which publishes a peer's broadcasts at `<root>/<peer>/<name>` (the root is
`live` by default) and lets each peer publish only under its own id. Every use
names the layout, so nothing about it is implicit. An application with its own
scheme calls the node with its own paths and gives it a grant of its own, see
`NodeConfig::grant`.

A `BroadcastTicket` names a peer and the path it publishes at, whatever the
layout. `Node::subscribe_ticket` reads one on any node: it grants the ticket's
publisher that one path for as long as the subscription lives, so a viewer
needs no layout and no grant of its own.

## Example

Publish a camera, then share the ticket:

```rust
use iroh::protocol::Router;
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{LocalBroadcast, VideoEncoding, VideoRendition, VideoSource, video},
};

let layout = LiveLayout::default();
let endpoint = EndpointConfig::default().bind().await?;
let node = Node::new(endpoint.clone(), layout.config(&[]));
// Accepts the viewers. Keep it: dropping it stops accepting.
let router = node.mount(Router::builder(endpoint)).spawn();
// Created in the node's route table: peers read it without a copy.
let broadcast: LocalBroadcast = layout.publish(&node, "hello")?;
let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(
    camera,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
println!("{}", layout.ticket(node.id(), "hello")?);
```

On the other side, subscribe and play. A node with the default config is
enough:

```rust
use iroh_live::media::{PlayerConfig, remote_broadcast};

let subscription = node.subscribe_ticket(&ticket).await?;
let player = remote_broadcast(&subscription).play(PlayerConfig::default())?;
let mut frames = player.video();
```

## Main types

- `Node` is the node. `Node::new(endpoint, config)` creates it, and
  `LiveLayout::config(extra)` builds a `NodeConfig` for the layout.
  `mount(router_builder)` adds it to the application's `Router`, so peers can
  connect. Other protocols go on the same router. `Node::shutdown` ends the
  node but leaves the endpoint open: whoever binds the endpoint closes it,
  directly or through `Router::shutdown`, which also shuts down the node and
  every service mounted on the router.
- `LiveLayout` is the `<root>/<peer>/<name>` convention. `publish::<T>(&node,
  name)` creates a broadcast in the node's route table, published to everyone,
  attached relays included, and returns it as a `LocalBroadcast` or a raw
  `broadcast::Producer`. `publish_with(&node, name, audience, fill)` takes
  another audience, such as `Audience::peers([peer])`, fills the broadcast
  before it is announced, and returns the `Publication` with the result.
  `ticket(peer, name)` returns the ticket for a broadcast.
- `Node::publish(path, audience)` and `Node::subscribe(path, reach)` are the
  path-level calls: any path, any `Audience`, any `Reach`. They apply no
  layout. `publish` returns the `Publication`, whose `broadcast()` is the
  producer to fill and whose `ticket()` names it.
- `BroadcastTicket` holds a publisher's endpoint id and the full path of a
  broadcast. `BroadcastTicket::at(peer, path)` builds one.
- `Node::subscribe_ticket(&ticket)` returns the `Subscription` once a route is
  found.
- `media::remote_broadcast(&subscription)` reads its media without waiting for
  the catalog, for a subscription from a ticket, a room or `Node::subscribe`.
  The broadcast follows its path through the route table, so a change of route
  is a switch, not an end. Its players adapt to the link that serves it.
- `calls::Calls` places and answers one-to-one calls over the layout's `call`
  path, the convention that `irl call` and the Android demo share.
  `Calls::new(&node, &layout)` publishes this node's side at
  `<root>/<id>/call`, visible to nobody. `ring(peer)` offers it to that peer
  only, `call(peer)` rings and waits for the peer to answer, and
  `answer(peer)` resolves the other side. Each returns a `Call`, and dropping
  it hangs up. `events()` streams `CallEvent`, and `ringing()` watches the
  peers that ring.
- `EndpointConfig` binds an endpoint with `MoqPreset`, a secret key and mDNS.
  `EndpointConfig::apply` does the same on your own endpoint builder.
- `NodeConfig::default()` is subscribe-only: peers may read and publish
  nothing. `LiveLayout::config` lets each peer publish under its own id.
- `Error` wraps an `iroh_live_node::Error` as `Transport` and, with `media`, a
  `moq_ext_player::Error` as `Media`. A failure inside moq-net arrives as
  `Transport` holding the node's `Error::Moq`. The other variants come from
  binding an endpoint and parsing a ticket.

## Features

No feature is on by default. `full` turns on `media`, `capture`, `render`,
`sound-server` and `record`. `media` is the media stack. Without it the crate
is the layout, calls and endpoint setup only, which is how the relay uses it.
`playback`, `aec`, `pipewire`, `rpicam`, `vaapi`, `nvidia` and `v4l2` add
devices and codecs, imply `media`, and pass through to the media crates.
`sound-server` reaches audio devices through PipeWire or PulseAudio, through
moq-audio's `pipewire` and `pulseaudio` features.

`rooms` re-exports [`iroh-live-rooms`](../iroh-live-rooms) as
`iroh_live::rooms`. Run rooms on a node by adding
`iroh_live::rooms::publish_scope` to the layout's config, with
`layout.config(&[Arc::new(iroh_live::rooms::publish_scope)])`, and mount the
gossip with `Rooms::mount`:

```rust
let rooms = Rooms::new(&node);
let router = rooms.mount(node.mount(Router::builder(endpoint))).spawn();
```

## Examples

`examples/publish.rs` publishes a camera and a microphone, with a simulcast
ladder behind `--simulcast`. `examples/publish-pi.rs` publishes a Raspberry Pi
camera and needs `rpicam`. `examples/moq_only.rs` serves a broadcast with
`Node` alone, without a layout. `examples/subscribe_test.rs` is the helper the
browser test suite runs.

[Using iroh-live](../docs/guide/usage.md) has complete programs for the common
cases: calls, rooms, custom paths, relays and raw MoQ tracks.
