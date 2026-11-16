# Internals overview

These pages are for contributors, and for anyone who debugs the pipeline.
They name files and internal types, which the rest of the book avoids. The
[concepts](../concepts/overview.md) explain the model an application sees,
and these pages assume it.

iroh-live sits between two things it does not own: iroh for connectivity and
identity, and moq's media crates for capture, codecs and rendering. This
repository holds the layer that joins them.

## Crates and where they are explained

| Crate | Internals page |
|---|---|
| `iroh-live` | This page: the facade, its errors, and calls |
| `iroh-live-node` | [The node and its transport](transport.md), [Authorization and audiences, explained](auth-explained.md) |
| `moq-transport-iroh` | [The node and its transport](transport.md#alpn-negotiation) |
| `iroh-live-rooms` | [Authorization and audiences, explained](auth-explained.md#rooms) |
| `moq-ext-access` | [Authorization and audiences, explained](auth-explained.md) |
| `moq-ext-publisher`, `moq-ext-sources` | [Publishing](publish.md) |
| `moq-ext-player` | [Subscribing](subscribe.md), [Playout and A/V sync](playout.md) |
| `moq-ext-adaptive` | [Adaptive rendition switching](adaptive.md) |
| `moq-ext-json` | [Send data without media](../how-to/data.md#latest-value-state) |
| `moq-ext-egui` | [Instrumentation and tests](devtools.md) |
| `iroh-live-relay` | [Peer-to-peer and the relay](p2p-relay.md) |
| `iroh-live-cli` | [`irl` CLI reference](../reference/cli.md) |

[Crates and API docs](../reference/crates.md) says what each crate holds.

No `moq-ext-*` crate depends on iroh, so each could move upstream into moq,
the UI crate `moq-ext-egui` included. `moq-ext-player` and
`moq-ext-publisher` do not depend on each other. A transport publishes a
`LocalBroadcast` by reading it through `moq_net::Consume`. It builds a
`RemoteBroadcast` from a route table with `from_resolved`. `iroh-live-rooms`
has no media dependency. `iroh-live` depends on both sides and is the only
crate that joins them.

## Upstream media

Capture, encoding, decoding and GPU rendering come from `moq-video` and
`moq-audio`, upstream in the [moq](https://github.com/moq-dev/moq)
repository. This repository has no codec, capture backend or renderer of its
own. The upstream documentation covers them:

- [moq-video](https://doc.moq.dev/lib/rs/crate/moq-video): capture, encode,
  decode and render, the backend selection order, and the device enumerators.
- [moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio): microphone
  capture, encode and decode, the playback engine, and echo cancellation.
- [hang](https://doc.moq.dev/lib/rs/crate/hang) and
  [moq-mux](https://doc.moq.dev/lib/rs/crate/moq-mux): the catalog and the
  container formats.

`iroh_live::media` re-exports them as `iroh_live::media::video` and
`iroh_live::media::audio`. A crate that uses these paths gets the exact build
that `moq_video::render` links. This matters most for wgpu.
`moq_video::render` returns a `wgpu::Texture` from its own wgpu build, and a
texture from another wgpu major version is a different type.
[Platform support](../reference/platforms.md#codec-backends) describes how a
codec backend is picked.

## What the moq-ext crates add

Upstream covers one publisher with one rendition, and one subscriber that
takes what it is given. The `moq-ext-*` crates add three things on top.

- [Publishing](publish.md) encodes one source into a ladder of renditions.
- [Subscribing](subscribe.md) starts a player that picks among those
  renditions as the downlink changes, and swaps decoders without a blank
  frame.
- Each player's [playout clock](playout.md) keeps audio and video aligned
  across two separate decode paths.

## The facade

`iroh-live` holds `Calls` and the re-exports, and it is the only crate that
joins the media crates to the node. `LiveLayout` and `EndpointConfig` live in
`iroh-live-node`, so a service without media, such as `iroh-live-relay`, uses
them without the facade. [Nodes, sessions and routes](../concepts/nodes.md)
and [Paths, layouts and tickets](../concepts/paths.md) describe how an
application uses it.

`subscription.remote_broadcast()`, from the extension trait
`iroh_live::media::SubscriptionExt`, reads the media of a subscription
without waiting for the catalog. It works for a subscription from a ticket, a
room or `Node::subscribe`. The broadcast follows the path through the route
table and carries the serving link's measurements. So `remote.play(config)`
starts a player that adapts with nothing further to wire.

The facade's error is `iroh_live::Error`:

- `Node` wraps an `iroh_live_node::Error`.
- `Endpoint` wraps an `EndpointError`, which is `SecretKey` or `Bind`.
- `Media` wraps a `moq_ext_player::Error`.
- `Rooms` wraps an `iroh_live_rooms::Error`.
- `Calls` wraps an `iroh_live::calls::Error`.

Audiences, admission, relays and routes belong to
[`iroh-live-node`](transport.md). A call is one broadcast per side, shown
only to the peer it rings. [Authorization and audiences,
explained](auth-explained.md#calls) follows a call through the access model.

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
