# Paths, layouts and tickets

A *path* names a broadcast, such as `live/<endpoint id>/hello`. A *layout* is
the rule that says where each peer publishes, and which peer owns a path. A
layout is a path convention. A *ticket* is a string that carries a path, and
its publisher, to a viewer.

## Why paths name their publisher

The path of a broadcast is the same on every link, direct or through a
relay. A node therefore has to stop one peer from publishing at another
peer's path. Otherwise a viewer could read the wrong stream. iroh-live puts
the publisher's endpoint id into the path, and lets each peer publish only
under its own id.

## `LiveLayout`

`LiveLayout` is the layout iroh-live ships. It puts a peer's broadcasts at
`live/<endpoint id>/<name>`, and its policy lets every peer publish there
only. `LiveLayout::publish`, `LiveLayout::path` and `LiveLayout::ticket` all
use it:

```rust,no_run
# use iroh_live::{EndpointConfig, Node, layout::LiveLayout, media::LocalBroadcast};
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
# let endpoint = EndpointConfig::from_env()?.bind().await?;
let layout = LiveLayout::default();
let node = Node::new(endpoint, layout.config());
let broadcast: LocalBroadcast = layout.publish(&node, "hello")?; // live/<endpoint id>/hello
# Ok(())
# }
```

Rooms use a second layout, `RoomsLayout`, with paths
`rooms/<topic>/<endpoint id>/<name>`. A node that runs both takes the union:
`iroh_live::layout::config((LiveLayout::default(), RoomsLayout))`.

## Your own paths

The node itself takes any path. `Node::publish(path, audience)` and
`Node::subscribe(path, reach)` apply no layout. A layout of your own
implements the `Layout` trait: `scope` returns the
patterns a peer may publish under, and `owner` returns the peer that owns a
path. The two must agree: the owner of any path in `scope(peer)` is `peer`.
The agreement matters for [ticket claims](../reference/access.md#ticket-claims).
[Publish at your own path](../how-to/custom-paths.md) is a complete program.

A path pattern uses `*` for one segment and `**` for the rest of the path,
as in `live/*/hello` or `stage/<endpoint id>/**`.

## Broadcast tickets

A `BroadcastTicket` holds a publisher's endpoint id and the path of one of its
broadcasts. `LiveLayout::ticket(id, name)` builds the ticket for
`live/<id>/<name>`, and `Publication::ticket()` returns the ticket of any
publication. `Node::subscribe_ticket` subscribes to it from any node, whatever
that node's policy.

```rust,no_run
# use iroh_live::{BroadcastTicket, Node, layout::LiveLayout};
# fn f(node: Node, layout: LiveLayout) -> Result<(), Box<dyn std::error::Error>> {
let ticket = layout.ticket(node.id(), "hello")?;
println!("{ticket}"); // ilv1:<endpoint id>/live/~/hello
let parsed: BroadcastTicket = ticket.to_string().parse()?;
# Ok(())
# }
```

A broadcast ticket is not a secret. It names a publisher and a path, and the
publisher's policy and the publication's audience decide who may read it.

The ticket holds no network addresses. iroh finds them from the endpoint id:
through DNS and [pkarr](https://pkarr.org) (signed records published to a
DHT) on the internet, and through mDNS on a local network. That also keeps
the QR code small.

A *call ticket* is a broadcast ticket for the `call` path. `irl call` prints
one.

## Room tickets

A `RoomTicket` holds a gossip topic and a list of *bootstrap members*, the
members a new member joins through. `Room::ticket()` returns the room's ticket
with this member listed, so share that one with the people who join.

A room ticket is a bearer secret: anyone who has it can join the room and
read its broadcasts. Share it only with the people you invite.

## Where to go next

- [Access control](access.md) explains how a layout becomes a policy.
- [Ticket formats](../reference/tickets.md) gives the string forms and the
  limits on ticket claims.
- [Calls and rooms](calls-and-rooms.md)
