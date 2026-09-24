# Transport

`iroh-moq` is the transport: a node's route table, its publications, its
sessions with peers, and its links to moq relays. It is the only crate in the
workspace that knows about both iroh and moq-net.

For what MoQ itself is, read [the moq-lite layer
page](https://doc.moq.dev/concept/layer/moq-lite) and [the iroh transport
page](https://doc.moq.dev/concept/layer/iroh) upstream. This page covers what we
build on top.

## Paths name the publisher

A node's own broadcasts live at `live/<endpoint id>/<name>`, and room broadcasts
at `rooms/<topic>/<endpoint id>/<name>`. The same broadcast therefore has the
same path over every link, direct or through any number of relays, which is what
lets a route table see several routes to it, and what lets a relay's token grant
`live/<id>/**` to exactly one publisher. A `BroadcastTicket` still names a
publisher and a name; `ticket.path()` is the path.

For one release a node also answers the bare name on direct sessions, which is
where a node from before this layout looks, and a direct subscribe falls back to
the bare name when the publisher-named path has not appeared within two seconds
and the publisher announces nothing under `live/<its id>/`, the mark of a node on
the older layout. Relays are never offered bare names.

## One route table, fed by every link

Every link writes what its peer announces into an ingest origin of its own, and a
bridge mirrors each of those routes into the node's one route table: a dynamic
route per link route, with the same hop chain and cost, answering requests by
resolving the path through that link and splicing the result. moq picks the best
route in the table (lowest cost among routes of the same anonymity, then fewest
hops) and fails over when it dies.

Keeping the links' routes apart as well as merged is what lets the node say
which link serves a path (`Moq::routes`, `Subscription::session`), and lets a
direct session answer a path that only means something on that session, such as
an old node's bare name.

The bridge of a direct session mirrors only the routes to that peer's own
broadcasts: paths that name the peer, `live/<peer>/...` and
`rooms/<topic>/<peer>/...`. The table is shared by everything on the node and
answers a ticket without dialing, so a peer must not be able to put a route to
another publisher's path into it; hop chains cannot vouch for anything, since a
peer declares its own hop. Whatever else a peer announces, a bare name or an
application path such as `calls/<id>`, stays reachable over its session with
`Session::subscribe`. A relay link mirrors everything, because forwarding other
publishers' broadcasts is its job: attaching a relay trusts it, and its
admission, with every path it forwards.

moq re-splices a broadcast between routes that share a first hop, the
publisher the chain starts at, and a first hop is only what the publisher
declared, which anyone can compute from a public endpoint id. A relay's routes
therefore enter the table under a first hop derived from the one they claim:
routes to one source through different relays still share it, and can take over
from each other, but a relay route never shares a first hop with a direct one.
A subscription served by a direct session ends when that session goes, and
asking again (`Subscription::closed` does) resolves through the relay. Without
this, a peer that publishes someone else's path into a relay under that
publisher's hop would be spliced into a subscription the moment its direct
session dropped. This is the RFD's "a change of first hop ends a broadcast";
`iroh-moq/tests/origin.rs` pins down the moq behaviour underneath, and
`iroh-live-relay/tests/relay_bridge.rs` the node's.

`Moq::subscribe(path, reach)` resolves a path in the table. With no route yet it
reaches out as `Reach` says: dial the publisher the path names, wait for a relay,
or both. The node's hop id is derived from its endpoint id, so a relay
recognizes its routes across a restart.

## Publications and audiences

`Moq::publish(name, broadcast, audience)` places an existing broadcast at
`live/<our id>/<name>`; `publish_at` takes an explicit path. The broadcast is not
created by the transport: anything implementing `Consume<broadcast::Consumer>`
is published by splicing it (`Request::accept`), so one broadcast can be
published at several paths at once.

Every session has a publish origin of its own. A publication is offered on a
session when its audience admits the peer (`Everyone`, a watched set of `Peers`,
or `Manual` with `Session::offer`) and the session's grant covers its path. The
offers change while the session runs, as audiences and grants change.

A withdrawn offer ends what the peer was reading through it. moq keeps serving a
path through a front for as long as the source it was handed lives, even after
the route retracts, and a new request joins that front, so retracting the route
alone would leave a peer that ignores the retraction reading on. Each offer
therefore answers through a gate, a small origin of its own that serves the
publication. Withdrawing the offer (dropping the `OfferGuard`, shrinking a
`Peers` set, `set_audience`, `unpublish`, or the session ending) tears the gate
down, which ends the source of every front spliced from it, and with it the
peer's subscriptions; a request after that finds no route.

## Sessions and admission

`Moq::connect(peer)` dials a peer and returns a `Session`. The actor
deduplicates: a second `connect` to a peer we already have a session with returns
that session, and concurrent dials coalesce onto one. Two peers that dial each
other at once keep both sessions.

Incoming connections arrive through `Moq` itself, which implements iroh's
`ProtocolHandler`. Under `Admission::Open` every session is admitted with
`Grant::everything()`. Under `Admission::Manual` the handshake pauses after the
peer's setup and waits in `Moq::accept()`; the application reads the request
(its path, a `jwt` query parameter, an H3 header) and admits with a `Grant` or
rejects. `Moq::sessions()` watches the open sessions.

Each session runs a connection monitor that reads the selected path's QUIC
statistics five times a second; `Session::link()` returns the latest
`LinkSample`, which iroh-live turns into the media crate's network signals.

## Relay links

Behind the `relay-links` feature, `Moq::attach_relay` stays attached to a moq
relay over moq-tokio's client, for `iroh://` and `https://` URLs. Public
publications go to the relay, and the relay's routes join the route table at a
cost of 10, so a direct route to the same broadcast wins while it exists. The
link redials with backoff; `RelayLink::status()` watches it.

## ALPN negotiation

`iroh_moq::ALPN` is `moq_net::ALPNS[0]`, the newest MoQ version this build
speaks, so it tracks the moq-net dependency rather than a string someone has to
remember to bump. `iroh_moq::alpns()` returns the whole `moq_net::ALPNS` list
newest first, with HTTP/3 appended last. Mount the node under all of them, and
the dial offers the rest through `ConnectOptions::with_additional_alpns`.

Both halves of the handshake branch on what was negotiated. Raw QUIC carries the
MoQ stream directly. H3 answers a CONNECT first: the client builds a
`web_transport_proto::ConnectRequest` listing every moq-lite ALPN as a protocol,
and the server replies `ConnectResponse::OK` echoing the first one requested. An
ALPN this build does not speak is `Error::UnsupportedAlpn`.

## Errors

`iroh_moq::Error` is the crate's one error type: dial and handshake failures,
refused sessions, a path that was never announced, a publication that already
exists, a grant that does not cover an offer, and shutdown.
