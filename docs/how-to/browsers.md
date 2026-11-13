# Browser relay

A browser cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection but no endpoint, so there is no hole punching and no way to accept
a connection. `iroh-live-relay` speaks WebTransport to browsers and iroh to
native peers, and moves broadcasts between them.

By default, anyone may connect to the relay and read every broadcast on it.
Publishing is limited: an iroh client may publish only at paths that name its
endpoint id, and a browser only at names of one segment. `--auth-key` makes
the relay verify JWTs, and the `--auth-url` and `--auth-public` flags hand
browser sessions to moq-relay's own auth.
[Access control](control-access.md#the-relays-auth-flags) explains the flags. The
[relay README](../../iroh-live-relay/README.md) covers the rest of running it:
the certificate, where it keeps its key, and which browsers work.

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
is not a ticket must already be on the relay, or the request fails. With
`--auth-url` or an `--auth-public` flag, the relay pulls no tickets for
browsers.

## Publishing through the relay

A publisher that subscribers cannot reach directly pushes to the relay:

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

The publisher stays attached and redials if the session drops. The broadcast
appears on the relay at `live/<publisher endpoint id>/<name>`, and
`irl publish` prints that path. Without a token, an iroh client may publish
only at paths that contain its own endpoint id, so nobody can publish under
another node's name. A relay started with `--auth-key` accepts a JWT from the
client, which a Rust node sets with `RelayConfig::with_token`.

The relay also serves a publish page, which publishes the browser's camera and
microphone. A Rust node reads such a broadcast by attaching to the relay and
subscribing with `Reach::Relays`, as `iroh-live/examples/subscribe_test.rs`
does. `irl watch` cannot, since its tickets name a publisher, not a relay.

`irl publish --relay` attaches with `consume: false` in its `RelayConfig`, so
it does not take the relay's routes into its own route table. The default
config does.
