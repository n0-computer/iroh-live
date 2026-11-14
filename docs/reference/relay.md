# Relay flags and endpoints

`iroh-live-relay` is the iroh-live relay server. [Relays](../concepts/relays.md)
explains what it does, and [Run the iroh-live relay](../how-to/run-relay.md)
how to deploy it.

```sh
cargo run -p iroh-live-relay -- [FLAGS]
```

On start, the relay prints its endpoint id and the addresses it listens at.
The first SIGINT or SIGTERM drains the browser sessions, and a second one
ends the relay at once.

## Flags

| Flag | Default | Description |
|---|---|---|
| `--bind <ADDR>` | `[::]:4443` | WebTransport bind address, over UDP |
| `--http-bind <ADDR>` | the address `--bind` bound, over TCP | HTTP bind address for the web viewer. The page connects to its own origin, so another port needs `?url=` |
| `--open-pulls` | off | Lets any session make the relay pull a ticket, see [Pulls](#pulls). For local and development use |
| `--auth-key <PATH>` | none | Verifies JWTs from browsers and iroh clients with the keys in this file, a JWK or a JWK set |
| `--auth-url <URL>` | none | Asks this auth server about every browser session, as moq-relay does |
| `--auth-public <PATTERN>` | none | Lets every browser session publish and subscribe at these patterns |
| `--auth-public-publish <PATTERN>` | none | Lets every browser session publish at these patterns |
| `--auth-public-subscribe <PATTERN>` | none | Lets every browser session subscribe at these patterns |

The pattern flags take a comma-separated list. `--auth-key` conflicts with
`--auth-url` and the `--auth-public` flags.

TLS certificates are self-signed and generated at startup. There is no ACME.

## Access flags

The relay decides browsers and iroh clients separately, since only iroh
clients have an endpoint id.

Without auth flags:

- An iroh client may read the whole relay, and publish at the paths that name
  its endpoint id, `live/<id>/...` and `rooms/<topic>/<id>/...`.
- A browser may read anything, and publish a name of one segment, which keeps
  it out of `live/` and `rooms/`.

With `--auth-key`:

- A session with a valid JWT gets what the token's claims grant, in place of
  the rules above, and an invalid token is refused.
- A session **without** a token keeps the rules above. Anonymous browsers and
  iroh clients still read every broadcast, and anonymous browsers can still
  publish names of one segment. `--auth-key` changes what token holders may
  do. It takes nothing from sessions without a token.
- Browsers are decided by moq-auth's reference policy,
  `moq_auth::serve::Policy`, and iroh clients by a `Verify` over the same key
  file, with `relay_policy()` as the fallback.
- The relay reads the key file again when it changes, for browsers and iroh
  clients alike. A rotated or removed key takes effect for the next session
  without a restart. A file that is gone or holds no key refuses every token.
  Sessions already admitted keep their authorization until it expires.

A browser presents its JWT as `?jwt=<token>` in the URL or as a SETUP token,
and a session that presents two different JWTs is refused. An iroh client
presents it through `RelayConfig::tokens`. The relay decides an iroh client
once, and closes its session when the authorization expires. The client's
relay link then redials with the tokens set on it last.

With `--auth-url` or an `--auth-public` flag, moq-relay's own auth decides
browser sessions, and iroh clients keep the default rules. A binary that
embeds the relay sets another iroh policy in `Config::iroh_policy`.

## Pulls

A session that names a broadcast ticket as its path asks the relay to
*pull* it. The relay then dials the ticket's publisher over iroh, and serves
the broadcast under that name. The relay pulls only for:

- a session whose JWT, verified with `--auth-key`, grants reading the
  ticket's path, or
- any session, with `--open-pulls`.

A relay without either pulls nothing, and refuses a session that asks. With
`--auth-url` or `--auth-public`, browsers pull nothing.

The relay pulls only a ticket whose path is the publisher's own, such as
`live/<peer>/<name>` or `rooms/<topic>/<peer>/<name>`, and refuses others with
`NotOwner`. A pull ends ten seconds after the last session that named it.

| Pull limit | Default |
|---|---|
| Pulls the relay holds at once | 256 |
| Pulls one client holds at once (a browser by IP address, an iroh client by endpoint id) | 16 |
| New dials one client may start in a burst | 8 |
| Time to earn one more dial | 5 s |

A binary that embeds the relay sets other limits in `Config::pull_limits`.

## Ports

| Port | Protocol | Carries |
|---|---|---|
| 4443 | UDP | WebTransport over HTTP/3, for browsers |
| 4443 | TCP | The web viewer and `/certificate.sha256`, over plain HTTP |
| chosen by iroh | UDP | The relay's iroh endpoint, for nodes |

## HTTP endpoints

| Path | Serves |
|---|---|
| `GET /` | The web viewer. `?name=<TICKET or name>` opens a broadcast |
| `GET /certificate.sha256` | The TLS certificate's fingerprint, which the page pins |
| `GET /{path}` | Static files of the viewer, with CORS |

## Files

The relay keeps its iroh secret key in `$IROH_LIVE_RELAY_DATA/iroh_secret_key`,
or else in `iroh-live-relay/iroh_secret_key` under the platform's data
directory (`~/.local/share` on Linux). The key keeps the relay's endpoint id
across restarts. A file that does not hold a key is an error, never a reason
to make a new one.
