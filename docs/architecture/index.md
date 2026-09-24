# Architecture

iroh-live is an application layer over two things it does not own: iroh for
connectivity and identity, and [moq-video and
moq-audio](https://doc.moq.dev/lib/rs/) for media. What this repository adds is
the layer between them, plus the pieces neither side has a home for.

## Crates

| Crate | What it is |
|---|---|
| `iroh-moq` | MoQ transport over iroh: the route table, publications and audiences, sessions, relay links, tickets and endpoint setup |
| `iroh-rooms` | Rooms: gossip membership as a watched state, members-only broadcasts subscribed on demand, and room chat. Media-free |
| `iroh-live` | `Live`, the facade that joins media and transport, and the re-exports |
| `iroh-live-media` | Publish and subscribe plumbing over moq-video and moq-audio |
| `iroh-live-egui` | An egui widget over the texture `moq_video::render` returns, and the debug overlay |
| `iroh-live-media-android` | The Camera2 push bridge and the EGL renderer for Android |
| `iroh-live-cli` | The `irl` binary |
| `iroh-live-relay` | The browser bridge |

`iroh-live-media` has no iroh dependency: a broadcast arrives as a
`moq_net::broadcast::Producer` or `Consumer`, whatever carried it. `iroh-rooms`
has no media dependency. `iroh-live` depends on both and is the only crate that
joins them.

## What iroh-live-media adds

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

`Live` is a node ready for live media. It wraps an endpoint the application
binds, which is usually `EndpointOptions::default().bind()` (iroh's N0 preset
with a media-tuned transport, a key and mDNS):

```rust
let live = Live::builder(endpoint).with_router().spawn();
```

`with_router()` spawns a `Router` that mounts the transport under every ALPN
this build speaks, and `accept(alpn, handler)` mounts another protocol on it, as
rooms mount their gossip. `with_moq(moq)` uses a transport the application
created first.

`Live::publish(name, broadcast)` publishes a broadcast at `live/<our id>/<name>`
to everyone, and the publication's ticket is what to share.
`Live::subscribe(&ticket)` resolves the ticket's path over whichever link serves
it and returns a `RemoteBroadcast`; `Live::remote_broadcast(&subscription)`
does the same for a subscription from a room or from `Moq::subscribe`. The
transport's own concepts (audiences, admission, relays, routes) live in
[`iroh-moq`](transport.md) and are reached through `live.moq()`.

A one-to-one call is publish plus subscribe: `irl call` and the Android demo each
publish under `calls/<their own endpoint id>` and subscribe to the other's on
the session between them.

## Conventions

`&self` everywhere. Public types use interior mutability, so they are safe to
share across tasks and threads without wrapper types.

Cleanup is drop-based. Dropping a `LocalBroadcast` ends its publish tasks;
dropping a `VideoTrack` drops its supervisor, which drops the reader task, which
drops the decoder. `CancellationToken` coordinates a broadcast-wide shutdown and
`AbortOnDropHandle` ties a task's life to a handle.

Continuous state is `n0_watcher::Watchable` and `Direct<T>`, which always has a
current value and can be awaited for changes. The catalog, the active rendition,
the decoder backend, the open sessions and the routes to a path all work this
way. Discrete events are streams and channels: room events, sessions waiting
for admission.

Bounded channels only. Frames between the decoder and the renderer go through a
single-slot latest-wins channel rather than a queue, so a renderer that falls
behind skips to the newest picture instead of draining a backlog.
