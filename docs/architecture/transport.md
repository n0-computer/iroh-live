# Transport

`iroh-live-node` is the node. It holds a node's route table, its publications,
its sessions with peers and its links to moq relays. It sits on `moq-transport-iroh`,
which turns an iroh connection into a moq-net session and knows nothing about
nodes, routes or publications.

For MoQ itself, read [the moq-lite layer
page](https://doc.moq.dev/concept/layer/moq-lite) and [the iroh transport
page](https://doc.moq.dev/concept/layer/iroh) upstream.

## Paths name the publisher

`iroh-live-node` has no path layout. `Node::publish` takes a full path, and the crates
above bring their own layout. `iroh-live` publishes at
`live/<endpoint id>/<name>`, and `iroh-live-rooms` at
`rooms/<topic>/<endpoint id>/<name>`. A broadcast has the same path over every
link, direct or through any number of relays. That lets the route table see
several routes to it, and lets a relay's token grant `live/<id>/**` to one
publisher. A `BroadcastTicket` names a publisher and a name, and
`ticket.path()` is the path.

## One route table, fed by every link

Every link announces straight into the node's route table. A session's
subscriber is the table tagged `Link::Peer` and scoped to the paths its grant
lets the peer publish. A relay's is the table tagged `Link::Upstream`. Every
link serves the table's local view (`origin::Consumer::local`), which hides
routes from both tags, so a route learned on one link is never offered on
another: the node carries no traffic between its links. The peer's hop and the
link's cost travel with each route (`route.rs`). moq serves the cheapest route.
When it dies, moq moves to another route of the same instance, which is the
stream the publisher announced. A move between a direct and a relay route of
one instance does not end the broadcast. When the instance's last route goes,
the broadcast ends, and `Subscription::closed` asks the table again.

The node can still say which link serves a path (`Subscription::session`,
`Subscription::serving_link`), from the hop of the route that serves it. `None` means
no link: the path is the node's own publication, or no route is announced yet.
A relay that declares a hop of its own is matched to the relay link only while
exactly one relay link feeds the table, and to no link otherwise.
`Session::subscribe` resolves a path over one session only, and follows only
the instance that session announced. Rooms read each member this way. Both
rest on hops, which a peer may declare, so they are only as strong as grants
that keep each peer to its own paths.

A direct session's routes enter the table only within the grant. The node's
policy decides it for each peer, and the default policy grants
`Grant::subscribe_only()`: a peer may read anything and publish nothing.
`LiveLayout::config()` lets a peer publish under `live/<peer>/`, and
`iroh_live::layout::config((LiveLayout::default(), RoomsLayout))` adds
`rooms/*/<peer>/`. The table is shared by
everything on the node and answers a ticket without dialing. A peer whose grant
let it announce another publisher's path could stand in for that publisher,
so grants should name the peer. A relay is trusted with every path it forwards,
since forwarding other publishers' broadcasts is its job. Attach only relays
that keep each publisher to its own paths.

`iroh-live-node/tests/origin.rs` tests the moq behaviour this relies on.

`Node::subscribe(path, reach)` resolves a path in the table. If there is no
route yet, it reaches out as `Reach` says: dial the publisher
(`Reach::Direct(id)`), wait for a relay (`Reach::Relays`), or both
(`Reach::Both(id)`). The node's hop id is derived from its endpoint id, so a
relay recognizes its routes after a restart.

## Publications and audiences

`Node::publish(path, audience)` creates a broadcast at `path` in the route
table, and returns a `Publication` whose `broadcast()` is the
`broadcast::Producer` to fill. moq-net creates every broadcast inside an
origin, and the route table is the only one: peers read the broadcast itself,
with nothing copied in between. `Node::publish_with` fills the broadcast before
it is announced, so no reader finds it without its first tracks. The fill
runs before the node takes its lock, so it may fail or call back into the
node without harm.
`LiveLayout::publish` builds a `LocalBroadcast` in it this way. A
publication stays until `unpublish`, until its broadcast closes, or until the
node shuts down. Dropping its handles does not withdraw it, so a publication
that belongs to a task goes through `Publication::unpublish_on_drop`, whose
guard unpublishes it when the task ends or is aborted.

Who sees a publication is moq's per-session authorization
(`moq_net::auth::Handle::authorize`), which every session enforces locally, on
every protocol version, as client and as server. The node authorizes each
session before its handshake, so its first announcements already keep to it.
The grant's subscribe half lists the exact paths of the publications whose
audience admits the peer, within the session's `Grant`. The audience is
`Granted` or a watched set of `Peers`. A relay link sees every `Granted`
publication and never a `Peers` one, which a relay would forward to anyone. `Grant`, `SessionRequest`, `Audience` and the
rule that turns publications into a link's visible paths (`visible_paths`) live
in `moq-ext-access`, which has no iroh dependency, and `iroh-live-node` re-exports
them for endpoint ids.

The node works the grants out again, and re-authorizes the sessions whose view
changed, when a publication is added or removed, when an audience or a `Peers`
set changes (`Publication::set_audience`), when the policy decides a peer
again (`Node::reauthorize`), and when a session or relay link is added. A narrower
grant retracts the path from the peer and resets what it reads there with
`Unauthorized`. A wider one announces the path again. The rest of the session
carries on.

The route table also holds every publication of the node whatever its
audience, so `Node::subscribe` resolves a node's own private publication
locally. Audiences bound peers, not the node.

## Sessions and admission

`Node::connect(peer)` dials a peer and returns a `Session`. If a session with the
peer exists, it returns that one, and concurrent dials share one. Two peers
that dial each other at the same moment keep both sessions.

`Node::mount` adds the node to the application's `RouterBuilder` under every
ALPN in `alpns()`, and incoming connections arrive through its
`ProtocolHandler`. A node that is never mounted refuses every incoming
connection. After the peer's setup, the node asks its policy
(`NodeConfig::policy`) whether to admit the session, and with which
authorization. It asks the same before it dials a peer, and a rejection then
fails the dial with `Error::Rejected`. Without a policy, every session is
admitted with `Grant::subscribe_only()`. Each decision may take up to
`NodeConfig::decide_timeout`. `Node::sessions()` watches the open sessions.

The node keeps one authorization per session on its link (`state.rs`). The
effective grant of a session is that authorization's grant plus the peer's
ticket claims. A deadline per session (`reauth.rs`) fires at
the authorization's `expires` time, and the actor then asks the policy again.
So does `Node::reauthorize`, for each session with the peer. A new decision
replaces the session's authorization and re-authorizes it, and a rejection
closes it. One decision per session runs at a time, and at most 16 run at
once. A trigger that arrives while a decision runs makes the actor ask once
more afterwards, so no change is lost. A session whose first decision started
before a `reauthorize` call registers with nothing and is decided again: the
state counts the calls, and the session compares the count from before its
decision. [Access control](../guide/access.md) describes the policies
from the application's side.

Each session runs a connection monitor (`link.rs`) that reads moq-net's session
statistics every 200 ms into the adaptive crate's `NetworkSample`, and
`Session::moq()` exposes the moq-net session it reads. The sample holds the
round trip, the minimum round trip over 15 s, the loss rate and throughput over
2 s, the peer's delivery estimate, whether the path is relayed and a path
generation. Every figure is an `Option` and reads `None` until measured. A
switch to another QUIC path bumps `path_generation` and starts the history
over. `Subscription::serving_link()` returns the `ServingLink` (`id`, `kind`,
`sample`) of whichever link serves the subscription. `iroh-live/src/network.rs`
hands the sample to the player and makes its path generation count changes of
the serving link too. The monitor logs every sample at TRACE
as `link sample`. The node keeps no list of paths of its own:
`Session::connection()` is the iroh connection, and its `paths()` and
`paths_stream()` give each path's remote address, selection and RTT.

## Relay links

`Node::attach_relay(RelayConfig::new(url))` attaches to a moq relay at an
`iroh://` or `https://` URL. `https://` links use moq-tokio's client, and
`iroh://` links dial through the node's own endpoint, since moq-tokio has no
iroh transport. The link redials with
backoff, and `RelayLink::status()` watches its `RelayStatus`. Publications with
the `Granted` audience go to the relay. `RelayConfig::token` is a `TokenFn`
that the link asks for a token: an `iroh://` link before every dial, and an
`https://` link once, when it attaches. The relay's routes join the route table
at a cost of 10 against a direct link's cost of one, so a direct route wins
while it exists.

`RelayConfig::consume` defaults to true, which feeds every route the relay
knows into the node's route table. A node that only publishes through the
relay sets `consume: false`.

`NodeConfig::table` hands the node an existing route table instead of a new
one. `iroh-live-relay` gives its pull node the cluster origin this way, so a
pulled broadcast lands in the cluster as the publisher's session announces it.
The node still serves only the table's local view within each session's grant,
so its peers never see the rest of the cluster.

A relay link runs the same monitor over its current MoQ session, and
`Subscription::serving_link()` returns its sample while the relay serves the
subscription. `relayed` is always false there, and each reconnect starts a new
path generation.

## ALPN negotiation

`moq_transport_iroh::ALPN` is `moq_net::ALPNS[0]`, the newest MoQ version this build
speaks. `moq_transport_iroh::alpns()` returns all of `moq_net::ALPNS`, newest
first. Mount the node under all of them. A dial offers only the newest: iroh's
`Router` sorts the ALPNs it accepts and TLS picks the server's first match, so
a dial that offered the older ones too would settle on moq-lite-03. An ALPN this build does not speak fails with
`moq_transport_iroh::Error::UnsupportedAlpn`, which the node wraps as
`Error::Transport`.

MoQ runs on raw QUIC. Raw QUIC has no request URI, so the request path and the
token travel in the MoQ SETUP. The dialing node sets the token with moq-net's
`Client::with_token`. The SETUP of moq-lite-06, the version iroh peers
negotiate today, has no token field, so the token also rides the setup path as
`?jwt=`. The accepting node reads `Handshake::token` and falls back to that
query parameter, and `SessionRequest::token` returns the result.

An application that runs moq-net's client or server itself gets the same
negotiation from `moq_transport_iroh::Transport`: `Transport::connect` dials a
peer, and `Transport::new` wraps a connection that is already open, accepted or
dialed with iroh's connect options. `iroh-live-relay` accepts its iroh sessions
through `Transport::new`.

## The poll transport

moq-net runs sessions over a poll-based transport interface
(`moq_net::transport::poll`). `moq_transport_iroh::Transport` implements it on
iroh's connection and streams directly. Stream reads and writes poll iroh's
streams. Accepts, opens, datagram reads and the connection close are iroh
futures that borrow the connection, so the transport stores one per operation,
built on a clone of the connection and shared by every task that polls it.

iroh has no send groups, so streams order by their own priority, saturated to
iroh's 32 bits. The wire counters come from the connection's UDP statistics.
The transport reports no probe target and no receive window: iroh's congestion
controller takes no target, and iroh does not report the windows in force. A
send stream dropped before it is finished is reset with code 0, so the peer
never reads a truncated stream as complete.

## Errors

`iroh_live_node::Error` is the crate's one error type. It covers dial and
handshake failures, refused sessions, a path that was never announced or cannot
be resolved, a publication that already exists, a grant that does not cover a
path, and shutdown. moq-net's own errors stay typed: `Error::Moq` holds a
`moq_net::Error` when moq-net refuses an operation, such as a path that does
not parse.
