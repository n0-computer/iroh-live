# Browser relay

A browser cannot dial an iroh endpoint. WebTransport gives a page a QUIC
connection, not a QUIC endpoint, so there is no hole punching and no way to
accept an inbound connection. `iroh-live-relay` sits between the two: it speaks
WebTransport to browsers and iroh to native peers, and moves broadcasts either
way.

**It has no authentication.** Every connection is granted publish and subscribe
on every path. Run it on a machine you control and do not expose it.

## Running it

```sh
cargo run -p iroh-live-relay
```

It binds `[::]:4443` for QUIC and serves HTTP on the same port, generates a
self-signed certificate at startup, and prints its iroh endpoint id and HTTP
port. `--bind` and `--http-bind` change the addresses.

The iroh secret key is persisted under `IROH_LIVE_RELAY_DATA`, or the platform
data directory, so the endpoint id survives a restart and a ticket keeps working.

Because the certificate is self-signed, a browser needs to be told to trust it.
`GET /certificate.sha256` returns the fingerprint for pinning. ACME provisioning
is not implemented.

## Watching a P2P stream in a browser

Nothing has to be arranged in advance. Publish as usual:

```sh
irl publish
```

Then open the relay in a browser and paste the ticket, or link straight to it:

```
https://localhost:4443/?name=<TICKET>
```

The relay parses the name as a `LiveTicket`, dials the endpoint inside it over
iroh, subscribes, and mirrors that one broadcast into its cluster. A second
viewer of the same ticket shares the first one's upstream connection, and
concurrent arrivals coalesce onto one connect rather than racing.

A name that is not a ticket is looked up locally and fails immediately if the
relay does not already have it.

## Publishing through the relay

A publisher that subscribers cannot dial directly connects to the relay instead:

```sh
irl publish --relay <RELAY_ENDPOINT_ID>
```

Publishing is node-wide, so the announce follows the connection with nothing
further to configure. The relay's endpoint id is on its startup line.

The relay also serves a publish page, which captures the browser's camera and
microphone and publishes into the relay. Native clients subscribe to that
broadcast like any other.

## The web client

`iroh-live-relay/web/` is a solid-js and TypeScript application built with Vite,
using the `@moq/watch` and `@moq/publish` web components. `include_dir` embeds
`web/dist` into the binary at compile time, so the relay serves it with no files
on disk.

```sh
cd iroh-live-relay/web
npm ci
npm run dev      # Vite dev server with hot reload
npm run build    # bundle for embedding
```

Rebuild the bundle before rebuilding the relay if you change the client.

## HTTP endpoints

`GET /` serves the landing page, `GET /certificate.sha256` the TLS fingerprint,
and `GET /{path}` any other embedded file. All of them carry permissive CORS
headers.

## Tests

`tests/e2e-browser/` is a Playwright suite that builds the relay and the CLI,
starts both, and watches a stream in Chromium. `cargo make test-e2e` runs it,
building the prerequisites first.
