# Tickets

A ticket is a string that tells a viewer where to find a broadcast. There are
two kinds.

## BroadcastTicket

`iroh_live::BroadcastTicket` holds a publisher's endpoint id and the name of
one of its broadcasts. It maps to the path `live/<endpoint id>/<name>`, where
`Live::publish` puts the broadcast.

```rust
use iroh_live::BroadcastTicket;

let ticket = BroadcastTicket::new(live.endpoint().id(), "hello");
assert_eq!(ticket.path().as_str(), format!("live/{}/hello", ticket.peer()));

let parsed: BroadcastTicket = ticket.to_string().parse()?;
```

A publishing node gets its own ticket with `live.ticket("hello")`.

The string form is a URI:

```
iroh-live:<base64url, no padding, of the endpoint id>/<name>
```

Parsing also accepts it without the `iroh-live:` prefix. Serde uses the same
string. The ticket also implements `iroh_tickets::Ticket` with kind
`broadcast`.

The ticket holds no addresses. iroh finds them from the endpoint id through
pkarr and DNS, and `EndpointOptions` adds mDNS for local networks without
internet access. A ticket without addresses also makes a smaller QR code,
which matters on the Pi demo's small e-paper panel.

## Call tickets

A call uses a `BroadcastTicket` named `call` (`iroh_live::CALL`). `irl call`
and the Android demo each publish their side under that name and subscribe to
the other's over the session between them. `iroh_live::Call` does the
subscribing.

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
