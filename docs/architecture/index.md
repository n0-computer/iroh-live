# Architecture

iroh-live is an application layer over two things it does not own: iroh for
connectivity and identity, and [moq-video and
moq-audio](https://doc.moq.dev/lib/rs/) for media. What this repository adds is
the layer between them, plus the pieces neither side has a home for.

## Crates

| Crate | What it is |
|---|---|
| `iroh-moq` | MoQ transport over iroh: the node origin, sessions, and ALPN negotiation |
| `iroh-rooms` | Gossip rooms. Media-free: it moves broadcast names and hands back consumers |
| `iroh-live` | `Live`, `Call`, `Subscription`, and tickets |
| `moq-media` | Publish and subscribe plumbing over moq-video and moq-audio |
| `moq-media-egui` | An egui widget over the texture `moq_video::render` returns, and the debug overlay |
| `moq-media-android` | The Camera2 push bridge and the EGL renderer for Android |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

`moq-media` has no iroh dependency: a broadcast arrives as a
`moq_net::broadcast::Producer` or `Consumer`, whatever carried it. `iroh-rooms`
has no media dependency. `iroh-live` depends on both and is the only crate that
joins them.

## What moq-media adds

Upstream covers a single publisher with a single rendition and a single
subscriber taking whatever it is given. Four things sit above that.

[Publishing](publish.md) fans one source out to a simulcast ladder, because an
upstream producer publishes one rendition and owns the device it captures from.
[Subscribing](subscribe.md) chooses among those renditions as the downlink moves
and swaps decoders without a blank frame. The [playout clock](playout.md) keeps
audio and video aligned across two independent decode paths. The catalog carries
an [extension](publish.md#catalog) for chat and publisher identity alongside
hang's media sections.

Everything else is upstream. See [the media stack](media-stack.md) for what we
use, what we contributed back, and what was lost when the in-house stack was
deleted.

## iroh-live

`Live` binds an iroh `Endpoint` to a MoQ transport. It is built through a
builder, because whether it owns a router and whether it runs gossip are separate
decisions:

```rust
let live = Live::from_env().await?.with_router().with_gossip().spawn();
```

`from_env()` reads `IROH_SECRET` and binds an endpoint with the N0 preset, then
hands back the builder. `with_router()` spawns a `Router` and mounts every ALPN
this build speaks; an application that already has a router calls
`Live::register_protocols` on its own `RouterBuilder` instead. `with_gossip()`
creates a `Gossip` instance, which is the one thing `iroh-rooms` needs from here.

`Live::publish(path)` creates a broadcast on the node origin and returns a
`moq_media::publish::LocalBroadcast`. It is announced to every peer with a
session, so publishing is a property of the node rather than of a connection.
`Live::publish_raw` gives the bare producer for a caller writing its own tracks.

`Live::subscribe(remote, path)` dials, subscribes, and returns a `Subscription`
bundling the `MoqSession`, the `RemoteBroadcast`, and a
`watch::Receiver<NetworkSignals>` with the stats recorder and signal producer
already wired up. `Subscription::media()` opens whichever tracks the broadcast
carries.

`Call` is 1:1 sugar over the two. Each side publishes under
`calls/<its own endpoint id>` and subscribes to the other's, which is what
`Call::path(endpoint_id)` computes. The per-peer path replaced a fixed name that
two concurrent calls used to collide on.

## Conventions

`&self` everywhere. Public types use interior mutability, so they are safe to
share across tasks and threads without wrapper types.

Cleanup is drop-based. Dropping a `LocalBroadcast` ends its publish tasks;
dropping a `VideoTrack` drops its supervisor, which drops the reader task, which
drops the decoder. `CancellationToken` coordinates a broadcast-wide shutdown and
`AbortOnDropHandle` ties a task's life to a handle.

Continuous state is `n0_watcher::Watchable` and `Direct<T>`, which always has a
current value and can be awaited for changes. The catalog, the active rendition,
and the decoder backend all work this way. Discrete events are streams and
channels: room events, incoming sessions.

Bounded channels only. Frames between the decoder and the renderer go through a
single-slot latest-wins channel rather than a queue, so a renderer that falls
behind skips to the newest picture instead of draining a backlog.
