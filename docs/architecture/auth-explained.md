# Who may read what: authorization and audiences in iroh-live

A node in iroh-live accepts QUIC connections from anyone who knows its
endpoint id. Each connection carries a MoQ session, and over that session the
peer can do two things. It can subscribe to broadcasts this node publishes,
and it can announce broadcasts of its own into this node's route table. So
every node has to answer the same questions for every session: who is this,
what may it read, and what may it write?

This page explains how iroh-live answers them. It follows one session from
the dial to the close. On the way it shows where each piece of the
`moq-ext-access` crate fits, what moq-net enforces for us, and where
`Audience` comes into play. The last part weighs whether `Audience` still earns
its place, or whether a policy could do its job.

The [access guide](../guide/access.md) is the reference for the same
material. It lists every policy, flag and default. This page explains why
the pieces have the shape they have.

## Identity comes for free

Every iroh endpoint has an ed25519 key pair, and its endpoint id is the public
key. When a peer connects, the TLS handshake proves that the peer holds the
secret key of the id it claims. A node therefore always knows who is on the
other end, before it reads a single MoQ message. A policy receives that id as
`PeerRequest::peer`, and it can rely on it.

Nothing else in the request is authentic. The path the peer asks for and the
query parameters of that path are what the peer chose to send. A token is
authentic only if it carries a signature that a verifier checks.

