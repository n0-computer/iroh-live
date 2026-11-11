# moq-transport-iroh

[Media over QUIC](https://moq.dev/) sessions over [iroh](https://github.com/n0-computer/iroh)
connections. It turns an iroh connection into a `moq_net::Session` and knows
nothing about nodes, routes or publications.

Use it when you want plain MoQ over iroh, for example a relay that runs
moq-net's server itself, or a client that needs no route table. For a peer that
publishes and subscribes at paths, use [`iroh-live-node`](../iroh-live-node),
which is built on this crate.

## What is in it

- `dial` and `accept`: check the negotiated ALPN and return a `Transport`.
- `Transport`: moq-net's poll transport (`moq_net::transport::poll::Session`)
  implemented on the iroh connection and its streams. Hand it to
  `moq_net::Client::connect` or `moq_net::Server::accept_request`. MoQ runs on
  raw QUIC, so the request path and any token travel in the MoQ SETUP.
- `ALPN`, `alpns()`, `HANDSHAKE_TIMEOUT`: the protocol identifiers to mount a
  handler under, newest MoQ version first.
- `MoqPreset`: iroh's N0 preset with BBR3, so the send-rate estimate moq-net
  gives subscribers tracks the link.
- `LinkSampler`, `LinkSample`, `sample_link`: sample a connection's selected path
  every 200 ms into round trip, minimum round trip, loss, goodput and the
  peer's delivery estimate.

```rust
use moq_transport_iroh::{MoqPreset, dial};

let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
let transport = dial(&endpoint, peer).await?;
let (session, driver) = moq_net::Client::new()
    .connect(moq_net::time::Instant::now(), transport)
    .await?;
```

iroh has no send groups, so `send_group` returns `None` and streams order by
their own priority. `probe` and `receive_window` return `None`: iroh's
congestion controller takes no probe target, and iroh does not report its
receive windows.

See `iroh-live-relay/src/iroh_sessions.rs` for the server side.

## Features

None.
