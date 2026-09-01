# Tickets

A ticket is everything a viewer needs to reach a broadcast, in one string. There
are two kinds, in two crates.

## LiveTicket

`iroh_live::ticket::LiveTicket` names a publisher's endpoint address and the path
its broadcast is on.

```rust
use iroh_live::ticket::LiveTicket;

let ticket = LiveTicket::new(live.endpoint().addr(), "hello");
println!("{ticket}");

let parsed: LiveTicket = string.parse()?;
```

`Display` produces a URI:

```
iroh-live:<BASE64URL_NOPAD(postcard(EndpointAddr))>/<broadcast-name>
```

`FromStr` accepts that form, the same thing without the `iroh-live:` prefix, and
the legacy `name@BASE32(addr)` shape that older builds produced. Nothing produces
the legacy form any more.

`to_bytes` and `from_bytes` are the postcard encoding of the whole ticket. Use
them when the ticket travels inside another wire format rather than as text.

`with_relay_urls` sets a list of MoQ relay URLs for a viewer that cannot reach the
publisher directly. Those survive `to_bytes` but not the URI, which carries only
the endpoint address and the name.

The URI stays under 2000 characters, which is short enough for a QR code. The Pi
demo renders one on an e-paper display, and `irl publish` prints one in the
terminal unless you pass `--no-qr`.

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
