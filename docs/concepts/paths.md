# Paths and layouts

A *path* names a broadcast, such as `live/<endpoint id>/hello`. A *layout* is
the rule that says where each peer publishes, and which peer owns a path.

## Why paths name their publisher

The path of a broadcast is the same on every link, direct or through a
relay. A node therefore needs a way to stop one peer from publishing at a
path that belongs to another, since a viewer would then read the wrong
stream. iroh-live puts the publisher's endpoint id into the path, and lets
each peer publish only under its own id.

## `LiveLayout`

`LiveLayout` is the layout iroh-live ships. It puts a peer's broadcasts at
`live/<peer>/<name>`, and its policy lets every peer publish there only.
`LiveLayout::publish`, `LiveLayout::path` and `LiveLayout::ticket` all use it:

```rust,ignore
let layout = LiveLayout::default();
let node = Node::new(endpoint, layout.config());
let broadcast: LocalBroadcast = layout.publish(&node, "hello")?; // live/<id>/hello
```

Rooms use a second layout, `RoomsLayout`, with paths
`rooms/<topic>/<peer>/<name>`. A node that runs both takes the union:
`iroh_live::layout::config((LiveLayout::default(), RoomsLayout))`.

## Your own paths

The node itself takes any path. `Node::publish(path, audience)` and
`Node::subscribe(path, reach)` apply no layout. A layout of your own
implements the `Layout` trait: `scope` returns the patterns a peer may
publish under, and `owner` returns the peer that owns a path. The two must
agree: the owner of any path in `scope(peer)` is `peer`.
[Publish at your own path](../how-to/custom-paths.md) is a complete program.

A path pattern uses `*` for one segment and `**` for the rest of the path,
as in `live/*/hello` or `stage/<id>/**`.

## Where to go next

- [Access control](access.md) explains how a layout becomes a policy.
- [Tickets](tickets.md) explains how a path travels to a viewer.
