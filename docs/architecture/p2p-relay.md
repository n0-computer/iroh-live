# Peer-to-peer and the relay

## Direct connectivity

iroh connects peers directly when it can. Two machines on the same network reach
each other over the local link, and when both have public addresses or cooperative
NATs, hole punching opens a direct UDP path. When it fails, traffic falls back to
an iroh relay, which forwards opaque packets and costs its own round trip.

From the media pipeline's point of view the transition is invisible. What changes
is round-trip time and available bandwidth, which is exactly what [adaptive
rendition switching](adaptive.md) reads. `iroh-live`'s stats recorder labels the
selected path `direct` or `relayed` so the overlay can show which one is carrying
the stream.

An iroh relay and a MoQ relay are unrelated. The first forwards UDP between peers
that cannot reach each other and understands nothing about the media. The second
is the one below.

## iroh-live-relay

Browsers cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection, not a QUIC endpoint, so there is no hole punching and no way to accept
an inbound connection. `iroh-live-relay` bridges the two worlds: it runs
`moq_relay::Cluster` behind both a WebTransport listener and an iroh endpoint, so
a broadcast that arrives over either transport is reachable from the other.

It also serves the web client, built with solid-js against `@moq/watch` and
`@moq/publish` and embedded into the binary with `include_dir`.

Two flags configure it. `--bind` is the QUIC address, default `[::]:4443`, and
`--http-bind` is the HTTP address, defaulting to the same. TLS certificates are
self-signed and generated at startup; `GET /certificate.sha256` returns the
fingerprint so a browser can pin it. ACME provisioning is not implemented.

The iroh secret key is persisted under `IROH_LIVE_RELAY_DATA`, or the platform
data directory, so the relay's endpoint id survives a restart.

**There is no authentication.** The relay grants publish and subscribe on every
path to every connection. Do not put one on a public address.

## Pull on demand

The relay does not need to be told about a publisher in advance. When a client
subscribes to a broadcast whose name parses as a `LiveTicket`, the relay dials the
endpoint in the ticket over iroh, subscribes to the broadcast, and mirrors it into
the cluster under the same name. Everything after that is ordinary relay fan-out:
a second viewer of the same ticket shares the first one's upstream connection.

Two details keep that honest. Concurrent pulls for one ticket coalesce through a
map of in-flight connects, so two browsers arriving together open one upstream
session rather than two. And the mirror is scoped to exactly the one broadcast the
ticket names, so nothing else the remote node publishes leaks into the cluster.

A name that is not a ticket is looked up locally and fails immediately if it is
not there, rather than hanging: the cluster origin registers no dynamic handler.

## Publishing to a relay

From the publisher's side, reaching a relay is just a connection. Everything the
node publishes is announced on every MoQ session it has, so `irl publish --relay
<ENDPOINT_ID>` connects and the announce follows. See the [browser relay
guide](../guide/browser-relay.md) for the full workflow.
