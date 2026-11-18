# Relay flags and endpoints

`iroh-live-relay` is the iroh-live relay server. [Relays](../concepts/relays.md)
explains what it does, and [Run the iroh-live relay](../how-to/run-relay.md)
how to deploy it.

```sh
cargo run -p iroh-live-relay -- [FLAGS]
```

On start, the relay prints its endpoint id, its access, one line per kind of
session, and the addresses it listens at. The first SIGINT or SIGTERM drains
the browser sessions, and a second one ends the relay at once.

## Flags

| Flag | Default | Description |
|---|---|---|
| `--bind <ADDR>` | `[::]:4443` | WebTransport bind address, over UDP |
| `--http-bind <ADDR>` | the address `--bind` bound, over TCP | HTTP bind address for the web viewer. The page connects to its own origin, so another port needs `?url=` |
| `--auth-key <PATH>` | none | Verifies JWTs from browsers and iroh clients with the keys in this file, a JWK or a JWK set |
| `--rcan-principal <KEY[@ROOT]>` | none | Accepts rcan capability chains that this key issued, from iroh clients. Repeatable |
| `--auth-url <URL>` | none | Asks this auth server about every browser session, as moq-relay does |
| `--auth-public <PATTERN>` | none | Lets sessions without a token publish and subscribe at these patterns |
| `--auth-public-publish <PATTERN>` | none | Lets sessions without a token publish at these patterns |
| `--auth-public-subscribe <PATTERN>` | none | Lets sessions without a token subscribe at these patterns |
| `--auth-public-own-paths` | off | Lets iroh clients without a token publish at their own paths |
| `--auth-own-paths` | off | Lets iroh clients with a verified token also publish at their own paths |
| `--max-session <DURATION>` | `24h` | Bounds how long a session admitted with a token lasts |
| `--allow-unexpiring-tokens` | off | Accepts tokens that never expire |
| `--open-pulls` | off | Lets any session that may read a ticket's path make the relay pull it, see [Pulls](#pulls) |

The pattern flags take a comma-separated list. `--auth-url` conflicts with
the `--auth-public` flags, as in moq-relay. Every `--auth-*` and `--rcan-*`
flag makes the relay private.

TLS certificates are self-signed and generated at startup. There is no ACME.

## Access

*Own paths* are the paths that name a client's endpoint id:
`live/<id>/**` and `rooms/*/<id>/**`. Only iroh clients have an endpoint id.

### Open

Without any `--auth-*` or `--rcan-*` flag the relay is open, and verifies no
tokens:

- An iroh client reads the whole relay and publishes at its own paths.
- A browser reads the whole relay and publishes names of one segment, which
  keeps it out of `live/` and `rooms/`.

The relay warns at every start that it is open.

### Private

With any `--auth-*` or `--rcan-*` flag the relay is private:

- A session with a valid token gets what the token grants, and nothing more.
  `--auth-own-paths` adds an iroh client's own paths. A token that does not
  verify is refused. It never falls back to the public area.
- A session without a token gets exactly the `--auth-public*` patterns,
  rooted at `/`, for browsers and iroh clients alike. Without patterns it is
  refused. `--auth-public-own-paths` adds an iroh client's own paths.
- A session admitted with a token lasts at most `--max-session`. At that
  bound the relay sends a GOAWAY, and an iroh client dials a new session
  with its token while the old one serves for up to five seconds. The client
  sees no refusal. A session whose token expires first closes as expired.
- A token without an expiry is refused, unless `--allow-unexpiring-tokens`
  is set.

A private relay with public patterns has a *public area*. For example,
`--auth-key public.jwk --auth-public-subscribe 'demo/**'` lets anyone watch
`demo/`, and only token holders read the rest.

The relay refuses to start when the flags contradict each other:

- A public publish pattern that reaches `live/*/**` or `rooms/*/*/**`.
  Viewers could not tell an anonymous publisher there from the real one. Use
  `--auth-public-subscribe` for read-only access.
- A public pattern without a wildcard, such as `demo`. Write `demo/**`.
- A private relay that would admit no reader: no key, no principal, no public
  subscribe pattern and no `--auth-url`.
- `--auth-own-paths` or `--allow-unexpiring-tokens` without `--auth-key` or
  `--rcan-principal`.
- An `--rcan-principal` root with `*` or `**`, or one that is not a valid
  path.

### Which verifier decides a token

