# Architecture

iroh-live sits between two things it does not own: iroh for connectivity and
identity, and [moq-video and moq-audio](https://doc.moq.dev/lib/rs/) for media.
This repository holds the layer that joins them.

## Crates

| Crate | What it is |
|---|---|
| `moq-transport-iroh` | MoQ sessions over iroh connections: dial, accept, ALPN negotiation, the poll adapter, link statistics. No node concepts |
| `iroh-live-node` | The node over that transport: the route table, publications and audiences, sessions and relay links |
| `moq-ext-access` | Access control: grants, session requests and audiences. No transport dependency |
| `iroh-live-rooms` | Rooms: gossip membership as a watched state, and members-only broadcasts subscribed on demand. Media-free |
| `iroh-live` | `Live`, the facade that joins media and transport, `BroadcastTicket`, endpoint setup, and the re-exports |
| `moq-ext-player` | A player over moq-video and moq-audio: rendition choice, audio/video sync, recording |
| `moq-ext-publisher` | A broadcast that encodes sources into renditions |
| `moq-ext-adaptive` | Rendition choice and decoder handover as pure state machines |
| `moq-ext-sources` | Video and audio sources: test pattern, files, capture, push senders |
| `moq-ext-media-core` | The error, slot state and stats helpers the two above share |
| `iroh-live-egui` | Video views for egui over the texture `moq_video::render` returns, and the debug overlay |
| `iroh-live-media-android` | The EGL renderer and JNI handle helpers for Android |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

`moq-ext-player` and `moq-ext-publisher` have no iroh dependency, and neither
depends on the other. A transport publishes a `LocalBroadcast` by reading it through `moq_net::Consume`, and builds a
`RemoteBroadcast` from a route table with `from_origin` or `from_resolved`.
`iroh-live-rooms` has no media dependency. `iroh-live` depends on both and is
the only crate that joins them.

## What the moq-ext crates add

Upstream covers one publisher with one rendition, and one subscriber that takes
what it is given. This crate adds three things on top.

[Publishing](publish.md) encodes one source into a simulcast ladder.
[Subscribing](subscribe.md) starts a player that picks among those renditions
as the downlink changes, and swaps decoders without a blank frame. Each
player's [playout clock](playout.md) keeps audio and video aligned across two
separate decode paths.

The rest is upstream. See [the media stack](media-stack.md) for what we use.

## iroh-live

`Live` is a node ready for live media. It wraps an endpoint the application
binds, usually with `EndpointOptions::default().bind()`: `MoqPreset` (iroh's N0 preset
with BBR3 congestion control), a key and mDNS. `EndpointOptions::apply` does the
same on your own endpoint builder.

```rust
let live = Live::builder(endpoint).with_router().spawn();
```

`with_router()` spawns a `Router` that mounts the transport under every ALPN
this build speaks. `accept(alpn, handler)` mounts another protocol on it.
`with_rooms()` mounts rooms' gossip that way, and widens the grant so members
can publish their room broadcasts.

`Live::publish(name)` creates a broadcast at `live/<our id>/<name>` in the
route table, published to everyone, and returns it as an
`moq_ext_publisher::LocalBroadcast` (or any type built from a
`broadcast::Producer`). Peers read it without a copy. `Live::ticket(name)` returns the
`BroadcastTicket` to share. `Live::subscribe(&ticket)` resolves the ticket's
path over whichever link serves it and returns the `Subscription`.
`Live::remote_broadcast(&subscription)` reads its media without waiting for the
catalog, for a subscription from a ticket, a room or `Node::subscribe`. The
broadcast follows the path through the route table and carries the serving
link's measurements, so `remote.play(config)` starts a player that adapts with
nothing further to wire.

The facade's error is `iroh_live::Error`: `Transport` wraps an
`iroh_live_node::Error`, `Media` an `moq_ext_player::Error`, and `SecretKey` and
`Bind` come from `EndpointOptions`. Audiences, admission, relays and routes belong to [`iroh-live-node`](transport.md), reached
through `live.node()`.

`irl call` and the Android demo share a one-to-one call convention. Each side
publishes one broadcast named `iroh_live::CALL` (`live/<id>/call`) with
`Live::call`, visible to nobody, and its `CallOffer` rings a peer by adding it
to the `Audience::Peers` set. Each side subscribes to the other's. A node
learns it is called when a peer's `call` path appears in its route table
(`Node::origin`), and a hang-up takes the peer out of the set, which retracts
the path and ends the other side's read. The session stays open, and the
broadcast keeps running for the next call.

## Conventions

Methods take `&self`. Public types use interior mutability, so they can be
shared across tasks and threads without wrappers.

Cleanup is drop-based. Dropping the last clone of a `LocalBroadcast` ends its
publish tasks, and `close()` ends them for every clone with a clean finish.
Dropping a `Player` stops its selector, its video supervisor and its audio
task, and the supervisor's reader tasks drop their decoders with them.
`CancellationToken` coordinates a broadcast-wide shutdown, and
`AbortOnDropHandle` ties a task's life to a handle.

Continuous state is watched: a watcher always has a current value and can be
awaited for changes. The media crate's catalog, `PublishStatus` and
`PlayerStatus` are tokio `watch` receivers. The transport's watchers, such as
the open sessions, are `n0_watcher` watchers, as in iroh. Statistics are
snapshots read on demand.

Channels are bounded. Frames between the decoder and the renderer go through
`VideoFrames`, a single-slot latest-wins stream with a cursor per handle. A
renderer that falls behind skips to the newest picture.
