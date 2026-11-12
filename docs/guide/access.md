# Access control

Every iroh endpoint has a key, and its endpoint id is the public half. When a
peer connects, the QUIC handshake proves that the peer holds the key of its
endpoint id. A node therefore always knows who is on the other side of a
session, and access control starts from that id.

A node answers two questions about each peer:

- **Admission:** may the peer hold a session with this node at all?
- **Authorization:** what may it read from this node, and what may it publish
  into it?

One *policy* answers both. It decides each peer when a session opens, and
again later when something changes. Its answer is an `Authorization`, whose
main part is a `Grant`, or a `Rejection`, which closes the session.

This page shows the defaults, the policies you can build, how the node asks
them, tokens, the relay's flags, and what the model protects against. The
types live in `moq-ext-access`. `iroh_live::node::policy` re-exports them with
the peer type fixed to an `EndpointId`.

For the reasoning behind this design, with a walk through one session's life
and where audiences come in, read
[Authorization and audiences, explained](../architecture/auth-explained.md).

## The defaults

You get access control without writing any. Each crate starts from a default:

| Where | Default policy | Effect |
|---|---|---|
| `NodeConfig::default()` | `policy::Open(Grant::subscribe_only())` | Every peer may read and publish nothing |
| `LiveLayout::config()` | `LiveLayout::policy()` | Every peer may read, and publish under `live/<its id>/` |
| `layout::config((LiveLayout::default(), RoomsLayout))` | the union of both layouts | Also `rooms/<topic>/<its id>/` |
| `iroh-live-relay`, iroh clients | `relay_policy()` | Read the whole relay, publish under `live/<its id>/` and `rooms/*/<its id>/` |
| `iroh-live-relay`, browsers | `browser_grant(path)` | Read anything, publish a name of one segment |

