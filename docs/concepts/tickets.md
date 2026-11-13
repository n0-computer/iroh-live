# Tickets

A *ticket* is a string that tells a viewer where to find something. It is
what `irl publish` prints, and what a QR code carries. There are two kinds.

## Broadcast tickets

A `BroadcastTicket` holds a publisher's endpoint id and the path of one of its
broadcasts. `LiveLayout::ticket(id, name)` builds the ticket for
`live/<id>/<name>`, and `Publication::ticket()` returns the ticket of any
publication. `Node::subscribe_ticket` plays one on any node.

```rust,ignore
let ticket = layout.ticket(node.id(), "hello")?;
println!("{ticket}"); // ilv1:<endpoint id>/live/~/hello
let parsed: BroadcastTicket = ticket.to_string().parse()?;
```

A broadcast ticket is not a secret. It names a publisher and a path, and the
publisher's policy and the publication's audience decide who may read it.

The ticket holds no network addresses. iroh finds them from the endpoint id,
through DNS and pkarr on the internet and mDNS on a local network. That also
keeps the QR code small.

A call uses a broadcast ticket for the `call` path. `irl call` prints one.

## Room tickets

A `RoomTicket` holds a gossip topic and a list of members to join through.
`Room::ticket()` returns the room's ticket with this member listed, so share
that one with the people who join.

A room ticket is a bearer secret: anyone who has it can join the room and
read its broadcasts. Share it only with the people you invite.

## Where to go next

- [Ticket formats](../reference/tickets.md) gives the string forms and the
  limits on ticket claims.
- [Calls and rooms](calls-and-rooms.md)
