# Nodes and endpoints

An iroh *endpoint* is a QUIC socket with a key pair. Its public key is the
*endpoint id*, which identifies the node to every peer. A `Node` is the
iroh-live state on top of one endpoint: its route table, its publications, its
sessions and its policy.

## Identity

The QUIC handshake proves that a peer holds the key of its endpoint id. A
node therefore always knows who is on the other side, and access control
starts from that id. Tickets and paths name publishers by endpoint id, so a
new key means new tickets.

`EndpointConfig::from_env()` reads the key from `IROH_SECRET`. Without it,
`EndpointConfig::default()` makes a new key on every start. To keep a key in a
file, pass `iroh_live::load_or_create_secret_key(path)` as
`EndpointConfig::with_secret_key`.

## Binding the endpoint

`EndpointConfig::bind()` binds an endpoint with `MoqPreset`: iroh's default
relays and address lookup, with BBR3 congestion control. It adds mDNS, so
peers on one network find each other without internet access. To set other
iroh options, `EndpointConfig::apply(builder)` adds the same settings to
your own `iroh::endpoint::Builder`. Make your own settings after `apply`.

## Mounting the node

`Node::new(endpoint, config)` creates the node. It does not accept connections on its own. `Node::mount` adds its protocols
to an iroh `RouterBuilder` that the application owns, and the application can
add other protocols to the same router.

```rust,ignore
let node = Node::new(endpoint.clone(), layout.config());
let router = node.mount(Router::builder(endpoint)).spawn();
```

A node that only watches does not need a router, since it only dials. A node
that publishes needs one, or a relay link, or nobody can reach it. Such a node
logs a warning when it publishes.

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
- [Environment variables](../reference/environment.md) lists `IROH_SECRET`.
