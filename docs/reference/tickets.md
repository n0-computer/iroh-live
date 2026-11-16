# Ticket formats

[Tickets](../concepts/tickets.md) explains what a ticket is for. This page
gives the string forms.

## `BroadcastTicket`

`iroh_live::BroadcastTicket`, defined in `iroh-live-node`, holds a
publisher's endpoint id and the full path of one broadcast. Its string form
is:

```text
ilv1:<peer>/<path>
```

- `<peer>` is the endpoint id in lowercase base32, 52 characters.
- `<path>` is the broadcast path. A path segment that equals the peer's own
  id is written `~`, which keeps the usual `live/<peer>/<name>` ticket short.
- Any other byte outside `A-Z`, `a-z`, `0-9`, `.`, `_` and `-` is
  percent-encoded.
- A ticket has at most 31 path segments and 4000 bytes.

For example, the ticket for `live/<peer>/studio` is
`ilv1:<52 characters>/live/~/studio`, about 72 characters. Serde uses the same
string.

| Call | Returns |
|---|---|
| `LiveLayout::ticket(id, name)` | The ticket for `live/<id>/<name>` |
| `BroadcastTicket::at(peer, path)` | A ticket for any path |
| `Publication::ticket()` | The ticket of a publication |
| `ticket.peer()`, `ticket.path()` | Its parts |
| `"ilv1:...".parse::<BroadcastTicket>()` | The parsed ticket |

`Node::subscribe_ticket` claims the ticket's path for its peer while the
subscription lives. The [access reference](access.md#ticket-claims) gives the
ownership check and the claim limits.

A call ticket is a `BroadcastTicket` for `<root>/<id>/call`, with the name
`iroh_live::calls::CALL`.

## `RoomTicket`

`iroh_live_rooms::RoomTicket` holds a gossip topic id and a list of bootstrap
endpoints. Its string form is an
[`iroh-tickets`](https://docs.rs/iroh-tickets) ticket of kind `room2`.

| Call | Returns |
|---|---|
| `RoomTicket::generate()` | A new topic, with no bootstrap endpoints |
| `Room::ticket()` | The room's ticket, with this member as a bootstrap endpoint |
| `ticket.to_string().parse::<RoomTicket>()` | The parsed ticket |
