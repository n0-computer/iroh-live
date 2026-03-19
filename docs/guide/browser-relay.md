# Browser Relay

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | iroh-live-relay |
| Platforms | Linux, macOS (server side); any modern browser (client side) |

## Why browsers need a relay

Browsers cannot speak iroh's QUIC protocol directly. iroh uses custom ALPNs and endpoint addressing that the browser WebTransport API does not support. The relay server bridges this gap: it accepts WebTransport connections from browsers and connects to iroh publishers on demand, forwarding media streams between the two worlds.

## How the relay works

The `iroh-live-relay` binary runs both an iroh endpoint and a WebTransport/HTTP server. When a browser requests a broadcast by ticket, the relay follows a pull-on-demand model:

1. The browser connects via WebTransport and requests a broadcast by ticket string.
2. The relay checks whether it already has that broadcast locally (from a previous pull or a direct publisher).
3. If not, the relay uses its own iroh endpoint to connect to the remote publisher, subscribes to the broadcast, and injects it into the local relay.
4. The browser receives the stream through the relay's WebTransport frontend.

Multiple browser clients watching the same ticket share a single upstream connection to the publisher. The relay only pulls what someone is watching.

## Running the relay

For local development with self-signed certificates:

```sh
cargo run -p iroh-live-relay -- --dev
```

This starts the relay on port 4443 (both QUIC and HTTP), generates a self-signed TLS certificate, and prints the certificate fingerprint for browser trust.

To use a custom bind address:

```sh
cargo run -p iroh-live-relay -- --dev --bind [::]:8443 --http-bind [::]:8443
```

The relay persists its iroh secret key to disk (in `$IROH_LIVE_RELAY_DATA` or the platform data directory) so the endpoint ID stays stable across restarts.

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--dev` | off | Use self-signed certificates, print TLS fingerprint |
| `--bind` | `[::]:4443` | QUIC/WebTransport bind address |
| `--http-bind` | `[::]:4443` | HTTP server bind address |

## The built-in web viewer

The relay serves a web viewer at its HTTP root. Open `https://localhost:4443` in a browser to see the landing page, then paste a ticket to watch a stream. You can also link directly:

```
https://localhost:4443?name=<TICKET>
```

Any iroh-live ticket can be watched in the browser this way. The web viewer uses the `@moq/watch` web component with WebCodecs for decoding and rendering.

The relay also serves a publish page where you can capture from the browser camera and microphone and publish into the relay. Other iroh-live clients (desktop or mobile) can then subscribe to that broadcast.

## HTTP endpoints

- `GET /` serves the web viewer landing page.
- `GET /certificate.sha256` returns the TLS certificate fingerprint (dev mode only).
- `GET /{path}` serves static files with CORS headers.

## Web client development

The web assets live in `iroh-live-relay/web/` and are embedded into the relay binary at compile time via `include_dir`. The client is built with SolidJS, TypeScript, and Vite.

To develop the web client with hot reload:

```sh
cd iroh-live-relay/web
npm install
npm run dev    # Vite dev server with hot reload
npm run build  # bundle for embedding into the relay binary
```

The web client depends on `@moq/watch` and `@moq/publish` from the [moq](https://github.com/kixelated/moq) project.

## End-to-end workflow

A typical workflow with the relay:

1. Start the relay: `cargo run -p iroh-live-relay -- --dev`
2. Publish from a desktop: `cargo run --release --example publish`
3. Open the relay URL in a browser with `?name=<TICKET>` to watch the stream.

The publisher does not need to know about the relay. The relay connects to the publisher on behalf of the browser viewer, using the iroh endpoint address embedded in the ticket.
