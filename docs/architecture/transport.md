# Transport

`iroh-moq` is the transport. It holds a node's route table, its publications,
its sessions with peers and its links to moq relays. It is the only crate in
the workspace that knows about both iroh and moq-net.

For MoQ itself, read [the moq-lite layer
page](https://doc.moq.dev/concept/layer/moq-lite) and [the iroh transport
page](https://doc.moq.dev/concept/layer/iroh) upstream.

## Paths name the publisher

`iroh-moq` has no path layout. `Moq::publish` takes a full path, and the crates
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
`Subscription::link`), from the hop of the route that serves it. `None` means
no link: the path is the node's own publication, or no route is announced yet.
A relay that declares a hop of its own is matched to the oldest relay link.
`Session::subscribe` resolves a path over one session only, and follows only
the instance that session announced. Rooms read each member this way. Both
rest on hops, which a peer may declare, so they are only as strong as grants
that keep each peer to its own paths.

A direct session's routes enter the table only within the grant. `MoqConfig::grant`
computes it from the peer's endpoint id, and the default is
`Grant::subscribe_only()`: a peer may read anything and publish nothing.
`iroh_live::grant` lets a peer publish under `live/<peer>/`, and
`LiveBuilder::with_rooms` adds `rooms/*/<peer>/`. The table is shared by
everything on the node and answers a ticket without dialing. A peer whose grant
let it announce another publisher's path could stand in for that publisher,
so grants should name the peer. A relay is trusted with every path it forwards,
since forwarding other publishers' broadcasts is its job. Attach only relays
that keep each publisher to its own paths.

`iroh-moq/tests/origin.rs` tests the moq behaviour this relies on.

`Moq::subscribe(path, reach)` resolves a path in the table. If there is no
route yet, it reaches out as `Reach` says: dial the publisher
(`Reach::Direct(id)`), wait for a relay (`Reach::Relays`), or both
(`Reach::Both(id)`). The node's hop id is derived from its endpoint id, so a
relay recognizes its routes after a restart.

## Publications and audiences

`Moq::publish(path, audience)` creates a broadcast at `path` in the route
table, and returns a `Publication` whose `broadcast()` is the
`broadcast::Producer` to fill. moq-net creates every broadcast inside an
origin, and the route table is the only one: peers read the broadcast itself,
with nothing copied in between. `Moq::publish_with` fills the broadcast before
it is announced, so no reader finds it without its first tracks.
`iroh_live::Live::publish` builds a `LocalBroadcast` in it this way. A
publication stays until `unpublish`, until its broadcast closes, or until the
node shuts down.

Who sees a publication is moq's per-session authorization
(`moq_net::auth::Handle::authorize`), which every session enforces locally, on
every protocol version, as client and as server. The node authorizes each
session before its handshake, so its first announcements already keep to it.
The grant's subscribe half lists the exact paths of the publications whose
audience admits the peer, within the session's `Grant`. The audience is
`Everyone` or a watched set of `Peers`. A relay link sees `Everyone`
publications when its `RelayOffer` is public, and never a `Peers` one, which a
relay would forward to anyone. `Grant`, `SessionRequest`, `Audience` and the
rule that turns publications into a link's visible paths (`visible_paths`) live
in `moq-ext-access`, which has no iroh dependency, and `iroh-moq` re-exports
them for endpoint ids.

The node works the grants out again, and re-authorizes the sessions whose view
changed, when a publication is added or removed, when an audience or a `Peers`
set changes (`Publication::set_audience`), when a session's grant changes
(`Session::set_grant`), and when a session or relay link is added. A narrower
grant retracts the path from the peer and resets what it reads there with
`Unauthorized`. A wider one announces the path again. The rest of the session
carries on.

The route table also holds every publication of the node whatever its
audience, so `Moq::subscribe` resolves a node's own private publication
locally. Audiences bound peers, not the node.

## Sessions and admission

`Moq::connect(peer)` dials a peer and returns a `Session`. If a session with the
peer exists, it returns that one, and concurrent dials share one. Two peers
that dial each other at the same moment keep both sessions.