Browsers are the exception. A browser has no iroh key, so it cannot dial a
node. It connects over WebTransport to `iroh-live-relay`, which accepts it
with moq-relay's own server. There, a browser is anonymous unless it presents
a JWT. [Relays and browsers](#relays-and-browsers) covers that door.

## Two questions, two parts

Access control in iroh-live is split along two questions, and each question
has one owner.

```
                 who may connect, and with what rights?
  peer ───────►  Policy::decide ──► Authorization { grant, expires }
                                         │
                                         ▼
  publication ─► Audience ──────►  what this link sees of that publication
                 what does this publication show, and to whom?
```

The **policy** answers per session. It sees the peer and its request, and it
returns a grant: the paths the peer may subscribe to, and the paths it may
publish. The node configures one policy, in `NodeConfig::policy`.

The **audience** answers per publication. Whoever publishes a broadcast picks
who sees it. That choice narrows the grant further. It never widens it.

### The policy and its grant

A policy is a trait with one required method, which is async:

```rust
pub trait Policy<P: PeerId>: fmt::Debug + Send + Sync + 'static {
    fn decide(&self, request: &PeerRequest<P>) -> impl Future<Output = Decision> + Send;
}

pub type Decision = Result<Authorization, Rejection>;

pub struct Authorization {
    pub grant: Grant,
    pub expires: Option<SystemTime>,
}

pub struct Grant {
    pub subscribe: Patterns,
    pub publish: Patterns,
}
```

A `Grant` has the same shape as a moq-auth grant, so a token means the same
thing to a node and to a moq relay. Its patterns are moq patterns: `*`
matches one path segment, `**` matches the rest of the path, and anything
else matches itself. Patterns are an allow-list. There is no way to write
"everything except this path".

The two halves of a grant do different jobs:

- `subscribe` is a ceiling on what the peer may read from this node.
- `publish` decides which paths the peer may announce into this node. In
  practice it is a naming rule: it says which peer may be the source of a
  path.

An `Authorization` adds an expiry to the grant. When the expiry passes, the
node asks the policy again. A `Rejection` closes the session. Its code goes
to the peer, and its reason stays in the local log.

The crate ships a few building blocks. `Open(grant)` admits everyone with one
grant, and `Closed` rejects everyone. `from_fn` wraps a synchronous closure,
and `.filter(predicate)` narrows any policy by a predicate. `Queue` hands
each decision to the application, and `Verify` hands it to a token verifier.
A node without a policy uses `Open(Grant::subscribe_only())`: every peer may
read, and no peer may publish.

### The audience of a publication

`Node::publish` takes a path and an audience:

```rust
pub enum Audience<P: PeerId> {
    /// Every session whose grant covers the path, relays included.
    Granted,
    /// These peers, as the set changes. Never offered to relays.
    Peers(n0_watcher::Watchable<BTreeSet<P>>),
}
```

`Granted` hands the decision back to the grant. Every session whose subscribe
patterns cover the path sees the publication. That includes the relays the
node is attached to.

`Peers` names a set of endpoint ids. Only sessions with those peers see the
publication, and only if their grant also covers the path. The set is a
`Watchable`, so the publisher can change it later through a clone. A `Peers`
publication never goes to a relay, even if the relay's id is in the set.

The whole rule is one function in `moq-ext-access`, `visible_paths`. It keeps
a publication on a link when the link's grant covers the path and the
audience admits the link:

```rust
match (audience, kind) {
    (AudienceKind::Granted, _) => true,
    (AudienceKind::Peers(peers), LinkKind::Direct) => {
        peer.is_some_and(|peer| peers.contains(peer))
    }
    _ => false, // a Peers publication on a relay link
}
```

## The life of a session

Here is what happens when Bob's node dials Alice's node and the session runs
for a while. Alice runs `LiveLayout::policy()`, which lets every peer read
and publish under `live/<its id>/`.

```
 Bob                          Alice's node                       Alice's policy
  │                                │                                    │
  │── QUIC + TLS ─────────────────►│ remote_id() = bob (authenticated)  │
  │── MoQ SETUP (path, token?) ───►│ within HANDSHAKE_TIMEOUT           │
  │                                │── decide(PeerRequest, Connect) ───►│
  │                                │      within decide_timeout (10 s)  │
  │                                │◄── Ok(Authorization{grant, exp}) ──│
  │                                │ auth.authorize(view) before SETUP ok
  │◄── SETUP ok ───────────────────│                                    │
  │◄── ANNOUNCE live/alice/studio ─│ only paths in the view             │
  │── ANNOUNCE live/bob/cam ──────►│ in publish: enters the route table │
  │── ANNOUNCE live/carol/cam ────►│ outside publish: withheld          │
  │── SUBSCRIBE live/alice/call ──►│ not in the view                    │
  │◄── refused: Unauthorized ──────│                                    │
  │                                │                                    │
  │          Alice rings Bob: the audience of live/alice/call gains bob │
  │◄── ANNOUNCE live/alice/call ───│ re-authorized, no policy call      │
  │                                │                                    │
  │          the authorization expires                                  │
  │                                │ lapse: grant nothing until decided │
  │                                │── decide(PeerRequest, Reauthorize)►│
  │                                │◄── Ok(new) or Err(rejection) ──────│
  │◄── close(code) on a rejection ─│                                    │
```

The rest of this section walks through those steps.

### Accept or dial

On accept, the node waits up to `HANDSHAKE_TIMEOUT` for the peer's MoQ SETUP.
It turns the SETUP into a `SessionRequest` with the requested path and the
token, if the peer sent one. Then it asks the policy, with
`Event::Connect`.

On dial, the node asks its own policy *before* it opens the connection. The
request then holds the path this node is about to send, its `direction()` is
`Direction::Dialed`, and there is no token. A node can thus refuse to dial a peer, and the dial
fails with `Error::Rejected`. The policy also decides what the dialed peer
may read and publish on that session. Both directions get the same
treatment.

Each session is decided on its own. A peer with two sessions has two
authorizations, and a token counts only for the session that presented it.

### The decide timeout

The node gives the policy `NodeConfig::decide_timeout`, 10 seconds by
default, through `decide_within`. A first decision that takes longer
rejects the session with `moq_net::Error::Timeout`. The node drops the
policy's future at that point, so a policy must tolerate cancellation at any
await.

### The grant goes to moq-net

moq-net has its own authorization layer, an `auth::Handle` per session. The
node calls `Handle::authorize` with a moq grant before it completes the
handshake. So the very first announcements to the peer already keep to the
grant.

The node does not pass the policy's grant through as it is. It computes a
*view* of the link, `State::link_view`, and the view is what moq enforces:

- **subscribe:** the exact paths of this node's own publications that the
  grant covers and whose audience admits the link. Each path is a literal
  pattern, so nothing else matches.
- **publish:** the grant's publish patterns, widened by the peer's ticket
  claims (see [ownership](#ownership-and-impersonation)).

moq-net then enforces the view on the session, whether or not the peer's
protocol version speaks AUTH:

- Announcements to the peer cover only paths in the subscribe half.
- A subscribe request outside the subscribe half is refused.
- An announcement from the peer outside the publish half is held back from
  the route table.
- A narrower view ends what falls outside at once with `Unauthorized`.
  Announcements retract, subscriptions reset, and broadcasts the peer
  published abort. A wider view brings back what the old one held back.

moq bounds the view. It refuses an AUTH_OK larger than 64 KiB, and caps a
union of patterns at 1024 members. A link that may see more publications
than that cannot be granted them one by one.

### The effective grant

Three inputs make up what a session may do at any moment:

```
publish   = policy.publish ∪ claimed paths of the peer
subscribe = { path of each own publication
              | policy.subscribe covers path
              ∧ the audience admits this link }
```

Claims add only to the publish half. A lapsed or missing authorization makes
both halves empty. The node serves only its own publications, so the
subscribe half never lists a route it learned from another link.

The node recomputes the view of every link whenever one of its inputs
changes:

- a publication is added or removed,
- an audience changes, or its `Peers` set changes,
- a claim is added or released,
- a decision is applied, or an authorization lapses.

All of this runs synchronously under the node's state lock, and none of it
calls the policy. moq skips a grant equal to the last one, so a change that
leaves a link's view alone costs its session nothing. The cost is links times
publications per change, which is fine at tens of each.

### Re-decision

The node asks the policy again, with `Event::Reauthorize`, in two cases. The
first is when an authorization reaches its `expires`. The second is when the
application calls `Node::reauthorize(peer)` or `Node::reauthorize_all()`,
because state the policy reads has changed. The node cannot see that state,
so it has to be told.

At expiry, the session *lapses* first. It may do nothing until the new
decision arrives, so a slow policy cannot keep an expired token alive. A new
authorization replaces the old one. A rejection takes the rights at once and
closes the session with the rejection's code.

Two answers are not a rejection when the node asks again: `Timeout` and
`TooManyRequests`. They mean the policy could not decide now. The session
keeps its authorization, and the node retries with a backoff from 500 ms to
30 s. At most 16 re-decisions run at once across the node. One session never
has two running, and a trigger that arrives meanwhile queues one more.

`reauthorize` has one more effect that matters later. It bumps a node-wide
counter. Any session, of any peer, whose first decision was still running
when the counter moved registers with nothing and is decided again. That
way no session keeps rights from before the call.

## Ownership and impersonation

The publish half of a grant matters more than it looks. moq moves a reader
between routes that claim the same broadcast instance. A peer that may
announce another publisher's path could therefore take over that publisher's
readers. iroh-live prevents that with *layouts*: path conventions in which a
path names its publisher.

```rust
pub trait Layout<P: PeerId> {
    /// The paths `peer` may publish its own broadcasts under.
    fn scope(&self, peer: &P) -> Patterns;
    /// The peer that owns `path`, or `None` for a path outside the layout.
    fn owner(&self, path: &str) -> Option<P>;
}
```

`LiveLayout` puts a peer's broadcasts at `live/<peer id>/<name>`. Its
`scope` is `live/<peer id>/**`, and its `owner` parses the id back out of a
path. `RoomsLayout` does the same for `rooms/<topic>/<member id>/<name>`,
with the scope `rooms/*/<member id>/**`. A pair of layouts is their union.

A layout doubles as a policy. `LiveLayout::policy()` admits every peer with
`Grant::publish_under(layout.scope(peer))`. That grant lets the peer read
anything and publish only at paths that name it. Bob can announce
`live/<bob>/cam`, but an announcement of `live/<carol>/cam` from Bob never
enters Alice's route table. For a node that also runs rooms, use
`layout::policy((LiveLayout::default(), RoomsLayout))`.

Two more rules keep impersonation out:

- An instance id is a random string. It reaches only the links whose view
  lists the publication, so a peer cannot claim an instance it never saw.
- `Session::subscribe` and `Room::subscribe` read over the session with the
  named peer, and check that the peer's grant lets it publish the path.

### Ticket claims

A ticket names a publisher and a path, and any node should be able to play
it, whatever its policy. A node with the default `Open(Grant::subscribe_only())`
lets no peer publish anything, so the publisher's announcement would be
withheld.

`Node::subscribe_ticket` solves that with a *claim*. Before it subscribes,
the node calls `Node::claim_path(publisher, path)`. The claim adds exactly
that literal path to the publish half, on every session with the publisher,
for as long as the subscription holds it. When the last claim on a path
drops, the grant narrows again, and moq ends what local readers read there.

A claim must not let one peer stand in for another, so the publisher has to
own the path. With `NodeConfig::layout` set, the layout's `owner` decides.
Without it, exactly one segment of the path must be the publisher's endpoint
id in canonical hex. No other segment may parse as an endpoint id. A
node accepts at most 64 claimed paths, and 16 KiB of them, per peer.

## Tokens

The endpoint id says *who* a peer is. A token can say *what someone else
allowed it to do*. `Verify` turns a `TokenVerifier` into a policy:

```rust
pub trait TokenVerifier<P: PeerId>: fmt::Debug + Send + Sync + 'static {
    fn verify(&self, token: &str, request: &PeerRequest<P>) -> Decision;
}

let policy = Verify::new(verifier).with_fallback(Open(Grant::subscribe_only()));
```

With a token, the verifier decides, and a token it rejects is a rejection. It
never falls through to the fallback, so a forged token gains nothing.
Without a token, the fallback decides. The default fallback is `Closed`,
which makes the token required. One case does fall through: on a
re-decision, a token rejected with `Expired` hands the session to the
fallback. A session whose token expires thus narrows to the fallback's
rights instead of closing.

A node dials with no token, so the fallback decides every dial. Under
`Closed` the node can dial nobody.

Two verifiers ship behind features:

| | `JwtVerifier` (`jwt`) | `RcanVerifier` (`rcan`) |
|---|---|---|
| Token | a JWT, as moq-relay reads it | an `rcan1:` capability chain |
| Checked against | a moq-auth `KeySet` | a principal's ed25519 key and a root path |
| Grant | the claims' patterns, rooted at the requested path | the last link's `MoqCapability`, within the verifier's root |
| Expiry | the `exp` claim | one second after the earliest link's `valid_until` |
| Bound to the peer | no: whoever holds it may use it | yes: the last link must name the peer's endpoint id |

An rcan chain may have up to 8 links of at most 2 KiB each. Each link may
narrow the capability it delegates, never widen it.

### How a token travels

`ConnectOptions::token` sends the token twice. It goes in the MoQ SETUP as an
out-of-band token, and also as `?jwt=` in the setup path. The second copy is
there because moq-lite-06, the newest version iroh peers negotiate, has no
token field in its SETUP.
`SessionRequest::token` reads the SETUP token first, then the `jwt` query
parameter, then `token`. A SETUP token counts only if it is out-of-band and
valid UTF-8.

A token is a secret, so the code keeps it out of logs. The `Debug` output of
`SessionRequest` prints the query keys and `<redacted>` for the token, never a
value. The relay logs the request path without its query.

## Interactive admission with `Queue`

Some decisions belong to a person, such as "Bob wants to connect, allow?".
A `Queue` is a policy that puts each request into a bounded channel. The
application takes them with `next()` and answers each `Pending` with `admit`
or `reject`. Dropping a `Pending` rejects it.

A full queue rejects a new session at once with `TooManyRequests`, so a slow
application does not pile up waiting peers. A person takes longer than 10
seconds, so raise `decide_timeout` with a queue. On a re-decision, a full
queue or a timeout keeps the session's authorization, as described above.

## Where `Audience` comes in

Two library components use `Audience::Peers` today. Everything else in the
libraries publishes with `Granted`, including `LiveLayout::publish`.

### Calls

`Calls::new` publishes the node's call side at `live/<its id>/call` with
`Audience::Peers` over an empty set. Nobody sees it. `Calls::ring(peer)` adds
the peer to the set and dials it if no session exists:

```rust
let audience = Watchable::new(BTreeSet::new());
let (publication, side) =
    node.publish_with(path, Audience::Peers(audience.clone()), fill)?;

// ring:
let mut set = audience.get();
if set.insert(peer) {
    audience.set(set).ok();
}
```

The set change wakes a small task in the node, which recomputes every link's
view. The peer sees `live/<caller>/call` appear in its route table, and that
appearance *is* the ring. The peer answers by ringing back, and each side
subscribes to the other. A hang-up removes the peer from the set, and the
path disappears at once.

Both halves of access control are at work in a call. The caller's audience
decides who is offered the call side. The callee's policy decides whether
the caller's announcement enters its route table. That is why `Calls`
requires each node's policy to let the other publish at
`live/<peer id>/call`, as `LiveLayout::policy()` does.

### Rooms

A room is a gossip topic. Its members announce themselves, and the room
keeps the current member ids in a `Watchable<BTreeSet<EndpointId>>`.
`Room::publish` places a broadcast at `rooms/<topic>/<me>/<name>` with
`Audience::Peers(members.clone())`. Every room publication shares the one
watchable, so a member who joins sees all of them, and a member who leaves
loses all of them. The rooms crate never touches the node's policy.

A room ticket is a bearer secret. Anyone who holds it can join the gossip
topic, become a member, and so enter every member's audience.

### How it combines with the grant

The audience can only narrow. A peer in a `Peers` set whose grant does not
cover the path sees nothing. A `Granted` publication is exactly as visible as
the grants allow. So token-gated reading needs no audience at all: publish
with `Granted`, and let the policy's subscribe half decide.

### The relay rule

A `Peers` publication never goes to a relay link, even if the relay's id is
in the set. A relay forwards what it carries to anyone its own rules admit.
The publisher's set ends at the publisher's node, so offering the
publication to a relay would hand it to strangers.

A node's relay links do not go through the policy at all. Their grant is
`Grant::everything()`, and `LinkKind::Relay` is the only thing that keeps a
publication off them. A browser, which can only reach a node through a
relay, therefore never sees a `Peers` publication.

### Could a policy do this instead?

Both uses of `Peers` are membership lists. A policy can also express a
membership list: give the member a subscribe pattern for the path, and call
`Node::reauthorize` when the list changes. Here is the call side done that
way, next to today's version.

Today, with an audience:

```rust
// publish
node.publish_with(call_path, Audience::Peers(rung.clone()), fill)?;

// ring
let mut set = rung.get();
set.insert(peer);
rung.set(set).ok();
```

With a policy instead. This is a sketch, not code in the repository:

```rust
#[derive(Debug, Clone)]
struct CallPolicy {
    layout: LiveLayout,
    /// What every peer may read. Must not cover the call side.
    public: Patterns,
    call: Pattern, // Pattern::literal("live/<me>/call")
    rung: Watchable<BTreeSet<EndpointId>>,
}

impl Policy<EndpointId> for CallPolicy {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        let peer = *request.peer();
        let mut grant = Grant { subscribe: self.public.clone(), publish: self.layout.scope(&peer) };
        if self.rung.get().contains(&peer) {
            grant.subscribe.insert(self.call.clone());
        }
        Ok(grant.into())
    }
}

// publish
node.publish_with(call_path, Audience::Granted, fill)?;

// ring
let mut set = rung.get();
set.insert(peer);
rung.set(set).ok();
node.reauthorize(peer);
```

The two versions look alike, but they differ in four ways.

**Locality.** With an audience, the code that publishes owns who sees the
publication. `Calls` and `Room` set it up themselves and need nothing from
the application beyond a policy that admits the layout. A node has exactly
one policy, set when the node is built. In the policy version, the
application must compose the call rule, the room rule, and its own rules
into that one policy. Each library would have to export a piece of policy and
trust the application to wire it in. The policy also has to know every path
that peers may read, since patterns cannot say "everything except the call
side". The `public` field above carries that burden. In return, the policy
version keeps every access rule in one place. A reader of the code can then
answer "what may Bob read?" from one function.

**The relay rule.** Relay links bypass the policy, with a grant of
everything. In the policy version, the call side is `Granted`, so every
attached relay would receive it, and forward it to anyone. The policy alone
cannot keep a publication off relays. Some per-publication flag would still be
needed, such as a `Granted` that excludes relays. That is a smaller audience
type, but it is still an audience.

**Re-authorization cost.** An audience change recomputes views synchronously
under the state lock, and it never calls the policy. The peer sees the change
as soon as moq sends the new grant. A `reauthorize` runs an async decide for every
session with the peer, bounded by `decide_timeout` and 16 at once. That
cost reaches further than it looks:

- A hang-up takes effect only after the decision finishes, not at once.
- Every decide runs the whole policy again: a token verification, a database
  lookup, or a `Queue` prompt to a person.
- The node-wide counter means a ring also forces a second decision on every
  other peer whose first decision is in flight.
- A room needs one `reauthorize` per member who joins or leaves, instead of
  one set update.

**Granularity.** A policy decides per session, and an audience admits per
peer id. They agree for calls and rooms, since both identify members by
endpoint id. A policy can do things an audience cannot. It can gate on a
token, give the grant an expiry, and gate the publish half. A rule such as
"members of room T read `rooms/T/**`" fits a policy well. An audience cannot
widen anything.

In short, an audience is cheap, local and relay-safe, but it is a second
place where access rules live. A policy is the single place, but it pays an
async round trip per change. It also needs some other way to keep private
publications off relays. The plan in `plans/auth-admission.md` (section 6.6)
argues for keeping both. These are the facts to weigh if you revisit that.

## Relays and browsers

A relay appears in two roles.

**A node attached to a relay.** `Node::attach_relay` opens a relay link of
`LinkKind::Relay`. Its grant is `Grant::everything()`, not a policy decision.
The relay sees every `Granted` publication and no `Peers` one. The node
accepts the relay's announcements for any path, since a relay is trusted with
the paths it forwards. Attach only relays that keep each publisher to its own
paths. The node presents a token to the relay through `RelayConfig::token`,
a `TokenFn`. An `iroh://` link asks it for a fresh token on every dial.

**`iroh-live-relay` itself.** It has two doors.

- *iroh clients* come in through `IrohSessions`, which knows their endpoint
  id. It decides each session with a policy, by default `relay_policy()`:
  read the whole relay, and publish under `live/<id>/` and
  `rooms/*/<id>/`. With `--auth-key`, the policy becomes
  `Verify::new(JwtVerifier::new(keys)).with_fallback(relay_policy())`. The
  relay decides a client once. When the authorization expires, it closes the
  session, and the client's relay link redials with a fresh token.
- *Browsers* come in through moq-relay's server. Without auth flags, the relay
  answers each admission with `browser_grant(path)`: read anything, and
  publish a name of one segment below the dialed path. One segment keeps a
  browser out of `live/` and `rooms/`, whose paths name an iroh publisher.
  With `--auth-key`, a valid JWT grants what its claims say. With
  `--auth-url` or `--auth-public`, moq-relay's own auth decides.

When a session names a broadcast ticket, the relay *pulls* the broadcast. Its
pull node runs with `Open(Grant::nothing())`, so no publisher may announce
anything by default. Each pull holds a `PathClaim` for the ticket's path, and
the node's layout, `(LiveLayout, RoomsLayout)`, checks that the ticket's peer
owns it. A ticket for a path the peer does not own fails with `NotOwner`.

## What isn't there yet

**Transit.** No node forwards. Every link serves the node's own
publications only, never a route learned from another link. That is what
lets the publish half act as a naming rule, and what keeps a `Peers`
publication at the first hop. A forwarding node would need a separate rule
for which paths it may relay, and something to carry an audience beyond the
first hop. `plans/auth-admission.md` (section 7) sketches the options.
One is to forward only `Granted` publications, on links that declare themselves
forwarders. Others are rcan capabilities to serve a path to a set, and end-to-end
encryption.

**Revocation of rcan chains.** A chain stays valid until its earliest link
expires. Nothing revokes a link early. A policy can keep its own deny list
around `Verify`, with `.filter`, and call `Node::reauthorize` when the list
changes. Short-lived links bound the damage meanwhile.

## Cheat sheet

| I want to ... | Use |
|---|---|
| let every peer read, and nobody publish | the default, `Open(Grant::subscribe_only())` |
| let every peer read, and publish only under its own id | `LiveLayout::default().config()` |
| also allow room broadcasts | `layout::config((LiveLayout::default(), RoomsLayout))` |
| admit only known peers | `layout.policy().filter(..)`, plus `Node::reauthorize` when the set changes |
| ask a person before admitting | `Queue`, with a longer `decide_timeout` |
| require a token from moq-relay's issuer | `Verify::new(JwtVerifier::new(keys))` |
| delegate rights bound to a device key | `Verify::new(RcanVerifier::new(principal, root))` |
| let peers without a token in too | `.with_fallback(Open(..))` on the `Verify` |
| rights that end at a set time | return an `Authorization` with `expires` |
| show a broadcast to whoever the grant allows, relays included | `Audience::Granted` |
| show a broadcast to a few peers, and never to a relay | `Audience::Peers(watchable)` |
| change who sees a broadcast | update the `Peers` set, or `Publication::set_audience` |
| play a ticket under any policy | `Node::subscribe_ticket` |
| present a token when dialing | `ConnectOptions::token`, or `RelayConfig::token` for a relay |
