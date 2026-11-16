# Access defaults and policy API

The access types live in `moq-ext-access`, and `iroh_live::node::policy`
re-exports them with the peer type fixed to `EndpointId`.
[Access control](../concepts/access.md) explains the model, and
[Control access](../how-to/control-access.md) has recipes.

## Defaults

| Where | Default policy | Every peer may |
|---|---|---|
| `NodeConfig::default()` | `policy::Open(Grant::subscribe_only())` | Read, and publish nothing |
| `LiveLayout::config()` | `LiveLayout::policy()` | Read, and publish under `live/<its id>/` |
| `layout::config((LiveLayout::default(), RoomsLayout))` | the union of both layouts | Also publish under `rooms/<topic>/<its id>/` |
| `iroh-live-relay`, iroh clients | `relay_policy()` | Read the whole relay, and publish under `live/<its id>/` and `rooms/*/<its id>/` |
| `iroh-live-relay`, browsers | `browser_grant(path)` | Read anything, and publish a name of one segment |

`NodeConfig::decide_timeout` defaults to 10 seconds.

## Cheat sheet

| To ... | Use |
|---|---|
| let every peer read, and nobody publish | the default, `Open(Grant::subscribe_only())` |
| let every peer read, and publish only under its own id | `LiveLayout::default().config()` |
| also allow room broadcasts | `layout::config((LiveLayout::default(), RoomsLayout))` |
| admit only known peers | `layout.policy().filter(..)`, plus `Node::reauthorize` when the set changes |
| ask a person before admitting | `Queue`, with a longer `decide_timeout` |
| require a token from moq-relay's issuer | `Verify::new(JwtVerifier::new(keys))` |
| delegate rights bound to a device key | `Verify::new(RcanVerifier::new(principal, root))` |
| let peers without a token in too | `.with_fallback(Open(..))` on the `Verify` |
| give rights that end at a set time | return an `Authorization` with `expires` |
| show a broadcast to every granted session, relays included | `Audience::Granted` |
| show a broadcast to a few peers, and never to a relay | `Audience::Peers(watchable)` |
| change who sees a broadcast | update the `Peers` set, or `Publication::set_audience` |
| play a ticket under any policy | `Node::subscribe_ticket` |
| present a token when dialing | `ConnectOptions::token`, or `RelayConfig::tokens` for a relay |

## Grants

A `Grant` has two sets of path patterns, `subscribe` and `publish`. A pattern
segment `*` matches one segment, and `**` matches the rest of the path.

| Constructor | Read | Publish |
|---|---|---|
| `Grant::everything()` | everything | everything |
| `Grant::subscribe_only()` | everything | nothing |
| `Grant::nothing()` | nothing | nothing |
| `Grant::publish_under(patterns)` | everything | under `patterns` |

## Audiences

| `Audience` | Reaches |
|---|---|
| `Audience::Granted` | Every session whose grant covers the path, attached relays included |
| `Audience::Peers(set)` | Only the peers in a watched set, and never a relay |

`Audience::peers([peer])` builds a fixed set. The node gives each session the
exact paths of the publications it may see, and moq enforces that on the
session. A peer that loses a path loses it at once, and what it read there
ends with `Unauthorized`.

## Policies

A policy implements `Policy<EndpointId>`, with `async fn decide(&self,
request: &PeerRequest) -> Decision`.

| Policy | Decides |
|---|---|
| `Open(grant)` | Admits every peer with `grant` |
| `Closed` | Rejects every peer |
| `from_fn(f)` | Calls the synchronous closure `f` |
| `inner.filter(predicate)` | Rejects the peers `predicate` refuses, and asks `inner` about the rest |
| `Queue::new(capacity)` | Hands each decision to the application |
| `Verify::new(verifier)` | Lets a token decide, with a fallback for peers without one |
| `LiveLayout::policy()`, any `Layout` | Admits every peer with a grant to publish under its scope |

`PeerRequest` holds:

