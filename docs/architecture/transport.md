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
lets the peer publish. A relay's is the table tagged `Link::Upstream`, so
routes from one upstream are never offered to another. The peer's hop and the
link's cost travel with each route (`route.rs`). moq serves the cheapest route.
When it dies, moq moves to another route of the same instance, which is the
stream the publisher announced. A move between a direct and a relay route of
one instance does not end the broadcast. When the instance's last route goes,
the broadcast ends, and `Subscription::closed` asks the table again.

The node can still say which link serves a path (`Subscription::session`,
`Subscription::link`). `Session::subscribe` resolves a path over one session
only, and follows only the instance that session announced. Rooms read each
member this way.

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

`Moq::publish(path, broadcast, audience)` places an existing broadcast at a
path. The transport does not create the broadcast: it publishes anything that
implements `Consume<broadcast::Consumer>`, so one broadcast can be published at
several paths at once.

Every session has a publish origin of its own, and the publication is copied
into it with `moq-ext-forward`, which reads a track from the broadcast only
when the peer asks for it. So one broadcast can be offered to each peer
without showing every peer the same set. A publication is offered on a
session when its audience admits the peer and the session's grant covers its
path. The audience is `Everyone` or a watched set of `Peers`. `Grant`, `SessionRequest`,
`Audience` and the rule for what a link sees live in `moq-ext-access`, which
has no iroh dependency, and `iroh-moq` re-exports them for endpoint ids. Offers change
while the session runs, as audiences change.

Withdrawing an offer retracts the path: the peer can no longer resolve it, and
the broadcast it resolved closes. Tracks it already reads run on, since in
moq-lite a retraction does not disturb subscriptions in flight. To cut a peer
off at once, close its session. An offer is withdrawn when a `Peers` set
shrinks, `unpublish` is called, or the session ends.

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
resolved, a publication that already exists, a grant that does not cover an
offer, and shutdown.
