# iroh-live-relay

A relay that serves iroh-live broadcasts to browsers over WebTransport.

Browsers cannot speak iroh. The relay accepts WebTransport sessions from
browsers and iroh sessions from iroh-live nodes, and serves both from one
moq-relay cluster. When a browser asks for a broadcast ticket, the relay pulls
that broadcast from its publisher over iroh.

## Running

```sh
cargo run -p iroh-live-relay

# Another port, for both QUIC and HTTP
cargo run -p iroh-live-relay -- --bind [::]:8443
```

Then open `http://localhost:4443` and paste a ticket or broadcast name, or
link to `http://localhost:4443/?name=<TICKET>`.

Use `http`, not `https`. TCP port 4443 serves the web viewer and
`/certificate.sha256` over plain HTTP, and opening `https://localhost:4443`
fails inside TLS (Firefox reports "SSL received a record that exceeded the
maximum permissible length"). UDP port 4443 carries WebTransport over HTTP/3,
which the page opens for you. The self-signed certificate needs no exception:
the page fetches its fingerprint from `/certificate.sha256` and pins it, which
`@moq/net` only does for an `http:` page.

### Browser support

The viewer needs WebTransport. Chromium works, Safari does not, and Firefox
needs version 153 or newer: earlier versions allow only two concurrent
remote-initiated streams
([bug 2046262](https://bugzilla.mozilla.org/show_bug.cgi?id=2046262)), and
`@moq/net` refuses them by user agent. On an older Firefox the client falls
back to WebSocket, which this relay does not serve, so the connection fails
with a 404 on `ws://localhost:4443/<name>`.

## Pull mode

1. A browser connects and asks for a broadcast by ticket.
2. If the cluster does not have it, the relay dials the publisher over its own
   iroh endpoint, subscribes, and splices the broadcast into the cluster under
   the name the browser asked for.
3. The browser reads it through the relay.

Browsers watching the same ticket share one upstream session, which the relay
closes ten seconds after the last viewer leaves.

## Web client

`web/` holds a SolidJS and TypeScript client built with Vite and embedded into
the binary with `include_dir`. The watch page plays a ticket or broadcast name
with `@moq/watch`, and the publish page publishes the browser's camera and
microphone.

```sh
cd iroh-live-relay/web
npm ci
npm run dev    # dev server with hot reload
npm run build  # bundle for embedding
```

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--bind` | `[::]:4443` | WebTransport bind address |
| `--http-bind` | the address `--bind` bound | HTTP bind address. The page connects to its own origin, so a different port needs `?url=` |

TLS certificates are self-signed and generated at startup. There is no ACME
and no token auth: anyone may connect and subscribe to anything. Publishing is
scoped by identity. The relay accepts iroh sessions itself, through
`iroh_moq::transport::accept`, so it knows each client's endpoint id and lets
it publish only at paths that name it, `live/<its id>/...` and
`rooms/<topic>/<its id>/...`. Browsers have no such identity and may publish
only at names of one segment.

The relay stores its iroh secret key in `$IROH_LIVE_RELAY_DATA`, or else in the
platform data directory, so its endpoint id survives restarts.

## HTTP endpoints

- `GET /certificate.sha256`: the TLS certificate fingerprint, for pinning
- `GET /`: the web viewer
- `GET /{path}`: static files, with CORS
