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

## Choose an access mode

Without auth flags the relay is *open*. It suits a relay on your own machine
or a trusted network:

- **Anyone who reaches the relay reads every broadcast on it**, over
  WebTransport or by its endpoint id. The relay warns about this at every
  start.
- An iroh client may publish only at its own paths, the paths that name its
  endpoint id. A browser may publish names of one segment.
- The relay pulls no tickets, so a browser cannot open a ticket until you
  pass `--open-pulls`.

Any `--auth-*` or `--rcan-*` flag makes the relay *private*. A session with a
valid token gets what the token grants. A session without one gets the
`--auth-public*` patterns, or is refused. The startup line says which mode
the relay runs in and why.

Make a key pair and sign tokens with the `moq` CLI from moq-cli, and give the
relay only the public key:

```sh
cargo install moq-cli
moq auth generate --algorithm ES256 --out private.jwk --public public.jwk

# A viewer: read everything for 8 hours.
moq auth sign --key private.jwk --subscribe '**' --ttl 8h > viewer.jwt
# A publisher with a known endpoint id: its own paths, read everything.
moq auth sign --key private.jwk \
  --publish "live/$PUBLISHER_ID/**" --publish "rooms/*/$PUBLISHER_ID/**" \
  --subscribe '**' --ttl 12h > publisher.jwt
```

Then pick the flags for the relay you want:

```sh
# Private: only token holders.
iroh-live-relay --auth-key public.jwk

# Private, and any token holder also publishes at its own paths, so one
# shared token with --subscribe '**' serves every member.
iroh-live-relay --auth-key public.jwk --auth-own-paths

# A private relay with a public area: anyone watches demo/, and only token
# holders read the rest and publish.
iroh-live-relay --auth-key public.jwk --auth-public-subscribe 'demo/**'

# Anyone may push at their own iroh paths, and only members watch.
iroh-live-relay --auth-key public.jwk --auth-public-own-paths

# iroh members only, with rcan chains the team key issues.
iroh-live-relay --rcan-principal "$(irl key team)" --auth-own-paths

# The old --auth-key behaviour, written out: anyone reads, tokens add rights.
iroh-live-relay --auth-key public.jwk --auth-public-subscribe '**' \
  --auth-public-publish '*' --auth-public-own-paths
```

[Use tokens](tokens.md#on-the-iroh-live-relay) shows how clients present a
token, and how to issue rcan chains with `irl rcan`.

A token grants exactly what it states. A subscribe-only token publishes
nothing unless the relay runs `--auth-own-paths`, which applies to JWTs and
to principals at the relay root, not to a principal limited to a root below
it.

### Keys, expiry and revocation

- Tokens must expire. A private relay refuses a token without an expiry
  unless you pass `--allow-unexpiring-tokens`.
- A session admitted with a token lasts at most `--max-session`, 24 hours by
  default. The relay then asks the client to reconnect, and an iroh client
  dials a new session with its token. `irl publish` keeps running.
- The relay reads the key file again when it changes, and checks it once a
  second. Removing a key refuses it for new sessions at once, and closes the
  live iroh sessions it admitted within about a second. A browser session
  keeps its grant until its token or `--max-session` expires.
- An rcan chain cannot be revoked before it expires. Keep device links
  short, hours rather than days. Removing a `--rcan-principal` takes a
  restart, which ends every session anyway.
- Keep principal keys apart from node keys, on the operator's machine. `irl
  key` stores them readable by this user only.

`--open-pulls` lets every session the relay admits to read a ticket's path
make it dial the ticket's publisher. On an open relay that is anyone. On a
private relay it is token holders, plus sessions without a token whose public
patterns reach `live/` or `rooms/`, which the relay warns about.

## Put it on the internet

The browser connection needs a certificate the browser trusts, or a page
served over plain `http://` that pins the self-signed one. Plain HTTP also
carries the page and its certificate pin, so on a private relay a browser's
token is only as safe as the network. The relay warns about this when the
viewer listens beyond loopback. Put browsers on a trusted network, or behind
a proxy that terminates TLS. A page on
`https://` cannot pin it, and browsers refuse the self-signed certificate.
The relay cannot load a certificate of your own yet. Until it can:

- Serve the viewer to people on a network you control over `http://`, as the
  relay does by default.
- Or let publishers attach a relay link over `https://` to a moq-relay that
  has a real certificate, and send browsers there. That relay cannot pull
  tickets over iroh.

## Checklist

Before you expose a relay:

- Make it private: pass `--auth-key`, `--rcan-principal` or both. An open
  relay serves every broadcast to anyone who reaches it.
- Check the startup line. It lists what sessions with and without a token
  get.
- Do not use `--open-pulls` unless the public patterns stay out of `live/`
  and `rooms/`.
- Keep the private JWK, principal keys and the iroh secret key private, and
  back up the iroh key.
- Issue short-lived tokens, and give viewers links with `#jwt=`, not
  `?jwt=`.
- Calls and rooms publish to a set of peers, and never reach a relay. Only
  broadcasts with the `Granted` audience do. Do not put a relay's endpoint id
  in a `Peers` set: a relay that pulls such a broadcast serves it to everyone
  it admits.

## Where to go next

- [Serve browsers](browsers.md)
- [Publish through a relay](publish-through-relay.md)
- [Peer-to-peer and the relay](../architecture/p2p-relay.md) explains pulls.
