# Peer-to-peer and the relay

## Direct connectivity

iroh connects peers directly when it can. Two machines on one network reach
each other over the local link, and hole punching opens a direct UDP path
through most NATs. When that fails, traffic goes through an iroh relay, which
forwards opaque packets and adds its own round trip.

The media pipeline does not see the change. It sees a different round trip and
bandwidth, and the publisher's delivery estimate follows the bandwidth, which
is what [adaptive rendition switching](adaptive.md) reads. The link monitor in
`iroh-moq/src/link.rs` counts every change of selected path as a new path
generation, and the selector forgets what it learned on the old path. The
monitor's `link sample` TRACE log says whether the selected path is relayed.

An iroh relay and a MoQ relay are unrelated. The first forwards UDP between
peers that cannot reach each other and knows nothing about media. The second
is described below.

## iroh-live-relay

Browsers cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection, not an endpoint, so there is no hole punching and no way to accept
a connection. `iroh-live-relay` runs a `moq_relay::Cluster` behind a
WebTransport listener and an iroh endpoint, so a broadcast that arrives over
one is reachable from the other. Browsers come in through moq-tokio's server,
and iroh clients through `IrohSessions` (`src/iroh_sessions.rs`).

`IrohSessions` accepts iroh sessions itself, so iroh has authenticated each
client's endpoint id, and the relay lets a client publish only at paths that
name it. The [relay README](../../iroh-live-relay/README.md) covers the rest of
running it: flags, the certificate, the key and the web client.

## Pull on demand

The relay does not need to know a publisher in advance. When a session asks for
a name that parses as a `BroadcastTicket`, the relay subscribes to the ticket's
broadcast through its own iroh-moq node and mirrors it into the cluster under
that name (`src/pull.rs`). From then on it is ordinary relay fan-out: a second
viewer of the same ticket shares the first one's upstream session.

Concurrent pulls for one ticket share one dial, so two browsers that arrive
together open one upstream session. The mirror carries only the one broadcast
the ticket names. A pull ends once no session has named the ticket and nothing
has read the broadcast for ten seconds, which lets a page reload reuse it. When
the last pull of a publisher ends, the relay closes its session with that
publisher.

## Publishing to a relay

A publisher reaches a relay through a relay link.
`Moq::attach_relay(RelayConfig::new(url))` stays attached and redials with
backoff, and every publication with the `Everyone` audience is offered to it.
`irl publish --relay <ENDPOINT_ID>` attaches to `iroh://<ENDPOINT_ID>/` this
way. A relay link consumes by default: it copies every route the relay knows
into the node's route table. `irl publish` only publishes, so it attaches with
`consume: false`. The [browser relay guide](../guide/browser-relay.md) has the
workflow.
