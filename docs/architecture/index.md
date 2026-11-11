# Architecture

iroh-live sits between two things it does not own: iroh for connectivity and
identity, and [moq-video and moq-audio](https://doc.moq.dev/lib/rs/) for media.
This repository holds the layer that joins them.

## Crates

| Crate | What it is |
|---|---|
| `moq-transport-iroh` | MoQ sessions over iroh connections: dial, accept, ALPN negotiation, moq-net's poll transport on iroh, link statistics. No node concepts |
| `iroh-live-node` | The node over that transport: the route table, publications and audiences, sessions and relay links |
| `moq-ext-access` | Access control: grants, session requests, audiences, layouts, and the policies that admit and authorize peers, with JWT and rcan verifiers behind features. No transport dependency |
| `iroh-live-rooms` | Rooms: gossip membership as a watched state, and members-only broadcasts subscribed on demand. Media-free |
| `iroh-live` | `LiveLayout` and `Calls`, the facade that joins media and transport, endpoint setup, and the re-exports |
| `moq-ext-player` | A player over moq-video and moq-audio: rendition choice, audio/video sync, recording |
| `moq-ext-publisher` | A broadcast that encodes sources into renditions |
| `moq-ext-adaptive` | Rendition choice and decoder handover as pure state machines |
| `moq-ext-sources` | Video and audio sources: test pattern, files, capture, push senders |
| `moq-ext-media-core` | The error, slot state and stats helpers the two above share |
| `moq-ext-egui` | Video views for egui over the texture `moq_video::render` returns, and the debug overlay |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

No `moq-ext-*` crate depends on iroh, so each could move upstream into moq.
That includes the UI crate `moq-ext-egui`. `moq-ext-player` and `moq-ext-publisher` do not
depend on each other. A transport publishes a `LocalBroadcast` by reading it
through `moq_net::Consume`, and builds a `RemoteBroadcast` from a route table
with `from_resolved`.
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

`iroh-live` is the application's entry to the node for live media. The
application binds an endpoint, usually with `EndpointConfig::default().bind()`:
`MoqPreset` (iroh's N0 preset with BBR3 congestion control), a key and mDNS.
`EndpointConfig::apply` does the same on your own endpoint builder.

```rust
let layout = LiveLayout::default();
let node = Node::new(endpoint.clone(), layout.config());
let router = node.mount(Router::builder(endpoint)).spawn();
```

The node takes any path. `LiveLayout` is the one convention iroh-live ships:
broadcasts at `<root>/<peer>/<name>`, where the root is `live` by default, and
each peer may publish only under its own id. `LiveLayout::config()` builds
the `NodeConfig` with that layout's policy. The node owns neither the endpoint nor a router.
`Node::mount` adds the transport to the application's `RouterBuilder` under
every ALPN this build speaks, and the application adds its own protocols to the
same router. `Rooms::mount` adds the rooms' gossip, and
`iroh_live::layout::config((LiveLayout::default(), RoomsLayout))` builds a
config that also lets members publish their room broadcasts. `Router::shutdown` shuts down the node and every mounted service,
and closes the endpoint. A node that is neither mounted nor attached to a relay
logs a warning when it publishes, since no peer can dial in.

`layout.publish::<LocalBroadcast>(&node, name)` creates a broadcast at
`live/<our id>/<name>` in the route table, published to every granted session,
and returns it as an `moq_ext_publisher::LocalBroadcast` (or any type built from a
`broadcast::Producer`). Peers read it without a copy. `layout.ticket(node.id(),
name)` returns the `BroadcastTicket` to share. `Node::subscribe_ticket(&ticket)`
resolves the ticket's path over whichever link serves it and returns the
`Subscription`. On a node with the default config, it claims exactly the
ticket's path for the ticket's peer while the subscription lives.
`iroh_live::media::remote_broadcast(&subscription)` reads its media without
waiting for the catalog, for a subscription from a ticket, a room or
`Node::subscribe`. The broadcast follows the path through the route table and
carries the serving link's measurements, so `remote.play(config)` starts a
player that adapts with nothing further to wire.

The facade's error is `iroh_live::Error`: `Node` wraps an
`iroh_live_node::Error`, `Media` a `moq_ext_player::Error`, `Rooms` an
`iroh_live_rooms::Error` and `Calls` an `iroh_live::calls::Error`. `SecretKey`
and `Bind` come from `EndpointConfig`. Audiences, admission, relays and routes
belong to [`iroh-live-node`](transport.md), and [Access
control](../guide/access.md) describes the policies.

`irl call` and the Android demo share a one-to-one call convention, in
`iroh_live::calls`. `Calls::new(&node, &layout)` publishes this side as one
broadcast at `<root>/<id>/call`, visible to nobody, and `ring` rings a peer by
adding it to the `Audience::Peers` set. Each side subscribes to the other's. A
node learns it is called when a peer's `call` path appears in its route table,
which `Calls::events` reports as `CallEvent::Ringing`. A hang-up takes the peer
out of the set, which retracts the path and ends the other side's read. The
session stays open, and the broadcast keeps running for the next call. Each
node's policy must let the other publish its `call` path, which the layout's
policy does. A filter on it, such as a contact list, keeps strangers from
ringing.

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
