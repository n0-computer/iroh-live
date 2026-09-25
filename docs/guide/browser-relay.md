# Browser relay

A browser cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection but no endpoint, so there is no hole punching and no way to accept
a connection. `iroh-live-relay` speaks WebTransport to browsers and iroh to
native peers, and moves broadcasts between them.

**The relay has no authentication.** Every connection may subscribe to every
path. Publishing is limited only by identity, as described in [publishing
through the relay](#publishing-through-the-relay). Run it on a machine you
control, and do not expose it to the internet.

## Running it

```sh
cargo run -p iroh-live-relay
```

It binds `[::]:4443` for QUIC and serves HTTP on the same port. It generates a
self-signed certificate at startup and prints its iroh endpoint id. `--bind`
and `--http-bind` change the addresses.

The iroh secret key is stored in `IROH_LIVE_RELAY_DATA`, or else in the
platform data directory, so the endpoint id stays the same across restarts.

The browser needs no certificate exception. `GET /certificate.sha256` returns
the certificate's fingerprint, and the page pins it when it opens the
WebTransport session. `@moq/net` pins only for an `http:` page, so the viewer
is served over plain HTTP. There is no ACME support.

## Watching a P2P stream in a browser

Publish as usual:

```sh
irl publish
```

Then open the relay in a browser and paste the ticket, or link to it:

```
http://localhost:4443/?name=<TICKET>
```

Use `http`, not `https`. TCP port 4443 serves the viewer over plain HTTP, and
UDP port 4443 carries the WebTransport session, which the page opens itself.
An `https` URL fails inside TLS. Firefox reports it as "SSL received a record
that exceeded the maximum permissible length".

The viewer needs WebTransport. Chromium works, Safari does not, and Firefox
needs version 153 or newer. `iroh-live-relay/README.md` explains why.

When the name is a `BroadcastTicket`, the relay subscribes to the ticket's
broadcast over iroh and serves it to the browser under that name. Viewers of
the same ticket share one upstream session. A name that is not a ticket must
already be on the relay, or the request fails.

## Publishing through the relay

A publisher that subscribers cannot reach directly pushes to the relay:

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

The relay prints its endpoint id at startup. The publisher stays attached and
redials if the session drops. The broadcast appears on the relay at
`live/<publisher endpoint id>/<name>`, and `irl publish` prints that path.

The relay also serves a publish page, which publishes the browser's camera and
microphone. A Rust node reads such a broadcast by attaching to the relay and
subscribing with `Reach::Relays`, as `iroh-live/examples/subscribe_test.rs`
does. `irl watch` cannot, since its tickets name a publisher, not a relay.

Anyone may watch anything, but nobody may publish under another node's name.
An iroh client may publish only at paths that contain its own endpoint id:
`live/<its id>/...` and `rooms/<topic>/<its id>/...`. The relay accepts iroh
sessions itself, so it knows that id. A browser has no such identity, so it may
publish only at one-segment names, which is what the publish page's `?name=`
gives. A browser cannot publish into `live/` or `rooms/`.

`irl publish --relay` attaches with `consume: false` in its `RelayConfig`, so
it does not copy the relay's routes into its own route table. The default
config does.

## The web client

`iroh-live-relay/web/` is a SolidJS and TypeScript app built with Vite. It uses
the `@moq/watch` and `@moq/publish` web components. `include_dir` embeds
`web/dist` into the binary at compile time.

```sh
cd iroh-live-relay/web
npm ci
npm run dev      # Vite dev server with hot reload
npm run build    # bundle for embedding
```

After changing the client, rebuild the bundle and then the relay.

## HTTP endpoints

`GET /` serves the viewer, `GET /certificate.sha256` the TLS fingerprint, and
`GET /{path}` any other embedded file. All of them send permissive CORS
headers.

## Tests

`tests/e2e-browser/` is a Playwright suite. It builds the relay and the CLI,
starts both, and watches a stream in Chromium. `cargo make test-e2e` builds
what it needs and runs it.
