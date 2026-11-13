# Peer-to-peer and the relay

## Direct connectivity

iroh connects peers directly when it can. Two machines on one network reach
each other over the local link, and hole punching opens a direct UDP path
through most NATs. When that fails, traffic goes through an iroh relay, which
forwards opaque packets and adds its own round trip.

The media pipeline does not see the change. It sees a different round trip and
bandwidth, and the publisher's delivery estimate follows the bandwidth, which
is what [adaptive rendition switching](adaptive.md) reads. The link monitor in
`iroh-live-node/src/link/sampler.rs` counts every change of selected path as a new
path generation, and the selector forgets what it learned on the old path. The
monitor's `link sample` TRACE log says whether the selected path is relayed.

An iroh relay and the iroh-live relay are unrelated.
[Relays](../concepts/relays.md) tells the three meanings of "relay" apart.

## iroh-live-relay

Browsers cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection, not an endpoint, so there is no hole punching and no way to accept
a connection. `iroh-live-relay` runs a `moq_relay::Cluster` behind a
WebTransport listener and an iroh endpoint, so a broadcast that arrives over
one is reachable from the other. Browsers come in through moq-tokio's server,
and iroh clients through `IrohSessions` (`src/iroh_sessions.rs`).

`IrohSessions` accepts iroh sessions itself, so iroh has authenticated each
client's endpoint id. It decides each client with a policy, as a node does,
after the client's MoQ SETUP. The default, `relay_policy()`, lets a client
read the whole cluster and publish only at paths that name it. With
`--auth-key`, a client that presents a valid JWT gets what the token grants
instead. The relay decides a client once, and closes its session when the
authorization expires, so the client redials with a fresh token. Browsers go
through moq-relay's admissions, which the relay answers with
`browser_grant` or the browser's JWT. The [relay reference](../reference/relay.md)
lists the flags, and [Run the iroh-live relay](../how-to/run-relay.md) covers
deployment.

## Pull on demand

The relay does not need to know a publisher in advance. When a session asks for
a name that parses as a `BroadcastTicket`, and its token allows the pull or the
relay runs with `--open-pulls`, the relay subscribes to the ticket's broadcast
through a node of its own (`src/pull.rs`). `pull::Limits` caps the pulls in
total and per client, and rate-limits each client's new dials. That node uses the
cluster origin as its route table (`NodeConfig::table`), so the publisher's
session announces the broadcast straight into the cluster at its own path,
with no copy. The relay pulls only a ticket whose path is the publisher's
canonical `live/<peer>/<name>` or `rooms/<topic>/<peer>/<name>` path, and
refuses any other ticket with `NotOwner`. The session that named the ticket is
served the cluster with a mount at that name (`origin::Producer::mount`), so
it reads the broadcast under the name it asked for. From then on it is ordinary
relay fan-out: a second viewer of the same ticket shares the first one's
upstream session.

Concurrent pulls for one ticket share one dial, so two browsers that arrive
together open one upstream session. The relay claims each pulled path through
`Node::claim_path`, on a base grant of nothing. The session's grant then lets
the publisher announce only the pulled paths, so the cluster carries only the
broadcasts the tickets name. The relay dials with that grant, which the
session enforces from before its handshake, and a pull that joins a live
session widens it. A pull ends once no session has named the ticket for ten
seconds, which lets a page reload reuse it. Its path then leaves the grant and
the cluster. When the last pull of a publisher ends, the relay
closes its session with that publisher.

## Publishing to a relay

A publisher reaches a relay through a relay link, which
[the transport page](transport.md#relay-links) describes.
[Publish through a relay](../how-to/publish-through-relay.md) has the
workflow.
