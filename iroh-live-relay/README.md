# iroh-live-relay

Relay server that bridges iroh peer-to-peer streams to browsers via WebTransport.

Browsers cannot speak iroh's QUIC protocol directly. The relay bridges the gap by accepting WebTransport connections from browsers and pulling streams from iroh publishers on demand. When a browser subscribes to a ticket, the relay connects to the publisher, fetches the broadcast, and re-serves it over WebTransport using the MoQ protocol.

## Running

```sh
cargo run -p iroh-live-relay

# Custom bind addresses
cargo run -p iroh-live-relay -- --bind [::]:8443 --http-bind [::]:8443
```

Then open **`http://localhost:4443`**, and paste a ticket or broadcast name.
Direct link: `http://localhost:4443/?name=<TICKET>`

`http`, not `https`, and the distinction is the whole of what makes this work:

- **TCP 4443 speaks plain HTTP.** It serves the web viewer and
  `/certificate.sha256`. Opening `https://localhost:4443` reaches this listener
  and fails inside TLS, reported by Firefox as "SSL received a record that
  exceeded the maximum permissible length": that is the browser reading
  `HTTP/1.1 400` as a TLS record.
- **UDP 4443 carries WebTransport over HTTP/3**, which is where the media
  flows. It is the same port number and a different protocol, so the two do not
  collide. It is not an address you open from the address bar; the page does it
  for you.
- **The self-signed certificate needs no exception.** The page fetches its
  SHA-256 from `/certificate.sha256` over plain HTTP and pins it when opening
  the WebTransport session, so the browser never prompts. This is why the
  viewer is served over `http` in the first place: the pinning path in
  `@moq/net` only runs for an `http:` page URL.

### Browser support

The viewer needs WebTransport, so **Chromium** works and **Safari** does not.
**Firefox needs 153 or newer**: earlier versions allow only two concurrent
remote-initiated streams ([bug 2046262](https://bugzilla.mozilla.org/show_bug.cgi?id=2046262)),
and `@moq/net` refuses them by user agent rather than letting a session stall.

On an older Firefox the client falls back to a WebSocket transport, and this
relay does not serve one, so the connection fails with a 404 on
`ws://localhost:4443/<name>`. Adding the fallback means serving the WebSocket
upgrade on the same TCP port as the viewer, since the client derives the
WebSocket URL from the page's own origin and `@moq/watch` exposes no override.
That is a real change rather than a flag, and it is not done.

## How pull mode works

1. A browser connects via WebTransport and requests a broadcast by ticket string.
2. The relay checks if it already has that broadcast locally (from a previous pull or a direct publisher).
3. If not, it uses its own iroh endpoint to connect to the remote publisher, subscribes to the broadcast, and injects it into the local relay cluster.
4. The browser receives the stream through the relay's WebTransport frontend.

Multiple browser clients watching the same ticket share a single upstream connection.

## Web client

The [`web/`](web/) directory contains a SolidJS + TypeScript web client built with Vite:

- **Watch page**: paste a ticket or broadcast name to view a stream, using the `@moq/watch` web component with WebCodecs for decoding.
- **Publish page**: capture from the browser camera and microphone and publish into the relay.

The web assets are embedded into the relay binary at compile time via `include_dir`.

To develop the web client separately:

```sh
cd iroh-live-relay/web
npm ci
npm run dev    # Vite dev server with hot reload
npm run build  # bundle for embedding
```

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--bind` | `[::]:4443` | QUIC and WebTransport bind address |
| `--http-bind` | `[::]:4443` | HTTP bind address |

TLS certificates are self-signed and generated at startup. ACME provisioning is
not implemented, and neither is authentication: the relay grants publish and
subscribe on every path to every connection.

The relay persists its iroh secret key to disk (in `$IROH_LIVE_RELAY_DATA` or the platform data directory) so the endpoint ID stays stable across restarts.

## HTTP endpoints

- `GET /certificate.sha256` -- TLS certificate fingerprint, for pinning the self-signed cert
- `GET /` -- web viewer landing page
- `GET /{path}` -- static file serving (CORS enabled)
