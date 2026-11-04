# moq-transport-iroh

[Media over QUIC](https://moq.dev/) sessions over [iroh](https://github.com/n0-computer/iroh)
connections. It turns an iroh connection into a `moq_net::Session` and knows
nothing about nodes, routes or publications.

Use it when you want plain MoQ over iroh, for example a relay that runs
moq-net's server itself, or a client that needs no route table. For a peer that
publishes and subscribes at paths, use [`iroh-live-node`](../iroh-live-node),
which is built on this crate.

## What is in it

- `dial` and `accept`: negotiate raw QUIC or HTTP/3 by ALPN and complete the
  WebTransport handshake. `dial_with` carries a token for an HTTP/3 CONNECT.
- `poll_session`: wraps the result for `moq_net::Client` or `moq_net::Server`,
  through a vendored copy of moq-tokio's async-to-poll adapter. It goes away
  once web-transport-iroh speaks moq-net's poll traits itself.
- `ALPN`, `alpns()`, `HANDSHAKE_TIMEOUT`: the protocol identifiers to mount a
  handler under, newest MoQ version first, then HTTP/3.
- `MoqPreset`: iroh's N0 preset with BBR3, so the send-rate estimate moq-net
  gives subscribers tracks the link.
- `LinkSampler`, `LinkSample`, `watch`: sample a connection's selected path
  every 200 ms into round trip, minimum round trip, loss, goodput and the
  peer's delivery estimate.

```rust
use moq_transport_iroh::{MoqPreset, dial, poll_session};

let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
let session = dial(&endpoint, peer).await?;
// Hand `poll_session(session, false)` to `moq_net::Client::connect`.
```

See `iroh-live-relay/src/iroh_sessions.rs` for the server side.

## Features

None.
