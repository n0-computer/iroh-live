# Tickets

A ticket is everything a viewer needs to reach a broadcast, in one string. There
are two kinds, in two crates.

## LiveTicket

`iroh_live::ticket::LiveTicket` names a publisher's endpoint id and the path its
broadcast is on. It carries no socket addresses.

```rust
use iroh_live::ticket::LiveTicket;

let ticket = LiveTicket::new(live.endpoint().id(), "hello");
println!("{ticket}");

let parsed: LiveTicket = string.parse()?;
```

`Display` produces a URI:

```
iroh-live:<BASE64URL_NOPAD(endpoint id)>/<broadcast-name>
```

`FromStr` accepts that form, the same thing without the `iroh-live:` prefix, and
two older shapes: a base64url `postcard(EndpointAddr)` where the id now sits, and
the legacy `name@BASE32(addr)` that the first builds produced. Both parse, minus
their addresses. Nothing produces either any more.

`to_bytes` and `from_bytes` are the postcard encoding of the whole ticket. Use
them when the ticket travels inside another wire format rather than as text.

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

A call needs no ticket type of its own. Each side publishes under
`calls/<its own endpoint id>` and subscribes to the other's, so a `LiveTicket`
built with `Call::path(my_endpoint_id)` as its name is what you hand the person
you want to call. The per-peer path replaced a fixed `call` name that two
concurrent calls used to collide on.

## RoomTicket

`iroh_rooms::RoomTicket` identifies a room rather than a broadcast. It carries a
gossip topic id and a list of bootstrap endpoints, and it uses the
`iroh_tickets` envelope with kind `room`, so its string form starts with `room`
rather than a URI scheme.

```rust
use iroh_rooms::RoomTicket;

let ticket = RoomTicket::generate();          // fresh topic, no bootstrap
let parsed: RoomTicket = string.parse()?;
```

`RoomTicket::new_from_env` reads `IROH_LIVE_ROOM` for a full ticket, falls back
to `IROH_LIVE_TOPIC` for a hex topic id, and otherwise generates one and logs the
value to reuse.

`Room::ticket()` returns a ticket that includes the calling peer as a bootstrap
endpoint, which is what you pass to someone joining. See [rooms](rooms.md).
