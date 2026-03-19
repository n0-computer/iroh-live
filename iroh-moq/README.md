# iroh-moq

[Media over QUIC](https://moq.dev/) transport layer built on [iroh](https://github.com/n0-computer/iroh).

This crate connects iroh's QUIC endpoint to the [moq-lite](https://github.com/kixelated/moq) broadcast primitives. It manages sessions, routes incoming connections via iroh's `Router`, and provides the publish/subscribe interface that `iroh-live` builds on.

## High-level concepts

The **MoQ transport** (`Moq`) owns the iroh endpoint integration. It publishes local broadcasts, subscribes to remote ones, and accepts incoming sessions through a `MoqProtocolHandler` registered with iroh's router.

A **session** (`MoqSession`) represents a connection to a single remote peer, created either by calling `Moq::connect()` for outgoing connections or by accepting an `IncomingSession`. Each session can publish and subscribe to multiple named broadcasts independently.

Before accepting an incoming connection, you can inspect the remote peer's identity through the **incoming session** handle (`IncomingSession`) and decide whether to `accept()` or `reject()` it.

## Usage

```rust
use iroh::Endpoint;
use iroh_moq::Moq;

let endpoint = Endpoint::builder().discovery_n0().bind().await?;
let moq = Moq::new(endpoint.clone());

// Register as a protocol handler on the router
let handler = moq.protocol_handler();
let router = iroh::protocol::Router::builder(endpoint)
    .accept(iroh_moq::ALPN, handler)
    .spawn()
    .await?;

// Publish a local broadcast
moq.publish("my-stream", producer).await?;

// Connect to a remote peer and subscribe
let mut session = moq.connect(remote_addr).await?;
let consumer = session.subscribe("my-stream").await?;
```

The ALPN is `moq-lite-03`, matching the moq-lite protocol version.
