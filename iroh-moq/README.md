# iroh-moq

[Media over QUIC](https://moq.dev/) transport over
[iroh](https://github.com/n0-computer/iroh).

A `Moq` node keeps one route table fed by every link it has: direct sessions
with peers, and moq relays it is attached to. Broadcasts are published at paths
that name their publisher, `live/<endpoint id>/<name>`, so the same broadcast has
the same path over every link, and subscribing resolves a path in the table
rather than asking one session. moq picks the best route (lowest cost, then
fewest hops) and fails over when it dies.

```rust
use iroh_moq::{Audience, BroadcastTicket, MediaPreset, Moq, MoqConfig, Reach};

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
let publication = moq.publish("my-stream", &broadcast, Audience::Everyone)?;
println!("{}", publication.ticket().expect("a live path"));

// Or resolve someone else's, dialing its publisher if no route exists yet.
let subscription = moq.subscribe(ticket.path(), Reach::default()).await?;
let consumer = subscription.as_moq();
```

## Who sees what

A publication's `Audience` says who may see it: `Everyone`, a watched set of
`Peers`, or `Manual`, offered per session with `Session::offer`. A session's
`Grant`, decided when it is admitted, says what the peer may subscribe to and
publish, in moq-auth's pattern shape. With `Admission::Manual` every incoming
session waits in `Moq::accept` for the application to admit or reject it, which
is where a token check goes; `Grant::from_claims` behind the `auth` feature
turns verified moq-auth claims into a grant.

## Relays

Behind the `relay-links` feature, `Moq::attach_relay` stays attached to a moq
relay at an `iroh://` or `https://` URL, over moq-tokio's client: public
publications go to the relay, the relay's routes join the route table at a
higher cost than a direct route, and the link redials with backoff.

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
