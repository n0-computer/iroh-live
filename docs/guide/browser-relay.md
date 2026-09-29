# Browser relay

A browser cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection but no endpoint, so there is no hole punching and no way to accept
a connection. `iroh-live-relay` speaks WebTransport to browsers and iroh to
native peers, and moves broadcasts between them.

**The relay has no authentication.** Run it on a machine you control, and do
not expose it to the internet. The [relay README](../../iroh-live-relay/README.md)
covers running it: its flags, the certificate, where it keeps its key, what
each client may publish, and which browsers work.

## Watching a P2P stream in a browser

Start the relay, which prints its endpoint id, and publish as usual:

```sh
cargo run -p iroh-live-relay
irl publish
```

Then open the relay in a browser and paste the ticket, or link to it:

```
http://localhost:4443/?name=<TICKET>
```

Use `http`, not `https`. The README says why.

When the name is a `BroadcastTicket`, the relay subscribes to the ticket's
broadcast over iroh and serves it to the browser under that name. A name that
is not a ticket must already be on the relay, or the request fails.

## Publishing through the relay

A publisher that subscribers cannot reach directly pushes to the relay:

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

The publisher stays attached and redials if the session drops. The broadcast
appears on the relay at `live/<publisher endpoint id>/<name>`, and
`irl publish` prints that path. An iroh client may publish only at paths that
contain its own endpoint id, so nobody can publish under another node's name.

The relay also serves a publish page, which publishes the browser's camera and
microphone. A Rust node reads such a broadcast by attaching to the relay and
subscribing with `Reach::Relays`, as `iroh-live/examples/subscribe_test.rs`
does. `irl watch` cannot, since its tickets name a publisher, not a relay.

`irl publish --relay` attaches with `consume: false` in its `RelayConfig`, so
it does not copy the relay's routes into its own route table. The default
config does.
