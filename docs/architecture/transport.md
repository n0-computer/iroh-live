# The node and its transport

`iroh-live-node` is the node. It holds a node's route table, its
publications, its sessions with peers and its links to MoQ relays. It sits on
`moq-transport-iroh`, which turns an iroh connection into a moq-net session.
That crate knows nothing about nodes, routes or publications.

For MoQ itself, read [the moq-lite layer
page](https://doc.moq.dev/concept/layer/moq-lite) and [the iroh transport
page](https://doc.moq.dev/concept/layer/iroh) upstream.

## Paths

`iroh-live-node` has no path layout of its own. `Node::publish` takes a full
path, and the crates above bring their layouts, as
[Paths, layouts and tickets](../concepts/paths.md) describes. A broadcast has
the same path over every link, which lets the route table hold several routes
to it.

## One route table, fed by every link

Every link announces straight into the node's route table. A route records the
link it came from, the peer's *hop* and the link's *cost*. A hop is the id a
node declares for itself in its announcements. The node derives its own from
its endpoint id, so a relay recognizes its routes after a restart. A direct
session costs 1 and a relay link 10, and moq serves the cheapest route.

Each link serves only the table's *local view*: the node's own publications,
without the routes it learned from its links. So a route learned on one link
is never offered on another, and the node carries no traffic between its
links.

An *instance* is one announcement of a broadcast by its publisher. It can
reach the node over several routes, such as a direct session and a relay
link. When the serving route dies, moq moves to another route of the same
instance, and the broadcast does not end. When another instance takes the
path over, moq hands it out and the subscription moves to it.

When the instance's last route goes, the broadcast ends. The subscription
waits 100 ms, so that a successor can announce, such as a second session with
the publisher. Then it asks the table again, and ends if no route serves the
path. The player waits longer, see [Subscribing](subscribe.md).

The node can say which link serves a path (`Subscription::session`,
`Subscription::serving_link`), from the hop of the serving route. `None`
means no link: the path is the node's own publication, or no route is
announced yet. A relay that declares a hop of its own is matched to the relay
link only while exactly one relay link feeds the table.

`Session::subscribe` resolves a path over one session only, and follows only
the instance that session announced. Rooms read each member this way. Both
rest on hops, which a peer declares itself. So they are only as strong as
grants that keep each peer to its own paths.

## What a peer may announce

A direct session's routes enter the table only within the session's grant.
The node's policy decides the grant for each peer. The default policy grants
`Grant::subscribe_only()`: a peer may read anything and publish nothing.
`LiveLayout::config()` lets a peer publish under `live/<peer>/`, and the
rooms layout adds `rooms/*/<peer>/`.

The table is shared by everything on the node, and answers a ticket without
dialing. A peer whose grant let it announce another publisher's path could
stand in for that publisher, so grants should name the peer. A relay is
trusted with every path it forwards, since forwarding other publishers'
broadcasts is its job. Attach only relays that keep each publisher to its
own paths.

`Node::subscribe(path, reach)` resolves a path in the table. If there is no
route yet, it reaches out as `Reach` says: dial the publisher, wait for a
relay, or both.

## Publications and audiences

`Node::publish(path, audience)` creates a broadcast at `path` in the route
table. It returns a `Publication`, whose `broadcast()` is the producer to
fill. moq-net creates every broadcast inside an origin, and the route table is
the only one. So peers read the broadcast itself, with nothing copied in
between.

`Node::publish_with` fills the broadcast before it is announced, so no reader
finds it without its first tracks. The fill runs before the node takes its
lock, so it may fail or call back into the node without harm. Two callers
use it:

- `LiveLayout::publish` builds a `LocalBroadcast` in it.
- `Node::publish_value` publishes one latest-value JSON track through a
  `ValueWriter`, which stamps each update. `Subscription::value_track` reads
  such a track on the other side with a `ValueReader`, which reopens the
  track on each broadcast the subscription follows and ends only when the
  subscription closes.

A publication stays until `unpublish`, until its broadcast closes, or until
the node shuts down. Dropping its handles does not withdraw it. A publication
that belongs to a task goes through `Publication::unpublish_on_drop`, whose
guard unpublishes it when the task ends or is aborted.

moq enforces who sees a publication, through its per-session authorization.
Every session enforces it locally, on every protocol version, as client and
as server. The node authorizes each session before its handshake, so its
first announcements already keep to it.

The grant's subscribe half lists the exact paths of the publications whose
audience admits the peer, within the session's `Grant`. The audience is
`Granted` or a watched set of `Peers`. A relay link sees every `Granted`
publication and never a `Peers` one, which a relay would forward to anyone.

The node works the grants out again on these events: a publication is added
or removed, an audience changes, the policy decides a peer again, or a
session or relay link is added. It re-authorizes each session whose visible
paths changed. A narrower grant retracts the path from the peer, and resets
what the peer reads there with `Unauthorized`. A wider one announces the path
again. The rest of the session carries on.

The route table also holds every publication of the node, whatever its
audience. So `Node::subscribe` resolves a node's own private publication
locally. Audiences bound peers, not the node.

## Sessions and admission

`Node::connect(peer)` dials a peer and returns a `Session`. If a session with
the peer exists, it returns that one, and concurrent dials share one. Two
peers that dial each other at the same moment keep both sessions.

`Node::mount` adds the node to the application's `RouterBuilder` under every
ALPN in `alpns()`. A node that is never mounted refuses every incoming
connection. `Node::sessions()` watches the open sessions, and
`Node::accepted()` yields each accepted one once.

After the peer's setup, the node asks its policy whether to admit the
session, and with which authorization. It asks the same before it dials a
peer, and a rejection then fails the dial with `Error::Rejected`. Without a
policy, every session is admitted with `Grant::subscribe_only()`. Each
decision may take up to `NodeConfig::decide_timeout`.

The node keeps one authorization per session. The session's effective grant
is that authorization's grant plus the peer's ticket claims. The node asks
the policy again when the authorization expires, and when the application
calls `Node::reauthorize`. A new decision replaces the authorization, and a
rejection closes the session.

One decision per session runs at a time, and at most 16 run at once. A
trigger that arrives during a decision makes the node ask once more
afterwards, so no change is lost. A `reauthorize` call also re-decides every
session whose first decision was still running, so no session keeps rights
from before the call. [Control access](../how-to/control-access.md) describes
the policies from the application's side.

## Link measurements

Each session runs a connection monitor. Every 200 ms, it reads moq-net's
session statistics into a `NetworkSample`, the input of the adaptive crate.
The sample holds these figures, each an `Option` that reads `None` until
measured:

- the round trip, and the minimum round trip over 15 s;
- the loss rate, throughput and send rate over 2 s;
- the peer's delivery estimate;
- whether the path is relayed;
- a path generation, which a switch to another QUIC path bumps, starting the
  history over.

`Subscription::serving_link()` returns the `ServingLink` (`id`, `kind`,
`sample`) of whichever link serves the subscription, and
`Subscription::session()` the direct session that serves it.
`Session::sample()` returns the latest sample of a session the application
holds, and `Session::moq()` the moq-net session the monitor reads. The facade hands the
sample to the player, and counts a change of serving link as a new path
generation too. The monitor logs every sample at TRACE as `link sample`.

The node keeps no list of network paths of its own. `Session::connection()`
is the iroh connection, and its `paths()` and `paths_stream()` give each
path's remote address, selection and round trip.

## Relay links

`Node::attach_relay(RelayConfig::new(url))` attaches to a MoQ relay at an
`iroh://` or `https://` URL. `https://` links use moq-tokio's client.
`iroh://` links dial through the node's own endpoint, since moq-tokio has no
iroh transport.

Both run moq-net's connect policy, `moq_net::Reconnect`. It redials with
jittered backoff, and on a GOAWAY it dials the replacement while the old
session drains. The policy gives up when its first round of dials reaches no
relay, or when the relay refuses tokens that have not changed since. An
`iroh://` link then starts it again after a pause that doubles from 1 s to
30 s. So the link stays attached to a relay that is down when it attaches.
`RelayLink::status()` watches its `RelayStatus`.

Publications with the `Granted` audience go to the relay.
`RelayConfig::consume` defaults to true, which feeds every route the relay
knows into the node's route table. A node that only publishes through the
relay sets `RelayConfig::with_consume(false)`.

`RelayConfig::tokens` are presented on every dial: the first in the MoQ SETUP,
and also as `?jwt=` in the URL, since moq-lite-06 has no token field in its
SETUP. A `jwt` already in the URL wins. `RelayLink::tokens()` returns the
link's tokens, and `Tokens::set` replaces them for the live session and every
later dial. A relay that refused the old tokens is dialed again at once.

`NodeConfig::table` hands the node an existing route table instead of a new
one. `iroh-live-relay` gives its pull node the cluster origin this way, so a
pulled broadcast lands in the cluster when the publisher's session announces
it. The node still serves only the table's local view within each session's
grant, so its peers never see the rest of the cluster.

A relay link runs the same monitor over its current MoQ session.
`Subscription::serving_link()` returns its sample while the relay serves the
subscription. `relayed` is always false there, and each reconnect starts a new
path generation.

## ALPN negotiation

`moq_transport_iroh::ALPN` is the newest MoQ version this build speaks, and
`moq_transport_iroh::alpns()` returns all of them, newest first. Mount the
node under all of them. A dial offers only the newest. iroh's `Router` sorts
the ALPNs it accepts and TLS picks the server's first match, so a dial that
offered the older ones too would settle on moq-lite-03. An ALPN this build
does not speak fails with `UnsupportedAlpn`, which the node wraps as
`Error::Transport`.

MoQ runs on raw QUIC, which has no request URI. So the request path and the
token travel in the MoQ SETUP. The dialing node sets the token with moq-net's
`Client::with_token`. The SETUP of moq-lite-06, the version iroh peers
negotiate today, has no token field, so the token also rides the setup path
as `?jwt=`. The accepting node reads the SETUP's token and falls back to that
query parameter. `SessionRequest::token` returns the result.

An application that runs moq-net's client or server itself gets the same
negotiation from `moq_transport_iroh::Transport`. `Transport::connect` dials a
peer, and `Transport::new` wraps a connection that is already open.
`iroh-live-relay` accepts its iroh sessions through `Transport::new`.

## The poll transport

moq-net runs sessions over a poll-based transport interface.
`moq_transport_iroh::Transport` implements it on iroh's connection and
streams directly. Stream reads and writes poll iroh's streams. Accepts, opens,
datagram reads and the connection close are iroh futures that borrow the
connection. So the transport stores one future per operation, built on a
clone of the connection and shared by every task that polls it.

Some details of the interface:

- The wire counters come from the connection's UDP statistics.
- The transport reports no probe target and no receive window. iroh's
  congestion controller takes no target, and iroh does not report the
  windows in force.
- It exports keying material from the connection's TLS secrets, the same
  bytes at both ends and different bytes for every other session.
- A send stream dropped before it is finished is reset with code 0, so the
  peer never reads a truncated stream as complete.

### Send groups

moq-net opens one *send group* per subscription. A higher-priority group
preempts every lower one, and groups of equal priority share the bytes.
Inside a group, streams go in their own order, the newest group of a track
first. The control streams belong to no group and go before all of them.

Without send groups, moq-net ranks every group stream of the session in one
queue, and the older of two equal-priority subscriptions goes first. Under a
held bandwidth cap that starved a rendition switch. The replacement's first
group waited behind the old rendition's backlog, which never drained.

iroh schedules a stream by an `i32` urgency, higher first. Streams of one
urgency are sent round robin while the connection's send fairness is on,
which is the default of noq, the QUIC implementation inside iroh.
`moq_transport_iroh::SendGroup` packs moq-net's two levels into the urgency:

| Stream | Urgency |
|---|---|
| ungrouped, at priority `p` | `2^29 + p`, above every group |
| grouped, in a group at priority `g`, at tier `t` | `16 * g + (15 - t)` |

A stream's *tier* is its rank by order within its group. The streams with the
group's highest order are tier 0, the next distinct order tier 1, up to tier
15, which the rest share. So the heads of equal-priority groups share one
urgency and split the bytes, and a group's older streams wait for its head.
A higher-priority group outranks every tier of a lower one. A stream joining,
leaving or moving reranks its group. Datagrams ignore their group, since iroh
queues them apart from streams.

The mapping has these limits:

- A new urgency reaches data a stream already queued only at that stream's
  next turn, since noq 1.4 keeps a queued stream at its old urgency. A
  lowered urgency applies after one more turn. A raised one may wait as long
  as anything above the old urgency has data queued, as on a link held at
  its cap. A rendition switch is not affected, since it opens new streams,
  which are queued at their urgency from the start. A fix needs a change in
  noq.
- moq-quic, the QUIC stack behind moq-tokio, schedules groups with deficit
  round robin, which the mapping only approximates. Fairness is per stream
  and per turn, not per group and per byte, and only between streams of one
  tier. When a group's head has nothing queued, its next stream waits behind
  the other groups' heads. For live media that holds back a group's backlog
  first, which the newest-first order asks for anyway.
- The sharing rests on send fairness. An application that sets
  `send_fairness(false)` in iroh's transport config gets streams of one
  urgency in write order, and the older of two equal-priority groups goes
  first again. RFC 9218's per-stream incremental flag, which
  [n0-computer/noq#859](https://github.com/n0-computer/noq/pull/859) adds to
  noq, would let each group stream ask for round robin whatever that setting.

## Errors

`iroh_live_node::Error` is the crate's one error type. It covers dial and
handshake failures, refused sessions, and a path that was never announced or
cannot be resolved. It also covers a publication that already exists, a grant
that does not cover a path, and shutdown. moq-net's own errors stay typed:
`Error::Moq` holds a `moq_net::Error` when moq-net refuses an operation, such
as a path that does not parse.

## Where in the code

- `iroh-live-node/src/route.rs`: the route table, hops and costs, and the
  100 ms re-ask (`REASK_PAUSE`).
- `iroh-live-node/src/state.rs`: one authorization per session, and the
  `reauthorize` counter.
- `iroh-live-node/src/reauth.rs`: the deadline that re-decides a session when
  its authorization expires.
- `iroh-live-node/src/link.rs`: the connection monitor and its 200 ms
  samples.
- `iroh-live-node/src/relay.rs`: the `iroh://` relay link and its restart
  pause.
- `iroh-live-node/tests/origin.rs`: tests of the moq behavior the route table
  relies on.
- `moq-ext-access`: `Grant`, `SessionRequest`, `Audience` and
  `visible_paths`, the rule that turns publications into a link's visible
  paths. `iroh-live-node` re-exports them for endpoint ids.
- `iroh-live/src/network.rs`: hands the serving link's sample to the player.
- `moq-transport-iroh/src/`: ALPNs, the poll transport and `SendGroup`.
