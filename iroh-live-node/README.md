# iroh-live-node

The iroh-live node: [Media over QUIC](https://moq.dev/) over
[iroh](https://github.com/n0-computer/iroh) with one route table fed by direct
sessions and relays.

A `Node` publishes broadcasts at paths and subscribes to paths. It keeps
one route table fed by every link it has: direct sessions with peers, and moq
relays it is attached to. A path resolves to its cheapest route. When that
route dies, moq moves to another route of the same instance without ending the
subscription, and `Subscription::closed` asks the table again for any other.
`Publication::unpublish_on_drop` ties a publication to a task's lifetime, and
`Node::publish_value` publishes a latest-value JSON track that
`Subscription::value_track` reads.
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

// Publish a broadcast this process writes, to every granted session. It is
// created in the route table, and peers read it without a copy.
let publication = moq.publish("demo/my-stream", Audience::Granted)?;
let track = publication.broadcast().create_track("video", None)?;

// Resolve a peer's broadcast, dialing the peer if no route exists yet.
let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
let consumer = subscription.broadcast();
```

## Who sees what

A publication's `Audience` says who sees it: `Granted`, or a watched set of
`Peers`. A session's `Grant` says what the peer may subscribe to and publish,
in moq-auth's pattern form. `Audience` and `Grant` come from
[`moq-ext-access`](../moq-ext-access). The node's policy
(`NodeConfig::with_policy`) gives each peer its grant, and without one a peer
may read anything and publish nothing. `iroh-live`, for example, lets a peer
publish under `live/<its id>/` only, so no peer can stand in for another.

moq enforces it on each live session: the node authorizes every session, dialed
or accepted, for exactly the publications whose audience admits its peer, and
authorizes it again whenever a publication or an audience changes. A peer taken
out of an audience loses the path at once, and what it read there ends with
`Unauthorized`. Every link serves only the node's own publications, never a
route the node learned from another peer or a relay.

The policy decides each session when it opens, in either direction, and
again when its authorization expires and on `Node::reauthorize`. A new
decision replaces the session's authorization, and a rejection closes it.
`policy::Open`, `Closed`, `from_fn` and `filter` compose policies:

```rust
use iroh_live_node::{Grant, NodeConfig, policy};

// A node on a trusted network: every peer may publish and read anything.
let config = NodeConfig::default().with_policy(policy::Open(Grant::everything()));
```

To decide in the application, set a `policy::Queue` as the node's policy. Each
request waits in the queue until the application admits or rejects the
`Pending` that `Queue::next` yields. The node gives up on a decision after
`NodeConfig::decide_timeout`, 10 seconds by default, so raise it when a person
answers. `policy::Verify` lets a token decide, with JWT and rcan verifiers
behind features.

[Access control](../docs/how-to/control-access.md) covers the policies, tokens and the
trust model.

## Relays

`Node::attach_relay(RelayConfig::new(url))` stays attached to a moq relay at an
`iroh://` or `https://` URL, and redials it with backoff. Both kinds run
moq-net's connect policy (`moq_net::Reconnect`), which also follows a relay's
GOAWAY. `RelayConfig::iroh(id)` builds the `iroh://` URL from an endpoint id.
`RelayConfig::tokens` are the tokens the link presents on every dial, and
`RelayLink::tokens()` replaces them on a live link. Publications with the
`Granted` audience go to the relay, and the relay's routes join the route
table at a higher cost than a direct route. A node that only publishes through
the relay sets `RelayConfig::consume` to false.

A relay is trusted with every path it forwards. On a relay where anyone may
publish anywhere, a peer can publish `live/<victim>/cam`, and a subscriber whose
relay route arrives before its direct one reads the forgery. Attach only relays
that keep each publisher to its own paths, as `iroh-live-relay` and moq-relay's
tokens do.

## Links

Every link runs a connection monitor. `Subscription::serving_link()` returns a
`ServingLink` for whichever link serves the subscription, with the link's id,
kind and latest `NetworkSample`: round trip, minimum round trip, loss,
throughput and the peer's delivery estimate, with `None` for what is not
measured yet. `Session::moq()` gives the moq-net session itself, with its raw
statistics. `Node::accepted()` yields each accepted session once.
`NetworkSample` comes from `moq-ext-adaptive`, and `alpns()`, `ALPN` and
`HANDSHAKE_TIMEOUT` from `moq-transport-iroh`. This crate re-exports them.
`MoqPreset` is iroh's N0 preset with BBR3 congestion control, so the send-rate
estimate moq-net gives subscribers tracks the link. `Congestion` names BBR3,
CUBIC and NewReno, for a dial that compares controllers through
`ConnectOptions::quic`.

## Examples

`examples/delegated_publisher.rs` lets a device publish into another
account's space on an rcan capability chain. Run it with
`cargo run -p iroh-live-node --features rcan --example delegated_publisher`.

## Features

| Feature | Effect |
|---|---|
| `jwt` | `policy::JwtVerifier`, which verifies moq-relay's JWTs |
| `rcan` | `policy::RcanVerifier`, `MoqCapability` and `encode_chain`, for capability chains bound to the peer's key |
| `test-util` | Helpers for tests that run nodes in one process. Not a public API |
