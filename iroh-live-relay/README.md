# iroh-live-relay

Relay server that bridges iroh peer-to-peer streams to browsers via WebTransport.

Browsers cannot speak iroh's QUIC protocol directly. The relay bridges the gap by accepting WebTransport connections from browsers and pulling streams from iroh publishers on demand. When a browser subscribes to a ticket, the relay connects to the publisher, fetches the broadcast, and re-serves it over WebTransport using the MoQ protocol.

## Running

```sh
# Development mode (self-signed certs, prints TLS fingerprint)
cargo run -p iroh-live-relay -- --dev

# Custom bind address
cargo run -p iroh-live-relay -- --dev --bind [::]:8443 --http-bind [::]:8443
```

The relay serves a built-in web viewer at its HTTP root. Open `https://localhost:4443` in a browser to see the landing page, then paste a ticket to watch a stream.

Direct link: `https://localhost:4443?name=<TICKET>`

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
npm install
npm run dev    # Vite dev server with hot reload
npm run build  # bundle for embedding
```

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--dev` | off | Use self-signed certificates, print TLS fingerprint |
| `--bind` | `[::]:4443` | QUIC/WebTransport bind address |
| `--http-bind` | `[::]:4443` | HTTP server bind address |

The relay persists its iroh secret key to disk (in `$IROH_LIVE_RELAY_DATA` or the platform data directory) so the endpoint ID stays stable across restarts.

## HTTP endpoints

- `GET /certificate.sha256` -- TLS certificate fingerprint (dev mode)
- `GET /` -- web viewer landing page
- `GET /{path}` -- static file serving (CORS enabled)
