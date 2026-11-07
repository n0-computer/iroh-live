# Tickets

A ticket is a string that tells a viewer where to find a broadcast. There are
two kinds.

## BroadcastTicket

`iroh_live::BroadcastTicket` holds a publisher's endpoint id and the full path
of one of its broadcasts. It is defined in `iroh-live-node` and names any path,
whatever layout chose it. `LiveLayout` puts a broadcast at
`live/<endpoint id>/<name>`.

```rust
use iroh_live::{BroadcastTicket, layout::LiveLayout};

let layout = LiveLayout::default();
let ticket = layout.ticket(node.id(), "hello")?;
assert_eq!(ticket.path().as_str(), format!("live/{}/hello", ticket.peer()));

let same = BroadcastTicket::at(node.id(), layout.path(node.id(), "hello")?);
let parsed: BroadcastTicket = ticket.to_string().parse()?;
```

A published broadcast gives its own ticket with `Publication::ticket()`.
`BroadcastTicket::at(peer, path)` builds one for any path. The old `name()`
and `new(id, name)` are gone.

The string form is:

```
ilv1:<peer>/<path>
```

`<peer>` is the endpoint id in lowercase base32, 52 characters. `<path>` is the
broadcast path. A path segment that equals the peer's own id is written `~`,
which keeps the usual `live/<peer>/<name>` ticket short. Any other byte outside
`A-Z`, `a-z`, `0-9`, `.`, `_` and `-` is percent-encoded. A ticket has at most
31 path segments and 4000 bytes. For example, the ticket for `live/<peer>/studio`
is `ilv1:<52 characters>/live/~/studio`, about 72 characters. Serde uses the
same string.

Tickets in the old `iroh-live:<base64url>/<name>` form do not parse. Issue them
again, and replace any printed QR code.

`Node::subscribe_ticket` reads a ticket on any node, including a default
subscribe-only one. It claims exactly the ticket's path for the ticket's peer
through `Node::claim_path`, for as long as the subscription lives. A node
accepts at most 64 claimed paths and 16 KiB of paths per peer. It also checks
that the ticket's publisher owns the path: exactly one segment is the peer's
64-character hex id, and no other segment parses as an endpoint id. A
`NodeConfig::owner` function replaces that rule. A path that fails it returns
`NotOwner`.

The ticket holds no addresses. iroh finds them from the endpoint id through
pkarr and DNS, and `EndpointOptions` adds mDNS for local networks without
internet access. A ticket without addresses also makes a smaller QR code,
which matters on the Pi demo's small e-paper panel.

## Call tickets

A call uses a `BroadcastTicket` for the path `<root>/<id>/call`
(`iroh_live::calls::CALL`). `Calls::new(&node, &layout)` publishes this node's
side there, visible to nobody, and `Calls::ring(peer)` offers it to `peer` only.
`Calls::answer(peer)` subscribes to the other's. A peer's `call` path appearing
in the route table is the ring. Dropping a `Call`, or `Call::hang_up`, hangs
up. `irl call` and the Android demo use both.

## RoomTicket

`iroh_live_rooms::RoomTicket` identifies a room. It holds a gossip topic id and
a list of bootstrap endpoints. Its string form is an `iroh_tickets` ticket of
kind `room`.

```rust
use iroh_live_rooms::RoomTicket;

let ticket = RoomTicket::generate();          // a new topic, no bootstrap peers
let parsed: RoomTicket = ticket.to_string().parse()?;
```

`Room::ticket()` returns the room's ticket with this member as its bootstrap
endpoint. Share that one with people who join. See [Rooms](rooms.md).