`Moq` implements iroh's `ProtocolHandler`, so incoming connections arrive
through it. Under `Admission::Open` (the default) every session is admitted
with the grant `MoqConfig::grant` gives the peer, or `Grant::subscribe_only()` if
none is set. Under `Admission::Manual` the handshake waits after the peer's
setup, and the session shows up in `Moq::accept()`. The application reads the
`SessionRequest` (its path, a `jwt` query parameter, an HTTP/3 header) and
admits it with a `Grant` or rejects it. `Moq::sessions()` watches the open
sessions.

Each session runs a connection monitor (`link.rs`) that reads moq-net's session
statistics every 200 ms. `Session::link()` returns the latest `LinkSample`:
round trip, minimum round trip over 15 s, loss rate and goodput over 2 s, the
peer's delivery estimate, a path generation and whether the path is relayed.
Every figure is an `Option` and reads `None` until measured. A switch to
another QUIC path bumps `path_generation` and starts the history over.
`Subscription::link()` returns the `ServingLink` (`id`, `kind`, `sample`) of
whichever link serves the subscription. `iroh-live/src/network.rs` turns that
into the media crate's `NetworkSample`. The monitor logs every sample at TRACE
as `link sample`.

## Relay links

`Moq::attach_relay(RelayConfig::new(url))` attaches to a moq relay at an
`iroh://` or `https://` URL. `https://` links use moq-tokio's client, and
`iroh://` links dial through the node's own endpoint, since moq-tokio has no
iroh transport. The link redials with
backoff, and `RelayLink::status()` watches its `RelayStatus`. Publications with
the `Everyone` audience go to the relay by default (`RelayOffer::Public`). The
relay's routes join the route table at a cost of 10 (`DEFAULT_RELAY_COST`)
against a direct link's cost of one, so a direct route wins while it exists.

`RelayConfig::consume` defaults to true, which feeds every route the relay
knows into the node's route table. A node that only publishes through the
relay sets `consume: false`.

`MoqConfig::table` hands the node an existing route table instead of a new
one. `iroh-live-relay` gives its pull node the cluster origin this way, so a
pulled broadcast lands in the cluster as the publisher's session announces it.
The node still serves only the table's local view within each session's grant,
so its peers never see the rest of the cluster.

A relay link runs the same monitor over its current MoQ session, and
`RelayLink::link()` returns the sample. `relayed` is always false there, and
each reconnect starts a new path generation.

## ALPN negotiation

`iroh_moq::ALPN` is `moq_net::ALPNS[0]`, the newest MoQ version this build
speaks. `iroh_moq::alpns()` returns all of `moq_net::ALPNS`, newest first, with
HTTP/3 last. Mount the node under all of them. A dial offers the newest and
lists the rest as additional ALPNs.

Both sides branch on the negotiated ALPN. Raw QUIC carries the MoQ session
directly. HTTP/3 answers a CONNECT first: the client sends a
`web_transport_proto::ConnectRequest` that lists every moq-lite ALPN as a
protocol, and the server replies `ConnectResponse::OK` with the first one. An
ALPN this build does not speak fails with `Error::UnsupportedAlpn`.

An application that runs moq-net's client or server itself gets the same
negotiation from `iroh_moq::transport::{dial, accept}`, which return a
`web_transport_iroh::Session`. `iroh-live-relay` accepts its iroh sessions
through `accept`.

## The poll adapter

moq-net runs sessions over a poll-based transport interface, and
web-transport-iroh is async. `iroh-moq/src/adapter.rs` is moq-tokio's adapter,
vendored because moq-tokio's version takes only its own QUIC backends. It goes
away once web-transport-iroh implements the poll interface itself.

## Errors

`iroh_moq::Error` is the crate's one error type. It covers dial and handshake
failures, refused sessions, a path that was never announced or cannot be
resolved, a publication that already exists, a grant that does not cover a
path, and shutdown.