A node with the default config still plays tickets. `Node::subscribe_ticket`
lets the ticket's publisher publish that one path while the subscription
lives (see [ticket claims](#ticket-claims)).

## Grants and audiences

A `Grant` has two sets of moq patterns: `subscribe` and `publish`. A pattern
segment `*` matches one segment, and `**` matches the rest of the path.

- `subscribe` bounds what the peer may read from this node.
- `publish` bounds what the peer may announce into this node's route table.

`Grant::everything()`, `Grant::subscribe_only()`, `Grant::nothing()` and
`Grant::publish_under(patterns)` cover the common cases. `publish_under` lets
the peer read anything.

The grant is an upper limit. What a peer sees also depends on each
publication's `Audience`:

- `Audience::Granted` reaches every session whose grant covers the path,
  attached relays included.
- `Audience::Peers(set)` reaches only the peers in a watched set, and never a
  relay. Change the set through a clone of the `Watchable`.

The node gives each session the exact paths of the publications it may see.
moq enforces that on the session itself, so a peer cannot subscribe to a path
it was not given. When a publication, an audience or an authorization
changes, the node authorizes every affected session again. A peer that loses a
path loses it at once, and what it read there ends with `Unauthorized`.

## Policies

A policy implements `Policy<EndpointId>`. The node calls its `decide` with a
`PeerRequest`, which holds:

- `peer()`: the peer's endpoint id.
- `dialed()`: whether this node dialed the peer.
- `event()`: why the node asks, see [when the node asks](#when-the-node-asks).
- `path()`, `query(key)` and `token()`: what the peer asked for in its MoQ
  SETUP.

`decide` returns a `Decision`, which is `Result<Authorization, Rejection>`.
An `Authorization` holds the `grant` and an optional `expires` time. A
`Grant` converts into an `Authorization`
with no expiry. A `Rejection` has a reason, which stays local, and a code,
which the peer sees when its session closes. The code is `Unauthorized`
unless you set another with `Rejection::with_code`.

These building blocks cover most applications:

| Policy | Decides |
|---|---|
| `Open(grant)` | Admits every peer with `grant` |
| `Closed` | Rejects every peer |
| `from_fn(f)` | Calls the synchronous closure `f` |
| `inner.filter(predicate)` | Rejects the peers `predicate` refuses, and asks `inner` about the rest |
| `Queue::new(capacity)` | Hands each decision to the application |
| `Verify::new(verifier)` | Lets a token decide, see [tokens](#tokens) |

To combine policies in other ways, such as admitting what either of two
admits, write a policy of your own (see below) that asks both.

### An open LAN node

A node on a network you trust lets every peer publish and read anything:

```rust
use iroh_live::{
    NodeConfig,
    node::{Grant, policy},
};

let config = NodeConfig::default().with_policy(policy::Open(Grant::everything()));
```

### A layout as a policy

A *layout* is a path convention. It says where each peer publishes, and which
peer owns a path. `LiveLayout` puts a peer's broadcasts at
`live/<peer>/<name>`, and its policy admits every peer with a grant to publish
there only. So no peer can publish at a path that names another peer:

```rust
use iroh_live::{Node, layout::LiveLayout};

let layout = LiveLayout::default();
let node = Node::new(endpoint, layout.config());
```

`layout.config()` is `NodeConfig::default().with_policy(layout.policy())`.
For a node that also runs rooms, pass both layouts to the free function
`iroh_live::layout::config`. A pair of layouts is their union:

```rust
use iroh_live::{layout::{self, LiveLayout}, rooms::RoomsLayout};

let config = layout::config((LiveLayout::default(), RoomsLayout));
```

A layout of your own implements the `Layout` trait. `scope` returns the
patterns a peer may publish under, and `owner` returns the peer that owns a
path. The two must agree: the owner of any path in `scope(peer)` is `peer`.

```rust
use iroh::EndpointId;
use iroh_live::{
    layout::{self, Layout, LiveLayout},
    moq_net::{Pattern, Patterns},
};

/// Puts a peer's broadcasts at `stage/<peer>/<name>`.
#[derive(Debug, Clone)]
struct Stage;

impl Layout<EndpointId> for Stage {
    fn scope(&self, peer: &EndpointId) -> Patterns {
        let pattern: Pattern = format!("stage/{peer}/**").parse().expect("a valid pattern");
        Patterns::from(pattern)
    }

    fn owner(&self, path: &str) -> Option<EndpointId> {
        let (peer, name) = path.strip_prefix("stage/")?.split_once('/')?;
        let id: EndpointId = peer.parse().ok()?;
        (id.to_string() == peer && !name.is_empty()).then_some(id)
    }
}

let config = layout::config((LiveLayout::default(), Stage));
```

`NodeConfig::layout` is a second, separate use of a layout. It sets the
owner rule for [ticket claims](#ticket-claims). `layout::config` leaves it
unset, so claims keep the generic rule.

### Contacts only

A filter narrows a policy. This node admits only its contacts, each with the
layout's grant:

```rust
use std::collections::BTreeSet;

use iroh::EndpointId;
use iroh_live::{Node, NodeConfig, layout::LiveLayout, node::policy::Policy};
use n0_watcher::Watchable;

let contacts = Watchable::new(BTreeSet::<EndpointId>::new());
let known = contacts.clone();
let policy = LiveLayout::default()
    .policy()
    .filter(move |request| known.get().contains(request.peer()));
let node = Node::new(endpoint, NodeConfig::default().with_policy(policy));

// Later, when bob stops being a contact:
let mut without_bob = contacts.get();
without_bob.remove(&bob);
contacts.set(without_bob).ok();
node.reauthorize(bob);
```

The node does not know what the filter reads, so it does not notice when
the contact list changes. `Node::reauthorize(peer)` makes it decide again
about one peer, and `Node::reauthorize_all()` about every peer with a session.
Here the new decision rejects bob, and the node closes every session with him.

The filter covers both directions. A dial to a stranger fails with
`Error::Rejected`. [`iroh-live/examples/contacts_only.rs`](../../iroh-live/examples/contacts_only.rs)
runs this with three nodes in one process. `calls::Calls` works on such a
policy: a stranger's ring never reaches the application.

### A policy of your own

A policy that needs I/O, such as a database lookup, implements the trait with
`async fn decide`. It can also give the authorization an expiry, so the node
asks again when it passes:

```rust
use std::time::{Duration, SystemTime};

use iroh_live::node::{
    Grant,
    policy::{Authorization, Decision, PeerRequest, Policy, Rejection},
};

#[derive(Debug)]
struct Members;

impl Policy<iroh::EndpointId> for Members {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        let peer = request.peer();
        if !is_member(peer).await {
            return Err(Rejection::new("not a member"));
        }
        Ok(Authorization {
            grant: Grant::subscribe_only(),
            expires: Some(SystemTime::now() + Duration::from_secs(3600)),
        })
    }
}
```

When the member list changes, call `Node::reauthorize_all()`. The node then
decides again about every peer with a session, at most 16 at once.

### Admission by the application

A `Queue` hands each decision to the application, as a person who answers a
call does. The node puts each request in the queue, and the application
answers each `Pending` it takes with `admit` or `reject`. Dropping a `Pending`
rejects it.

```rust
use std::time::Duration;

use iroh_live::{
    NodeConfig,
    node::{Grant, policy::{Queue, Rejection}},
};

let queue = Queue::new(16);
let config = NodeConfig {
    // A person needs longer than the default 10 seconds.
    decide_timeout: Duration::from_secs(60),
    ..NodeConfig::default().with_policy(queue.clone())
};

// In a task of its own:
while let Some(pending) = queue.next().await {
    if ask_the_user(pending.request().peer()).await {
        pending.admit(Grant::subscribe_only());
    } else {
        pending.reject(Rejection::new("declined"));
    }
}
```

A full queue rejects at once with `TooManyRequests`.

`decide_timeout` covers the whole policy. Some nodes admit every peer with a
small grant and ask the application for more. Such a policy waits for the
queue with a timeout of its own, and falls back to the small grant:

```rust
use iroh_live::node::{
    Grant,
    policy::{Decision, PeerRequest, Policy, Queue},
};

#[derive(Debug)]
struct AskForMore(Queue);

impl Policy<iroh::EndpointId> for AskForMore {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        match tokio::time::timeout(Duration::from_secs(30), self.0.decide(request)).await {
            Ok(Ok(authorization)) => Ok(authorization),
            _ => Ok(Grant::subscribe_only().into()),
        }
    }
}
```

## When the node asks

The node asks its policy at these times:

| When | Event |
|---|---|
| It accepts a session, after the peer's MoQ SETUP | `Connect` |
| It dials a peer, with `dialed()` set and no token | `Connect` |
| An authorization reaches its `expires` time | `Reauthorize` |
| The application calls `Node::reauthorize(peer)` or `Node::reauthorize_all()` | `Reauthorize` |

The node decides each session on its own, with the path and token that
session opened with. A peer's second session asks the policy again, so a
`Queue` sees it, and a token counts only for the session that presented it.
`Node::reauthorize(peer)` decides every session with the peer again. A session
whose first decision is still running when the application calls
`reauthorize` or `reauthorize_all` holds nothing once it registers, and is
decided again.

A new decision replaces the session's authorization. A rejection closes the
session with the rejection's code. When an authorization expires before the
new decision arrives, the session may do nothing until it does.

Each decision may take up to `NodeConfig::decide_timeout`, 10 seconds by
default. A first decision that takes longer rejects the session with
`Timeout`. A later one that times out, or finds a `Queue` full, is not a
rejection: the session keeps its authorization, and the node asks again
later.

On the dialing side, a rejection by this node's own policy fails the dial with
`Error::Rejected`. moq-lite-06 has no message for a refusal, so a rejection by
the remote node shows in one of two ways. The dial fails with
`Error::Refused`, or the dial succeeds and the session then closes with the
remote's code.

`Node::grant_for(peer)` returns the authorization of the peer's newest session
that holds one, and `Session::grant()` the grant of one session.

## Ticket claims

`Node::subscribe_ticket` lets any node play a ticket, whatever its policy.
Before it subscribes, the node *claims* the ticket's path for the ticket's
publisher with `Node::claim_path`. The claim adds exactly that path to what
the publisher may publish, on every session with it, for as long as the
subscription lives.

A claim must not let one peer stand in for another, so the node checks that
the publisher owns the path. By default, exactly one segment of the path is
the publisher's endpoint id in hex, and no other segment parses as an
endpoint id. A `NodeConfig::layout` replaces that rule with the layout's
`owner`. A path that fails the check returns `Error::NotOwner`. A node accepts
at most 64 claimed paths and 16 KiB of claimed paths per peer.

## Tokens

A peer can present a token when it connects, in `ConnectOptions::token`. It
travels in the MoQ SETUP and, since moq-lite-06 has no token field there, also
as `?jwt=` in the requested path. The accepting node reads it with
`SessionRequest::token`, which takes the SETUP token, then the `jwt` query
parameter, then `token`.

`Verify` turns a `TokenVerifier` into a policy:

- With a token, the verifier decides. A token it rejects is a rejection, and
  never falls through to the fallback, so a forged token gains nothing.
- Without a token, the fallback decides. The default fallback is `Closed`,
  which makes a token required.
- When the node decides again about a live session and its token has
  expired, the fallback decides. The session narrows to the fallback's
  rights instead of closing.

The node dials with no token, so the fallback decides every dial. Under
`Closed` the node can dial nobody. Give a node that dials a fallback, or check
`request.dialed()` in it.

Two verifiers ship behind features of `moq-ext-access`, which `iroh-live-node`
and `iroh-live` forward:

| Feature | Verifier | Token | Bound to the peer |
|---|---|---|---|
| `jwt` | `JwtVerifier::new(keys)` | a JWT, as moq-relay reads it | no: whoever holds it may use it |
| `rcan` | `RcanVerifier::new(principal, root)` | an rcan capability chain | yes: the last link names the peer's endpoint id |

`JwtVerifier` checks the signature against a `moq_auth::KeySet`, and the
claims' patterns become the grant, relative to the path the peer requested.
The `exp` claim becomes the authorization's `expires`. A JWT is a bearer
token, so prefer rcan when the peer has an iroh key.

### An rcan-delegated publisher

An rcan chain starts at a *principal*, the key whose space a node shares,
such as an account key. The principal issues a `MoqCapability` to a key, and
each holder may delegate a narrower capability on, up to eight links. The
last link must name the endpoint id of the peer that presents it, so a stolen
chain is useless without that peer's key.

Alice's node shares `spaces/alice`. It verifies chains from alice's account
key, and lets peers without a token subscribe:

```rust
use iroh_live::{
    NodeConfig,
    node::{Grant, policy::{Open, RcanVerifier, Verify}},
};

let verifier = RcanVerifier::new(alice_account.public().as_verifying_key(), "spaces/alice");
let config = NodeConfig::default()
    .with_policy(Verify::new(verifier).with_fallback(Open(Grant::subscribe_only())));
```

Alice's account delegates publishing under `spaces/alice/bob/` to bob's
account, and bob's account delegates it on to his device. The `rcan` crate
signs the links, and the patterns of a `MoqCapability` are relative to its
root:

```rust
use std::time::Duration;

use iroh_live::{
    ConnectOptions,
    moq_net::Pattern,
    node::policy::{MoqCapability, encode_chain},
};
use rcan::{Expires, Rcan};

let capability = MoqCapability::new("spaces/alice").with_publish("bob/**".parse::<Pattern>()?);
let issued = Rcan::issuing_builder(
    alice_account.as_signing_key(),
    bob_account.public().as_verifying_key(),
    capability.clone(),
)
.sign(Expires::valid_for(Duration::from_secs(24 * 3600)));
let delegated = Rcan::delegating_builder(
    bob_account.as_signing_key(),
    device.id().as_verifying_key(),
    alice_account.public().as_verifying_key(),
    capability,
)
.sign(Expires::valid_for(Duration::from_secs(3600)));

// The device presents the chain when it dials alice's node.
let options = ConnectOptions {
    token: Some(encode_chain(&[issued, delegated])),
    ..Default::default()
};
device.connect_with(alice_addr, options).await?;
```

The device may now publish under `spaces/alice/bob/`. The authorization
expires one second after the earliest link's `valid_until`, and the device
then narrows to the fallback's grant. An encoded chain is `rcan1:` followed by
the lowercase base32 of the links, at most 2 KiB per link.
[`iroh-live-node/examples/delegated_publisher.rs`](../../iroh-live-node/examples/delegated_publisher.rs)
runs both sides in one process.

### Tokens for a relay link

A node presents a token to a relay through `RelayConfig::token`, a `TokenFn`.
`TokenFn::fixed(token)` presents one token. `TokenFn::new(f)` calls `f` each
time the link asks:

```rust
use iroh_live::{RelayConfig, TokenFn};

let link = node.attach_relay(RelayConfig {
    token: Some(TokenFn::fixed(token)),
    ..RelayConfig::iroh(relay)
})?;
```

An `iroh://` link asks before every dial, so a link that redials after its
token expired presents a fresh one. An `https://` link asks once, when it
attaches.

## The relay's auth flags

`iroh-live-relay` decides browsers and iroh clients separately, since only
iroh clients have an endpoint id.

Without flags, an iroh client may read the whole relay and publish at the
paths that name its endpoint id: `live/<id>/...` and `rooms/<topic>/<id>/...`.
A browser may read anything and publish a name of one segment, which keeps it
out of `live/` and `rooms/`.

| Flag | Effect |
|---|---|
| `--auth-key <PATH>` | Verifies JWTs from browsers and iroh clients with the keys in this file, a JWK or a JWK set. A valid token grants what its claims say, an invalid one is refused, and a session without a token keeps the default rules |
| `--auth-url <URL>` | Hands every browser session to this auth server, as moq-relay does |
| `--auth-public <PATTERN>` | Lets every browser session publish and subscribe at these patterns, as moq-relay does |
| `--auth-public-publish <PATTERN>`, `--auth-public-subscribe <PATTERN>` | The same for one direction |

`--auth-key` conflicts with the other flags. With `--auth-url` or an
`--auth-public` flag, moq-relay's own auth decides browser sessions, and the
relay pulls no tickets for browsers. iroh clients keep the default rules then.

A relay that verifies JWTs:

```sh
cargo run -p iroh-live-relay -- --auth-key relay.jwk
```

An iroh client presents a token signed with the same key through
`RelayConfig::token`, and a browser puts it in the URL as `?jwt=<token>`. With
`--auth-key`, the relay decides iroh clients with
`Verify::new(JwtVerifier::new(keys)).with_fallback(relay_policy())`. A binary
that embeds the relay sets another policy in `Config::iroh_policy`.

The relay decides an iroh client once, when it connects. When the
authorization expires, the relay closes the session, and the client's relay
link redials with a fresh token from its `TokenFn`.

## The trust model

- **Identity.** The transport authenticates a peer's endpoint id, so a
  policy can rely on `request.peer()`. Nothing else in the request is
  authentic unless a token signs it.
- **Paths name their publisher.** A grant that lets a peer publish only at
  paths that name it keeps the peer from standing in for another. The layouts
  and `relay_policy()` do this. moq moves readers between routes that claim
  the same broadcast. So a peer that may announce another publisher's paths
  can take over that publisher's readers.
- **No forwarding.** A node serves only its own publications. It never offers
  a route it learned from one link on another link.
- **Relays are trusted with what they carry.** A relay link sees every
  `Granted` publication, and the node accepts the relay's routes for any path.
  Attach only relays that keep each publisher to its own paths, as
  `iroh-live-relay` and moq-relay's tokens do. `Peers` publications never go
  to a relay.
- **Room tickets are bearer secrets.** Anyone who knows a room's topic id can
  join the room, announce itself, and read the members' room broadcasts. Share
  a room ticket only with the people you invite.
- **Broadcast tickets are not secrets.** A ticket names a publisher and a
  path. The publisher's policy and the publication's audience decide who may
  read it.
- **JWTs are bearer tokens, rcan chains are not.** Anyone who holds a JWT may
  present it. An rcan chain works only for the endpoint its last link names.
