# Architecture

iroh-live sits between two things it does not own: iroh for connectivity and
identity, and [moq-video and moq-audio](https://doc.moq.dev/lib/rs/) for media.
This repository holds the layer that joins them.

## Crates

| Crate | What it is |
|---|---|
| `iroh-moq` | MoQ transport over iroh: the route table, publications and audiences, sessions and relay links |
| `iroh-live-rooms` | Rooms: gossip membership as a watched state, and members-only broadcasts subscribed on demand. Media-free |
| `iroh-live` | `Live`, the facade that joins media and transport, `BroadcastTicket`, endpoint setup, and the re-exports |
| `iroh-live-media` | Sources, broadcasts and players over moq-video and moq-audio |
| `iroh-live-egui` | Video views for egui over the texture `moq_video::render` returns, and the debug overlay |
| `iroh-live-media-android` | The EGL renderer and JNI handle helpers for Android |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

`iroh-live-media` has no iroh dependency. A transport publishes a
`LocalBroadcast` by reading it through `moq_net::Consume`, and builds a
`RemoteBroadcast` from a route table with `from_origin` or `from_resolved`.
`iroh-live-rooms` has no media dependency. `iroh-live` depends on both and is
the only crate that joins them.

## What iroh-live-media adds

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
binds, usually with `EndpointOptions::default().bind()`: iroh's N0 preset with
BBR3 congestion control, a key and mDNS.

```rust
let live = Live::builder(endpoint).with_router().spawn();
```

`with_router()` spawns a `Router` that mounts the transport under every ALPN
this build speaks. `accept(alpn, handler)` mounts another protocol on it, which
is how rooms mount their gossip. `LiveBuilder::moq()` hands the transport to
rooms before the router exists.

`Live::publish(name, &broadcast)` publishes an `iroh_live_media::LocalBroadcast`
at `live/<our id>/<name>` to everyone. `Live::ticket(name)` returns the
`BroadcastTicket` to share. `Live::subscribe(&ticket)` resolves the ticket's
path over whichever link serves it and returns the `Subscription`.
`Live::remote_broadcast(&subscription)` reads its media without waiting for the
catalog, for a subscription from a ticket, a room or `Moq::subscribe`. The
broadcast follows the path through the route table and carries the serving
link's measurements, so `remote.play(config)` starts a player that adapts with
nothing further to wire.

The facade's error is `iroh_live::Error`: `Transport` wraps an
`iroh_moq::Error`, `Media` an `iroh_live_media::Error`, and `SecretKey` and
`Bind` come from `EndpointOptions`. Audiences,
admission, relays and routes belong to [`iroh-moq`](transport.md), reached
through `live.moq()`.

`irl call` and the Android demo share a one-to-one call convention. Each side
offers a broadcast named `iroh_live::CALL` (`live/<id>/call`) to the other peer
only, with an `Audience::Peers` of one, and subscribes to the other's. A node
learns it is called when a peer's `call` path appears in its route table
(`Moq::origin`), and a hang-up withdraws the offer, which ends the other side's
read. The session stays open.

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
awaited for changes. A broadcast's `PublishStatus` and a player's
`PlayerStatus` are tokio `watch` receivers. The catalog and the open sessions
are `n0_watcher` watchers, as in iroh. Statistics are snapshots read on
demand.

Channels are bounded. Frames between the decoder and the renderer go through
`VideoFrames`, a single-slot latest-wins stream with a cursor per handle. A
renderer that falls behind skips to the newest picture.
