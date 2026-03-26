# Relay + browser — remaining work

Steps 1–6 (relay binary, web viewer, WebTransport bridge, Playwright
tests) are complete. See
[docs/guide/browser-relay.md](../docs/guide/browser-relay.md) for
running the relay and the E2E workflow.

## ACME cert provisioning

Automatic TLS certificate via Let's Encrypt for production deployments.
Currently requires manually providing `--cert`/`--key`.

- [ ] Add `--acme-domain`, `--acme-email`, and `--acme-env` (prod/staging)
  CLI flags (mutually exclusive with `--dev`)
- [ ] Use `tokio-rustls-acme` crate for automatic cert provisioning
  (`instant-acme` is another option but requires more manual plumbing)
- [ ] Serve ACME challenges on HTTP (:8080), redirect other HTTP to HTTPS
- [ ] Background renewal task — check every 12h, renew if < 30 days
- [ ] Persist account key + cert in data_dir
- [ ] Both HTTPS and QUIC/WebTransport use the same ACME cert

### Network topology
```
HTTP :8080  →  ACME challenges + redirect to HTTPS
HTTPS :443  →  Web viewer + relay API
QUIC  :443  →  WebTransport (browsers) + iroh (native)
```

## Shared ticket trait

Common `Ticket` trait for `LiveTicket`, `CallTicket`, `RoomTicket`.

- [ ] Define `Ticket` trait with `endpoint_addr()`, `name()`,
  `relay_urls()` accessors, plus `Display`/`FromStr`
- [ ] Verify both `LiveTicket` and `CallTicket` support relay URL list
- [ ] Browser URL format with relay URL + CLI ticket format
- [ ] QR code generation for terminal/e-paper display
