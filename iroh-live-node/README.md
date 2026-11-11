# iroh-live-node

The iroh-live node: [Media over QUIC](https://moq.dev/) over
[iroh](https://github.com/n0-computer/iroh) with one route table fed by direct
sessions and relays.

A `Node` publishes broadcasts at paths and subscribes to paths. It keeps
one route table fed by every link it has: direct sessions with peers, and moq
relays it is attached to. A path resolves to its cheapest route. When that
route dies, moq moves to another route of the same instance without ending the
subscription, and `Subscription::closed` asks the table again for any other.
The application picks the paths, and nothing here knows about media: a
broadcast holds whatever tracks you write.

Use it for peer-to-peer MoQ with many sessions, relays, admission and per-peer
visibility. For a single plain MoQ session without a route table, use
[`moq-transport-iroh`](../moq-transport-iroh), which this crate is built on.
For media, use
[`iroh-live`](../iroh-live), which re-exports this crate as `iroh_live::node`.

```rust
use iroh_live_node::{Audience, Node, NodeConfig, MoqPreset, Reach};

let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
let moq = Node::new(endpoint.clone(), NodeConfig::default());

// Accept sessions on every MoQ version this build speaks.
let router = moq.mount(iroh::protocol::Router::builder(endpoint)).spawn();

// Publish a broadcast this process writes, to everyone. It is created in the
// route table, and peers read it without a copy.
let publication = moq.publish("demo/my-stream", Audience::Everyone)?;
let track = publication.broadcast().create_track("video", None)?;

// Resolve a peer's broadcast, dialing the peer if no route exists yet.
let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
let consumer = subscription.broadcast();
```

## Who sees what

A publication's `Audience` says who sees it: `Everyone`, or a watched set of
`Peers`. A session's `Grant` says what the peer may subscribe to and publish,
in moq-auth's pattern form. `Audience` and `Grant` come from
[`moq-ext-access`](../moq-ext-access). `NodeConfig::grant` gives each peer its
grant from its endpoint id, and without it a peer may read anything and publish
nothing. `iroh-live`, for example, lets a peer publish under `live/<its id>/`
only, so no peer can stand in for another.

moq enforces it on each live session: the node authorizes every session, dialed
or accepted, for exactly the publications whose audience admits its peer, and
authorizes it again whenever a publication or an audience changes. A peer taken
out of an audience loses the path at once, and what it read there ends with
`Unauthorized`. Every link serves only the node's own publications, never a
route the node learned from another peer or a relay.

To decide in the application, for example after checking a token, set a
`policy::Queue` as the node's policy. Each request waits in the queue until the
application admits or rejects the `Pending` that `Queue::next` yields. The node
gives up on a decision after `NodeConfig::decide_timeout`, 10 seconds by
default, so raise it when a person answers.

## Relays

`Node::attach_relay(RelayConfig::new(url))` stays attached to a moq relay at an
`iroh://` or `https://` URL, and redials it with backoff. `RelayConfig::iroh(id)`
builds the `iroh://` URL from an endpoint id. Public publications go to the
relay, and the relay's routes join the route table at a higher cost than a
direct route. A node that only publishes through the relay sets
`RelayConfig::consume` to false.

A relay is trusted with every path it forwards. On a relay where anyone may
publish anywhere, a peer can publish `live/<victim>/cam`, and a subscriber whose
relay route arrives before its direct one reads the forgery. Attach only relays
that keep each publisher to its own paths, as `iroh-live-relay` and moq-relay's
tokens do.

## Links

Every link runs a connection monitor. `Session::link_sample()` and
`RelayLink::link_sample()` return its latest `LinkSample`: round trip, minimum
round trip, loss, throughput and the peer's delivery estimate, with `None` for
what is not measured yet. `Subscription::serving_link()` returns the sample of
whichever link serves the subscription. `LinkSample`, `MoqPreset`, `alpns()` and `ALPN` come from
`moq-transport-iroh` and are re-exported here.

## Features

None.