| Token | Browsers | iroh clients |
|---|---|---|
| A JWT | moq-auth's reference policy with `--auth-key`, or the server of `--auth-url` | `KeyFile` over `--auth-key` |
| An rcan chain (`rcan1:...`) | Refused: a browser has no key to bind a chain to | The `--rcan-principal` whose key issued the first link |

The relay decides an iroh session on one token, the first of the client's
`RelayConfig::tokens`. A chain needs an `iroh://` relay link and the key its
last link names. `--auth-url` decides browsers only, so iroh clients then
need `--auth-key` or `--rcan-principal`, and the relay warns without them.

A browser presents its JWT as `?jwt=<token>` in the URL or as a SETUP token.
A session that presents two different JWTs is refused.

### Key changes

The relay reads the key file again when it changes, and checks it once a
second. A new session sees the change at once. Every live iroh session that
presented a token is decided again. It closes as unauthorized if its token
no longer verifies. If the token still verifies but now grants something
else, more or less, the relay sends a GOAWAY, and the client's new session
gets the new grant. The old session serves until then, for at most five
seconds. A browser session keeps its grant until its token or
`--max-session` expires. A file that is gone refuses every token and
closes the iroh sessions it admitted. A file that is there but does not parse,
as it may be for a moment while an editor rewrites it, refuses new tokens and
leaves live sessions alone until it parses again. To revoke every token,
remove the file or write a set without its keys.

Principals are fixed for the relay's lifetime. Removing one takes a restart.

### Startup line

```text
access: private (from --auth-key, --auth-public-subscribe, --rcan-principal)
  without a token: browsers and iroh clients subscribe demo/**, publish nothing
  JWTs: verified with public.jwk, for browsers and iroh clients
  rcan chains: 1 principal (b5f4a1c2e0 at /), iroh clients only
  with a token: the token's grant, for at most 24h
  pulls: sessions whose token reads the ticket's path
```

### Embedding

A binary that embeds the relay starts from `Config::default()`, which holds
the command line's defaults, and sets fields. `Config::access` names the mode,
an `AccessMode`, when the flags do not. `Config::iroh_policy` replaces the key file, the
principals and the public patterns for iroh clients, and needs auth settings
or `Config::access`. `Config::iroh_tokens()` returns the relay's own token
verifier to compose into it.

## Pulls

A session that names a broadcast ticket as its path asks the relay to
*pull* it. The relay then dials the ticket's publisher over iroh, and serves
the broadcast under that name. The relay pulls only for:

- a session whose token, a JWT or a chain, grants reading the ticket's path,
  or
- with `--open-pulls`, a session the relay admits to read the ticket's path.
  On an open relay that is every session. On a private one it is a token
  holder, or a session without a token whose public patterns reach the path.

A relay that verifies no tokens and lacks `--open-pulls` pulls nothing, and
refuses a session that asks. With `--auth-url`, browsers pull nothing.

An iroh client that names a ticket gets two decisions. Its own rights, what
it may publish, are decided at the path it dialed, the ticket's string.
Whether it reads the pulled broadcast is decided at the ticket's path. So a
token for `live/<id>/cam` lets it read that ticket and nothing else.

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
| 4443 | TCP | The web viewer, `/certificate.sha256` and moq-relay's HTTP routes, over plain HTTP |
| chosen by iroh | UDP | The relay's iroh endpoint, for nodes |

## HTTP endpoints

| Path | Serves |
|---|---|
| `GET /` | The web viewer. `?name=<TICKET or name>` opens a broadcast, and `#jwt=<TOKEN>` presents a token |
| `GET /certificate.sha256` | The TLS certificate's fingerprint, which the page pins |
| `GET /announced`, `GET /announced/{prefix}` | The broadcast paths the session may read, one per line |
| `GET /fetch/{path}` | A group of a track |
| `GET /{path}` | Static files of the viewer, with CORS |

`/announced` and `/fetch` follow the same access rules as a WebTransport
session, and take a token as `?jwt=`. Over plain HTTP that token is visible
to the network.

The viewer reads its token from the URL fragment, `#jwt=`, which the browser
never sends to a server, and removes it from the address bar. It still reads
`?jwt=` from older links, which is discouraged, since the query reaches the
server. It sends the token only to a relay on its own origin.

## Files

The relay keeps its iroh secret key in `$IROH_LIVE_RELAY_DATA/iroh_secret_key`,
or else in `iroh-live-relay/iroh_secret_key` under the platform's data
directory (`~/.local/share` on Linux). The key keeps the relay's endpoint id
across restarts. A file that does not hold a key is an error, never a reason
to make a new one.
