# Peer-to-peer and the relay

## Direct connectivity

iroh connects peers directly when it can. Two machines on one network reach
each other over the local link, and hole punching opens a direct UDP path
through most NATs. When that fails, traffic goes through an iroh relay, which
forwards opaque packets and adds its own round trip.

The media pipeline does not see the change. It sees a different round trip and
bandwidth, and the publisher's delivery estimate follows the bandwidth, which
is what [adaptive rendition switching](adaptive.md) reads. The link monitor in
`moq-transport-iroh/src/link.rs` counts every change of selected path as a new path
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
broadcast through its own iroh-live-node node (`src/pull.rs`). That node uses the
cluster origin as its route table (`NodeConfig::table`), so the publisher's
session announces the broadcast straight into the cluster at its own path,
with no copy. The relay pulls only a ticket whose path is the publisher's
canonical `live/<peer>/<name>` or `rooms/<topic>/<peer>/<name>` path. It refuses
any other ticket with `NotOwner`. The session that named the ticket is
served the cluster with a mount at that name (`origin::Producer::mount`), so it
reads the broadcast under the name it asked for. From then on it is ordinary
relay fan-out: a second viewer of the same ticket shares the first one's
upstream session.

Concurrent pulls for one ticket share one dial, so two browsers that arrive
together open one upstream session. The relay claims each pulled path
through `Node::claim_path`, on a base grant of nothing. The session's grant
then lets the publisher announce only the pulled paths, so the cluster carries only the broadcasts the
tickets name. The relay dials with that grant, which the session enforces from
before its handshake, and a pull that joins a live session widens it. A pull ends once no session has named the ticket for ten
seconds, which lets a page reload reuse it, and its path then leaves the
grant and the cluster. When the last pull of a publisher ends, the relay
closes its session with that publisher.

## Publishing to a relay

A publisher reaches a relay through a relay link.
`Node::attach_relay(RelayConfig::new(url))` stays attached and redials with
backoff, and every publication with the `Everyone` audience is visible to it.
`irl publish --relay <ENDPOINT_ID>` attaches to `iroh://<ENDPOINT_ID>/` this
way. A relay link consumes by default: it feeds every route the relay knows
into the node's route table. `irl publish` only publishes, so it attaches with
`consume: false`. The [browser relay guide](../guide/browser-relay.md) has the
workflow.
