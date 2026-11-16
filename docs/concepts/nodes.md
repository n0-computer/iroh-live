# Nodes, sessions and routes

An iroh *endpoint* is a QUIC socket with a key pair. Its public key is the
*endpoint id*, which identifies the node to every peer. A `Node` is the
iroh-live state on top of one endpoint: its route table, its publications, its
sessions and its policy.

## Identity

The QUIC handshake proves that a peer holds the key of its endpoint id. A
node therefore always knows who is on the other side, and access control
starts from that id. Tickets and paths name publishers by endpoint id, so a
new key means new tickets.

`EndpointConfig::from_env()` reads the key from `IROH_SECRET`, as
[Environment variables and files](../reference/environment.md) describes.
Without it, `EndpointConfig::default()` makes a new key on every start. To
keep a key in a file, pass `iroh_live::load_or_create_secret_key(path)` as
`EndpointConfig::with_secret_key`.

## Binding the endpoint

`EndpointConfig::bind()` binds an endpoint with `MoqPreset`: iroh's default
relays and address lookup, with BBR3 congestion control. BBR3 estimates the
path's bandwidth, which the player uses to pick a rendition (see
[Adaptive rendition switching](../architecture/adaptive.md#signals)). The
preset adds mDNS, so peers on one network find each other without internet
access. To set other iroh options, `EndpointConfig::apply(builder)` adds the
same settings to your own `iroh::endpoint::Builder`. Make your own settings
after `apply`.

## Mounting the node

`Node::new(endpoint, config)` creates the node. It does not accept
connections on its own. `Node::mount` adds its protocols to an iroh
`RouterBuilder` that the application owns, and the application can add other
protocols to the same router.

```rust,no_run
# use iroh::protocol::Router;
# use iroh_live::{EndpointConfig, Node, layout::LiveLayout};
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
# let endpoint = EndpointConfig::from_env()?.bind().await?;
# let layout = LiveLayout::default();
let node = Node::new(endpoint.clone(), layout.config());
let router = node.mount(Router::builder(endpoint)).spawn();
# Ok(())
# }
```

A node that only watches does not need a router, since it only dials. A node
that publishes needs a router or a relay link. Without either, nobody can
reach it, and the node logs a warning when it publishes.

## Sessions

A *session* is a MoQ connection with one peer, which is another node or a
relay. A node opens a session when it dials a peer, or accepts one when a
peer dials it. Each session carries the peer's announcements into the node's
*route table*, within what the node's [policy](access.md) lets that peer
publish. In the other direction, the session offers the peer the node's own
publications that the peer may see.

A node never forwards. It offers a peer only its own publications, never a
route it learned from another link.

## Routes and relay links

A *route* is a way to reach a broadcast: a session or a relay link that
announces its path. The node keeps every route it learns in its route table,
and a subscription reads from the best route there.

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

A subscription follows its path, not one route. One announcement of a
broadcast by its publisher is an *instance*, which can reach the node over
several routes. When the serving route goes, the subscription moves to
another route of the same instance, for example from a relay to a direct
session, and does not end. When the publisher restarts the broadcast, the
subscription moves to the new instance.

A subscription ends only when no route serves the path. The node asks the
route table again 100 ms after a route goes, and the player gives up after
three seconds without one (see [Subscribing](../architecture/subscribe.md)).

`Subscription::serving_link()` returns the link that serves a subscription
now, with its round trip and loss. The *serving link* is also what the player
measures to pick a rendition.

## Shutting down

Whoever binds the endpoint closes it. `Router::shutdown` shuts down the node
and every protocol mounted on the router, then closes the endpoint.
`Node::shutdown` ends the node's sessions and relay links, and leaves the
endpoint open. A node without a router calls `Node::shutdown` and then
`Endpoint::close`.

Most types clean up when dropped. Dropping the last clone of a broadcast
unpublishes it, and dropping a player stops its decoders.

## Where to go next

- [Quickstart: library](../getting-started/library-quickstart.md) has
  complete programs.
- [Send data without media](../how-to/data.md#without-the-media-crates) shows
  `serving_link` in a program.
- [The node and its transport](../architecture/transport.md) covers the route
  table, costs and announcements in depth.
