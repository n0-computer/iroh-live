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

Every link writes what its peer announces into an ingest origin of its own. A
bridge (`route.rs`) mirrors each of those routes into the node's route table as
a dynamic route with the same hops and cost. A request on that route resolves
the path through the link's ingest. moq serves the cheapest route. When it
dies, moq moves to another route with the same first hop; a move between a
direct and a relay route ends the broadcast, and `Subscription::closed` asks the
table again.

Because each link's routes are also kept apart, the node can say which link
serves a path (`Subscription::session`, `Subscription::link`), and list what one
link announces (`Session::origin`, `RelayLink::origin`).
`Session::subscribe` resolves a path over one session only. Rooms read each
member this way.

A direct session's ingest holds only what the session's grant lets the peer
publish. `MoqConfig::grant` computes that grant from the peer's endpoint id.
`iroh_live::grant` lets a peer publish under `live/<peer>/` and, with the
`rooms` feature, `rooms/*/<peer>/`. The table is shared by everything on the
node and answers a ticket without dialing. A peer that could put a route to
another publisher's path into it could stand in for that publisher, and hop
chains cannot prevent this because a peer declares its own hop. A relay link
mirrors everything, since forwarding other publishers' broadcasts is its job.
Attaching a relay trusts it with every path it forwards.

moq moves a broadcast between routes that share a first hop, and a first hop
is only what the publisher declared. So the bridge rewrites a relay route's
first hop to a value derived from the claimed one (`relayed` in `route.rs`).
Routes to one source through different relays still share a first hop and can
take over from each other. A relay route never shares one with a direct route.
A subscription served by a direct session therefore ends when that session
goes, and `Subscription::closed` asks the table again, which finds the relay
route. Without the rewrite, a peer that published someone else's path into a
relay under that publisher's hop would be spliced into the subscription once
the direct session dropped. `iroh-moq/tests/origin.rs` tests the moq behaviour
this relies on, and `iroh-live-relay/tests/relay_bridge.rs` tests the node's.

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

Every session has a publish origin of its own. A publication is offered on a
session when its audience admits the peer and the session's grant covers its
path. The audience is `Everyone` or a watched set of `Peers`. Offers change
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
with the grant `MoqConfig::grant` gives the peer, or `Grant::everything()` if
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

`Moq::attach_relay(RelayConfig::new(url))` attaches to a moq relay through
moq-tokio's client, for `iroh://` and `https://` URLs. The link redials with
backoff, and `RelayLink::status()` watches its `RelayStatus`. Publications with
the `Everyone` audience go to the relay by default (`RelayOffer::Public`). The
relay's routes join the route table at a cost of 10 (`DEFAULT_RELAY_COST`)
against a direct link's cost of one, so a direct route wins while it exists.

`RelayConfig::consume` defaults to true, which copies every route the relay
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

## Errors

`iroh_moq::Error` is the crate's one error type. It covers dial and handshake
failures, refused sessions, a path that was never announced or cannot be
resolved, a publication that already exists, a grant that does not cover an
offer, and shutdown.
