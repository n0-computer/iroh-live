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

It also serves the web client, built with solid-js on `@moq/watch` and
`@moq/publish` and embedded in the binary with `include_dir`.

`--bind` sets the QUIC address, `[::]:4443` by default, and `--http-bind` the
HTTP address, which defaults to the same. The TLS certificate is self-signed
and generated at startup. `GET /certificate.sha256` returns its fingerprint so a
browser can pin it. There is no ACME support.

The relay keeps its iroh secret key in `iroh_secret_key` under
`IROH_LIVE_RELAY_DATA`, or under `iroh-live-relay` in the platform data
directory. It loads the key with `iroh_live::secret_key_file`, so the relay's
endpoint id survives a restart.

**There is no authentication.** Anyone may connect and subscribe to every path.
Publishing is scoped by identity only. An iroh client, whose endpoint id iroh
authenticated, may publish only at `live/<its id>/...` and
`rooms/<topic>/<its id>/...`. A browser may publish only at names of one
segment. Do not run the relay on a public address.

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
`consume: false`. See the [browser relay guide](../guide/browser-relay.md) for
the full workflow.
