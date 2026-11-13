# Run the iroh-live relay

This page runs `iroh-live-relay` for others to use. For a first try on one
machine, the [CLI quickstart](../getting-started/cli-quickstart.md#open-it-in-a-browser)
is enough. The [relay reference](../reference/relay.md) lists every flag.

## Start it

```sh
cargo build --release -p iroh-live-relay
./target/release/iroh-live-relay
```

It prints its endpoint id, which publishers pass to `irl publish --relay`,
and the address of its web viewer. It logs at `info` unless `RUST_LOG` says
otherwise.

Open these ports:

- UDP 4443 for WebTransport, the browsers' connection.
- TCP 4443 for the web viewer, over plain HTTP.
- Outgoing UDP for the relay's iroh endpoint. It needs no fixed incoming
  port, since iroh punches through NAT and falls back to an iroh relay.

Keep the secret key. It lives in `$IROH_LIVE_RELAY_DATA/iroh_secret_key`, or
under the platform's data directory, and it fixes the relay's endpoint id.
Back it up, and set `IROH_LIVE_RELAY_DATA` to a persistent volume in a
container. A new key means a new endpoint id, which every publisher has to
learn again.

## Know the defaults

The defaults suit a relay on your own machine or a trusted network:

- **Anyone may read every broadcast on the relay.** Every publisher that
  pushes to it, and every broadcast it pulls, is visible to every browser and
  iroh client that connects.
- An iroh client may publish only at paths that name its endpoint id. A
  browser may publish names of one segment.
- The relay pulls no tickets, so a browser cannot open a ticket until you
  pass `--open-pulls` or set up tokens.
- TLS uses a self-signed certificate, made at every start. The web page pins
  its fingerprint, which works only when the page itself is loaded over plain
  `http://`. There is no ACME.

## Give rights with tokens

`--auth-key <PATH>` makes the relay verify JWTs signed with a key in the
file, a JWK or a JWK set as `moq auth` writes them. A token grants what its
claims say: publishing outside the default paths, and pulling a ticket.

`--auth-key` does **not** make the relay private. A session without a token
keeps the default rules, so anonymous browsers and iroh clients still read
every broadcast, and anonymous browsers can still publish names of one
segment.

No flag refuses anonymous iroh clients. For browsers, `--auth-url` hands
every session to an auth server of yours, as moq-relay does, which can refuse
anonymous ones. The relay then pulls no tickets for browsers.

The relay reads the key file again when it changes, so a rotated key takes
effect for the next session without a restart. Sessions already admitted keep
their authorization until their token expires.

`--open-pulls` lets any session make the relay dial any iroh endpoint, within
the pull limits. Use it only on a relay that only you can reach.

## Put it on the internet

The browser connection needs a certificate the browser trusts, or a page
served over plain `http://` that pins the self-signed one. A page on
`https://` cannot pin it, and browsers refuse the self-signed certificate.
The relay cannot load a certificate of your own yet. Until it can:

- Serve the viewer to people on a network you control over `http://`, as the
  relay does by default.
- Or let publishers attach a relay link over `https://` to a moq-relay that
  has a real certificate, and send browsers there. That relay cannot pull
  tickets over iroh.

## Checklist

Before you expose a relay:

- Every broadcast that reaches the relay is readable by anyone who can reach
  it. Publish only what may be public, or keep the relay on a private network.
- Do not use `--open-pulls`.
- Keep the JWK file and the iroh secret key private, and back up the key.
- Issue short-lived tokens. Removing a key stops new sessions, but a session
  admitted with a token keeps its rights until the token expires.
- Calls and rooms publish to a set of peers, and never reach a relay. Only
  broadcasts with the `Granted` audience do.

## Where to go next

- [Serve browsers](browsers.md)
- [Publish through a relay](publish-through-relay.md)
- [Peer-to-peer and the relay](../architecture/p2p-relay.md) explains pulls.
