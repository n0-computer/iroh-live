# iroh-moq

[Media over QUIC](https://moq.dev/) transport over
[iroh](https://github.com/n0-computer/iroh).

`Moq` binds an iroh `Endpoint` to a MoQ origin. Broadcasts created with
`Moq::publish` are announced to every peer, and `MoqSession` reaches the ones a
peer announces back. An internal actor owns session lifetime, so a second
`Moq::connect` to a peer we already have a session with returns that session
rather than opening a second connection.

```rust
use iroh_moq::Moq;

let moq = Moq::new(endpoint.clone());

// Accept incoming sessions on every MoQ version this build speaks.
let mut router = iroh::protocol::Router::builder(endpoint);
for alpn in iroh_moq::alpns() {
    router = router.accept(alpn, moq.protocol_handler());
}
let router = router.spawn();

// Publish, announced to every peer with a session.
let producer = moq.publish("my-stream")?;

// Or reach a peer's.
let session = moq.connect(remote_addr).await?;
let consumer = session.subscribe("my-stream").await?;
```

Publishing is a property of the node rather than of a connection: a moq-net
session takes exactly one publisher origin and the node origin is it. There is no
per-session publish.

## ALPN

`iroh_moq::ALPN` is `moq_net::ALPNS[0]`, the newest MoQ version this build
speaks, so it tracks the dependency rather than a string someone has to remember
to bump. `iroh_moq::alpns()` returns the whole list newest first, with HTTP/3
last. Register all of them, or a peer built against a different moq release will
not find a version in common.

Both halves of the handshake branch on what was negotiated: raw QUIC carries the
MoQ stream directly, and H3 answers a CONNECT first.
