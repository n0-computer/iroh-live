# iroh-live-relay

The iroh-live relay: a server that serves iroh-live broadcasts to browsers
over WebTransport, and to iroh nodes.

Browsers cannot speak iroh. The relay accepts WebTransport sessions from
browsers and iroh sessions from iroh-live nodes, and serves both from one
moq-relay cluster. A publisher pushes its broadcasts with a relay link, or the
relay pulls a broadcast from its publisher when a session names its ticket.

## Running

```sh
cargo run -p iroh-live-relay -- --open-pulls
```

Then open `http://localhost:4443/?name=<TICKET>`, over `http`, not `https`.
`--open-pulls` lets any session make the relay pull a ticket, which suits a
relay on your own machine only.

By default, anyone may connect and read every broadcast on the relay.
`--auth-key` changes what sessions with a valid JWT may do, and takes
nothing from sessions without one. Read
[Run the iroh-live relay](../docs/how-to/run-relay.md) before you expose a
relay.

## Documentation

- [Serve browsers](../docs/how-to/browsers.md): watching and publishing from a
  browser, browser support, and the web client in `web/`.
- [Relay reference](../docs/reference/relay.md): flags, access rules, pulls,
  ports, HTTP endpoints and the key file.
- [Peer-to-peer and the relay](../docs/architecture/p2p-relay.md): how pulls
  start, share and end their upstream sessions.

A binary that embeds the relay calls `iroh_live_relay::run` with a `Config`,
whose `iroh_policy` and `pull_limits` have no command line flags.
