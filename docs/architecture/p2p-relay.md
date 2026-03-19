# P2P and Relay

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | iroh-live, iroh-moq, iroh-live-relay |

## Direct connectivity

iroh connects peers directly when possible. Two devices on the same LAN communicate over the local network without any traffic leaving the subnet. When both peers have public IP addresses or simple NATs, iroh's hole-punching establishes a direct UDP path.

The iroh endpoint exposes path statistics via `conn.paths()`, which returns a `Watcher<PathInfoList>`. Each `PathInfo` reports RTT, whether the path is selected, and the remote address. The selected path is the one actively carrying traffic; iroh may maintain multiple candidate paths and switch between them as conditions change.

## Relay fallback

When NAT traversal fails (symmetric NAT, corporate firewalls, CGNAT), iroh relays traffic through the iroh relay network. This adds latency (the relay's RTT to each peer) but ensures connectivity. The relay path appears as a `PathInfo` with `remote_addr().is_relay()` returning true.

From the application's perspective, the transition between direct and relayed paths is transparent. The media pipeline sees only changes in RTT and bandwidth, which the adaptive rendition switching handles automatically.

## iroh-live-relay

The `iroh-live-relay` binary bridges iroh peers to browser clients. It runs moq-relay as a library with both iroh and WebTransport (noq) backends enabled, so broadcasts published through either transport are visible to subscribers on the other.

```
iroh-live publish --(iroh P2P)--> iroh-live-relay <--(WebTransport)-- browser
browser --(WebTransport)--> iroh-live-relay --(iroh P2P)--> iroh-live subscribe
```

The relay accepts iroh connections (moq-lite ALPN) and WebTransport/H3 connections (noq). Both feed into moq-relay's shared `Origin`, which manages broadcast routing. A broadcast published via iroh is automatically available to WebTransport subscribers, and vice versa.

## Pull model

The relay operates in pull mode: it connects to iroh publishers on demand when a browser client requests a broadcast. Multiple browser clients watching the same broadcast share a single upstream iroh connection. This keeps relay resource usage proportional to the number of distinct broadcasts, not the number of viewers.

## Web viewer

The relay embeds a web application built with SolidJS and the `@moq/watch` and `@moq/publish` web components. The watch component uses WebTransport to connect to the relay and WebCodecs to decode video. The web app is compiled by Vite and embedded in the relay binary via `include_dir`.

The watch page auto-detects the relay URL from the page origin. Broadcast names are passed as URL parameters (`?name=hello`), making links shareable.

## TLS and certificates

In development mode (`--dev`), the relay generates self-signed certificates. Browsers need `--ignore-certificate-errors` or the relay's certificate fingerprint (served at `/certificate.sha256`) for WebTransport to work with self-signed certs.

In production, ACME certificate provisioning (via `instant-acme`) handles automatic TLS. The same certificate serves both HTTPS and QUIC. HTTP-01 challenges are served on the relay's HTTP listener, and certificates are stored in the relay's persistent data directory. A background task renews certificates 30 days before expiry.

## Data directory

`RelayServer` maintains a persistent data directory (`IROH_LIVE_RELAY_DATA` env var, or the platform default). The iroh secret key is stored here so the endpoint ID remains stable across restarts. TLS certificates and the ACME account key are also persisted.
