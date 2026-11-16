# Routes, sessions and reach

A *session* is a MoQ connection between two nodes. A *route* is a way to
reach a broadcast: a session or a relay link that announces its path. The
node keeps every route it learns in one *route table*, and a subscription
reads from the best route there.

## Sessions

A node opens a session when it dials a peer, or accepts one when a peer dials
it. Each session carries the peer's announcements into the route table,
within what the node's [policy](access.md) lets that peer publish. In the
other direction, the session offers the peer the node's own publications that
the peer may see.

A node never forwards. It offers a peer only its own publications, never a
route it learned from another link.

## Relay links

A *relay link* is a session with a relay that the node keeps open.
`Node::attach_relay` creates one, and it redials with backoff when the
session drops. The relay's routes enter the same table at a higher cost than
a direct session, so a direct route wins while it exists. See
[Relays](relays.md).

## Reach

`Node::subscribe(path, reach)` looks the path up in the route table. If no
route is there yet, `Reach` says where to look:

| `Reach` | Effect |
|---|---|
| `Reach::Direct(id)` | Dial the publisher `id` |
| `Reach::Relays` | Wait for an attached relay to announce the path |
| `Reach::Both(id)` | Dial `id`, and also take a relay's route |

`Node::subscribe_ticket` uses the ticket's publisher and dials it.

## Switching routes

A subscription follows its path, not one route. When the serving route goes,
the subscription moves to another route of the same broadcast, for example
from a relay to a direct session, and does not end. When the publisher
restarts the broadcast, the subscription moves to the new one. It ends only
when no route serves the path.

`Subscription::serving_link()` says which link serves a subscription now,
with its round trip and loss. The player reads the same measurements to pick
a rendition.

## Where to go next

- [The transport architecture](../architecture/transport.md) covers the route
  table, costs and announcements in depth.
- [Use the node without media](../how-to/node-only.md) shows
  `serving_link` in a program.