- `peer()`: the peer's endpoint id, which the transport authenticates.
- `direction()`: `Direction::Dialed` if this node dialed the peer, else
  `Direction::Accepted`.
- `event()`: why the node asks, see below.
- `path()`, `query(key)` and `token()`: what the peer asked for in its MoQ
  SETUP. These are not authentic unless a token signs them.

`Decision` is `Result<Authorization, Rejection>`. An `Authorization` holds the
`grant` and an optional `expires` time, and a `Grant` converts into one with
no expiry. A `Rejection` has a reason, which stays local, and a code, which
the peer sees when its session closes. The code is `Unauthorized` unless
`Rejection::with_code` sets another.

## When the node asks

| When | Event |
|---|---|
| It accepts a session, after the peer's MoQ SETUP | `Connect` |
| It dials a peer, with `Direction::Dialed` and no token | `Connect` |
| An authorization reaches its `expires` time | `Reauthorize` |
| The application calls `Node::reauthorize(peer)` or `Node::reauthorize_all()` | `Reauthorize` |

The node decides each session on its own, with the path and token that
session opened with. A peer's second session asks the policy again, and a
token counts only for the session that presented it. `reauthorize_all` runs
at most 16 decisions at once.

A new decision replaces the session's authorization, and a rejection closes
the session. A first decision that takes longer than `decide_timeout` rejects
the session with `Timeout`. A later one that times out, or finds a `Queue`
full, keeps the session's authorization, and the node asks again later.

On the dialing side, a rejection by this node's own policy fails the dial with
`Error::Rejected`. A rejection by the remote node shows as `Error::Refused`,
or as a session that opens and then closes with the remote's code, since
moq-lite-06 has no message for a refusal.

`Node::authorization_for(peer)` returns the authorization of the peer's newest
session that holds one, and `Session::grant()` the grant of one session.

## Ticket claims

`Node::subscribe_ticket` lets any node play a ticket, whatever its policy.
Before it subscribes, the node *claims* the ticket's path for the ticket's
publisher with `Node::claim_path`. The claim adds exactly that path to what
the publisher may publish, on every session with it, while the subscription
lives.

The node checks that the publisher owns the path. By default, exactly one
segment of the path is the publisher's endpoint id in hex, and no other
segment parses as an endpoint id. A `NodeConfig::layout`, which
`NodeConfig::with_layout` sets, replaces that rule with the layout's `owner`.
`LiveLayout::config()` and `layout::config` set it to their layouts, so a
node configured from `LiveLayout` alone refuses a claim for a room path. A
path that fails the check returns `Error::NotOwner`.

| Limit | Value |
|---|---|
| Claimed paths per peer | 64 |
| Bytes of claimed paths per peer | 16 KiB |

## Tokens

A peer presents a token in `ConnectOptions::token`. It travels in the MoQ
SETUP and, since moq-lite-06 has no token field there, also as `?jwt=` in the
requested path. `SessionRequest::token` reads the SETUP token, then the `jwt`
query parameter, then `token`.

Under `Verify`, a token the verifier rejects is a rejection and never falls
through to the fallback. When the node decides again about a live session
whose token has expired, the fallback decides, and the session narrows to the
fallback's rights.

| Feature | Verifier | Token | Bound to the peer |
|---|---|---|---|
| `jwt` | `JwtVerifier::new(keys)` | a JWT, as moq-relay reads it | no, whoever holds it may use it |
| `rcan` | `RcanVerifier::new(principal, root)` | an rcan capability chain | yes, the last link names the peer's endpoint id |

A JWT's claims become the grant, relative to the path the peer requested, and
its `exp` claim becomes `expires`. An rcan chain has at most eight links of at
most 2 KiB each, and is encoded as `rcan1:` followed by lowercase base32. Its
authorization expires one second after the earliest link's `valid_until`.

A relay link presents `RelayConfig::tokens` on every dial, over `iroh://` and
`https://` alike. The relay's own flags are in the
[relay reference](relay.md#access-flags).
