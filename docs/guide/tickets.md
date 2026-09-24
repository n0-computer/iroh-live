# Tickets

A ticket is everything a viewer needs to reach a broadcast, in one string. There
are two kinds, in two crates.

## BroadcastTicket

`iroh_live::BroadcastTicket` names a publisher's endpoint id and the name of one
of its broadcasts, which it publishes at `live/<endpoint id>/<name>`. It carries
no socket addresses.

```rust
use iroh_live::BroadcastTicket;

let ticket = BroadcastTicket::new(live.endpoint().id(), "hello");
println!("{ticket}");
assert_eq!(ticket.path().as_str(), format!("live/{}/hello", ticket.peer()));

let parsed: BroadcastTicket = string.parse()?;
```

The publishing node mints its own with `live.ticket("hello")`, which names the
path `Live::publish` puts a broadcast at.

`Display` produces a URI:

```
iroh-live:<BASE64URL_NOPAD(endpoint id)>/<broadcast-name>
```

`FromStr` accepts that form, and the same thing without the `iroh-live:` prefix.

Serde goes through the same string. The ticket also implements
`iroh_tickets::Ticket` with kind `broadcast`, for applications that carry
tickets in iroh's envelope.

### Why no addresses

Addresses used to travel in the ticket, and on a host with several interfaces
they were most of it: a Pi with ten of them produced a 184-character ticket where
63 characters would do. Every one of those addresses is something iroh's address
lookup already finds from the id alone. Pkarr and DNS answer wherever both ends
have internet, and `irl` and the demos add mDNS on top, which answers on a local
network that has no route out at all. Between the two there is no network where
the ticket's own copy of the addresses was the thing that made the connection.

A shorter ticket is a sparser QR code, and that is the point. 63 characters fit
in a 37-module code where 184 needed 57. On the Pi demo's 122-pixel e-paper panel
that is three pixels per module instead of one.

This is a breaking change to the ticket format: a build from before it cannot
read a ticket minted after it.

## Call tickets

A call needs no ticket type of its own. `irl call` and the Android demo each
publish a broadcast named `call` and subscribe to the other's on the session
between them, so the callee only needs the caller's endpoint id. The ticket they
hand out is a `BroadcastTicket` named `call`.

## RoomTicket

`iroh_live_rooms::RoomTicket` identifies a room rather than a broadcast. It carries a
gossip topic id and a list of bootstrap endpoints, and it uses the
`iroh_tickets` envelope with kind `room`, so its string form starts with `room`
rather than a URI scheme.

```rust
use iroh_live_rooms::RoomTicket;

let ticket = RoomTicket::generate();          // fresh topic, no bootstrap
let parsed: RoomTicket = string.parse()?;
```

`Room::ticket()` returns a ticket that includes the calling peer as a bootstrap
endpoint, which is what you pass to someone joining. See [rooms](rooms.md).
