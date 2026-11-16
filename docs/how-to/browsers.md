# Serve browsers

A browser cannot dial an iroh endpoint: WebTransport gives a page a QUIC
connection but no endpoint, so there is no hole punching and no way to accept
a connection. The iroh-live relay speaks WebTransport to browsers and iroh to
nodes, and moves broadcasts between them. [Relays](../concepts/relays.md)
explains the model.

## Watch a broadcast in a browser

The relay can *pull* a broadcast by its ticket: it dials the publisher over
iroh, and the publisher needs to do nothing. For a relay on your own machine,
allow pulls with `--open-pulls`:

```sh
cargo run -p iroh-live-relay -- --open-pulls
irl publish
```

Then open the relay in a browser and paste the ticket, or link to it:

```text
http://localhost:4443/?name=<TICKET>
```

Use `http`, not `https`. TCP port 4443 serves the page over plain HTTP.
The page then pins the relay's self-signed certificate for WebTransport on
UDP port 4443, which the browser allows only from an `http:` page. Opening
`https://localhost:4443` fails inside TLS, and Firefox then reports "SSL
received a record that exceeded the maximum permissible length".

A relay reachable by others should not use `--open-pulls`, since it lets
anyone make the relay dial any endpoint. Start it with `--auth-key` instead,
and give viewers a JWT that grants reading the ticket's path, in the URL as
`?jwt=<token>`. See [Run the iroh-live relay](run-relay.md).

## Push instead of pull

A publisher can push to the relay instead, which needs no pull permission:

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

`irl publish` prints the path at which the broadcast appears on the relay,
`live/<publisher id>/<name>`. Open that path as the name:
`http://localhost:4443/?name=live/<publisher id>/hello`.
[Publish through a relay](publish-through-relay.md) covers the Rust side.

## Publish from a browser

The relay also serves a publish page, which publishes the browser's camera
and microphone under a name of one segment. A Rust node reads it by attaching
to the relay and subscribing with `Reach::Relays`, as
[`iroh-live/examples/subscribe_test.rs`](../../iroh-live/examples/subscribe_test.rs)
does. `irl watch` cannot, since its tickets name a publisher, not a relay.

## Browser support

The viewer needs WebTransport. Chromium works, and Firefox needs version 153
or newer. Safari does not work. Earlier Firefox versions allow only two
concurrent streams that the server opens
([bug 2046262](https://bugzilla.mozilla.org/show_bug.cgi?id=2046262)), so
`@moq/net` refuses them and falls back to WebSocket, which this relay does
not serve. The connection then fails with a 404 on
`ws://localhost:4443/<name>`.

## The web client

The viewer is a SolidJS and TypeScript client in `iroh-live-relay/web`,
built with Vite and embedded into the binary. To change it:

```sh
cd iroh-live-relay/web
npm ci
npm run dev    # dev server with hot reload
npm run build  # bundle for embedding, then rebuild the relay
```

## Where to go next

- [Run the iroh-live relay](run-relay.md) before you expose a relay.
- [Relay reference](../reference/relay.md) lists the flags.
