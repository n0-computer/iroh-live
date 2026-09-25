# iroh-moq

[Media over QUIC](https://moq.dev/) over [iroh](https://github.com/n0-computer/iroh).

A `Moq` node publishes broadcasts at paths and subscribes to paths. It keeps
one route table fed by every link it has: direct sessions with peers, and moq
relays it is attached to. A path resolves to its cheapest route, and moq moves
to the next one when that route dies. The application picks the paths, and
nothing here knows about media: a broadcast holds whatever tracks you write.

```rust
use iroh_moq::{Audience, Moq, MoqConfig, MoqPreset, Reach};

let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
let moq = Moq::new(endpoint.clone(), MoqConfig::default());

// Accept sessions on every MoQ version this build speaks.
let mut router = iroh::protocol::Router::builder(endpoint);
for alpn in iroh_moq::alpns() {
    router = router.accept(alpn, moq.clone());
}
let router = router.spawn();

// Publish a broadcast this process writes, to everyone.
let broadcast = moq_net::broadcast::Info::new().produce();
let publication = moq.publish("demo/my-stream", &broadcast, Audience::Everyone)?;

// Resolve a peer's broadcast, dialing the peer if no route exists yet.
let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
let consumer = subscription.as_moq();
```

## Who sees what

A publication's `Audience` says who sees it: `Everyone`, a watched set of
`Peers`, or `Manual`, offered per session with `Session::offer`. A session's
`Grant` says what the peer may subscribe to and publish, in moq-auth's pattern
form. `MoqConfig::grant` gives each peer its grant from its endpoint id.
`iroh-live`, for example, lets a peer publish under `live/<its id>/` only, so
no peer can stand in for another.

With `Admission::Manual`, incoming sessions wait in `Moq::accept` until the
application admits or rejects them, for example after checking a token.
`Grant::from_claims`, behind the `auth` feature, turns verified moq-auth claims
into a grant.

## Relays

`Moq::attach_relay(RelayConfig::new(url))` stays attached to a moq relay at an
`iroh://` or `https://` URL, and redials it with backoff. Public publications go
to the relay, and the relay's routes join the route table at a higher cost than
a direct route. A node that only publishes through the relay sets
`RelayConfig::consume` to false.

## Links

Every link runs a connection monitor. `Session::link()` and `RelayLink::link()`
return its latest `LinkSample`: round trip, minimum round trip, loss, goodput
and the peer's delivery estimate, with `None` for what is not measured yet.
`Subscription::link()` returns the sample of whichever link serves the
subscription.

## Endpoints and transport

`MoqPreset` is iroh's N0 preset with BBR3, so the send-rate estimate moq-net
gives subscribers tracks the link. `EndpointOptions` adds a secret key and mDNS.
`iroh_moq::transport::{dial, accept}` do the ALPN negotiation and HTTP/3
handling for applications that run moq-net's client or server themselves, as
`iroh-live-relay` does.

`iroh_moq::alpns()` lists every MoQ version this build speaks, newest first,
then HTTP/3. Mount the node under all of them, so peers on other moq releases
still connect.
