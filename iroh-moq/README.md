# iroh-moq

[Media over QUIC](https://moq.dev/) transport over
[iroh](https://github.com/n0-computer/iroh).

A `Moq` node keeps one route table fed by every link it has: direct sessions
with peers, and moq relays it is attached to. The application picks the paths;
when a broadcast has the same path over every link, subscribing resolves a path
in the table rather than asking one session. moq picks the best route (lowest
cost, then fewest hops) and fails over when it dies.

```rust
use iroh_moq::{Audience, MediaPreset, Moq, MoqConfig, Reach};

let endpoint = iroh::Endpoint::bind(MediaPreset).await?;
let moq = Moq::new(endpoint.clone(), MoqConfig::default());

// Accept incoming sessions on every MoQ version this build speaks.
let mut router = iroh::protocol::Router::builder(endpoint);
for alpn in iroh_moq::alpns() {
    router = router.accept(alpn, moq.clone());
}
let router = router.spawn();

// Publish a broadcast this process writes, to everyone.
let broadcast = moq_net::broadcast::Info::new().produce();
let publication = moq.publish("demo/my-stream", &broadcast, Audience::Everyone)?;

// Or resolve a peer's, dialing it if no route exists yet.
let subscription = moq.subscribe("demo/camera", Reach::Both(peer)).await?;
let consumer = subscription.as_moq();
```

## Who sees what

A publication's `Audience` says who may see it: `Everyone`, a watched set of
`Peers`, or `Manual`, offered per session with `Session::offer`. A session's
`Grant`, decided when it is admitted, says what the peer may subscribe to and
publish, in moq-auth's pattern shape. `MoqConfig::grant` gives each peer its
grant from its endpoint id, so an application can keep every peer to the paths
that name it; `iroh-live` lets a peer publish under `live/<its id>/` only. With `Admission::Manual` every incoming
session waits in `Moq::accept` for the application to admit or reject it, which
is where a token check goes; `Grant::from_claims` behind the `auth` feature
turns verified moq-auth claims into a grant.

## Relays

`Moq::attach_relay(RelayConfig::new(url))`
stays attached to a moq relay at an `iroh://` or `https://` URL, over
moq-tokio's client: public publications go to the relay, the relay's routes
join the route table at a higher cost than a direct route, and the link redials
with backoff. `RelayLink::status()` watches its `RelayStatus`.

`RelayConfig::consume` is on by default, which copies every route the relay
knows into the node's route table, so every broadcast on the relay becomes
resolvable here. A node that only publishes through the relay should attach
with `RelayConfig { consume: false, ..RelayConfig::new(url) }`.

## Links

Every link, direct session or relay, runs a connection monitor that reads its
statistics five times a second into a `LinkSample`: round trip, recent minimum
round trip, loss, goodput, and the peer's delivery estimate. The measured
figures are `Option`s, where `None` means not measured yet, never zero.
`Session::link()` and `RelayLink::link()` return a link's latest sample, and
`Subscription::link()` returns the `ServingLink` (`id`, `kind`, `sample`) of
whichever link serves a subscription at the moment, relay-served ones
included. The monitor logs each sample at trace level as `link sample`.

## Driving moq-net directly

A `Moq` node dials and accepts on its own. An application that runs moq-net's
client or server itself, as `iroh-live-relay` does, uses
`iroh_moq::transport::{dial, accept}` for the same ALPN negotiation and HTTP/3
handling. They return a `web_transport_iroh::Session`, so their signatures
follow web-transport-iroh's versioning rather than this crate's.

## Endpoints and tickets

`MediaPreset` is iroh's N0 preset with a BBR3 transport tuned for live media;
`EndpointOptions` adds a secret key and mDNS. A `BroadcastTicket` names a
publisher and a broadcast, `iroh-live:<endpoint id>/<name>`, and maps to the
path `live/<endpoint id>/<name>`.

## ALPN

`iroh_moq::ALPN` is `moq_net::ALPNS[0]`, the newest MoQ version this build
speaks, so it tracks the dependency rather than a string someone has to remember
to bump. `iroh_moq::alpns()` returns the whole list newest first, with HTTP/3
last. Register all of them, or a peer built against a different moq release will
not find a version in common.
