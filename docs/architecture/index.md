# Architecture

iroh-live sits between two things it does not own: iroh for connectivity and
identity, and [moq-video and moq-audio](https://doc.moq.dev/lib/rs/) for media.
This repository holds the layer that joins them.

## Crates

[Crates and API docs](../reference/crates.md) lists the crates and what each
holds.

No `moq-ext-*` crate depends on iroh, so each could move upstream into moq,
the UI crate `moq-ext-egui` included. `moq-ext-player` and `moq-ext-publisher`
do not depend on each other. A transport publishes a `LocalBroadcast` by
reading it through `moq_net::Consume`, and builds a `RemoteBroadcast` from a
route table with `from_resolved`. `iroh-live-rooms` has no media dependency.
`iroh-live` depends on both and is the only crate that joins them.

## What the moq-ext crates add

Upstream covers one publisher with one rendition, and one subscriber that takes
what it is given. The `moq-ext-*` crates add three things on top.

[Publishing](publish.md) encodes one source into a simulcast ladder.
[Subscribing](subscribe.md) starts a player that picks among those renditions
as the downlink changes, and swaps decoders without a blank frame. Each
player's [playout clock](playout.md) keeps audio and video aligned across two
separate decode paths.

The rest is upstream. See [the media stack](media-stack.md) for what we use.

## iroh-live

`iroh-live` is the facade. It holds `LiveLayout`, `Calls`, `EndpointConfig`
and the re-exports, and it is the only crate that joins the media crates to
the node. [Nodes and endpoints](../concepts/nodes.md) and
[Paths and layouts](../concepts/paths.md) describe how an application uses
it.

`subscription.remote_broadcast()`, from the extension trait
`iroh_live::media::SubscriptionExt`, reads the media of a subscription without
waiting for the catalog, for a subscription from a ticket, a room or
`Node::subscribe`. The broadcast follows the path through
the route table and carries the serving link's measurements, so
`remote.play(config)` starts a player that adapts with nothing further to
wire.

The facade's error is `iroh_live::Error`: `Node` wraps an
`iroh_live_node::Error`, `Media` a `moq_ext_player::Error`, `Rooms` an
`iroh_live_rooms::Error` and `Calls` an `iroh_live::calls::Error`. `SecretKey`
and `Bind` come from `EndpointConfig`. Audiences, admission, relays and routes
belong to [`iroh-live-node`](transport.md).

A call is one broadcast per side at `<root>/<id>/call`, published with
`Audience::Peers` over a set that starts empty. `Calls::ring` adds the peer
to the set, and the peer sees the path appear in its route table. A hang-up
takes the peer out of the set, which retracts the path and ends the other
side's read. The session stays open, and the broadcast keeps running for the
next call. [Authorization and audiences, explained](auth-explained.md#calls)
follows a call through the access model.

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
