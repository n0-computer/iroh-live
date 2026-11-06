# iroh-live-rooms

Multi-party rooms over [iroh-gossip](https://github.com/n0-computer/iroh-gossip)
and MoQ. Media-free: nothing here knows what a room's broadcasts carry.

Use it when several peers share a space. A room is a gossip topic, and its
members announce themselves and the broadcasts they publish. `Rooms::join` returns a
`Room` with its membership as a watched `RoomState`, `Room::publish` places a
broadcast at `rooms/<topic>/<member>/<name>` visible to the members only, and
`Room::subscribe` reads a member's broadcast on demand. Most applications get
this through `iroh_live::rooms`.

```rust
use iroh_live_rooms::{RoomTicket, Rooms};

let rooms = Rooms::new(&node);
// Mount it on the router with `rooms.mount(router_builder)`.
let room = rooms.join(&RoomTicket::generate(), Some("ada".into())).await?;
let _cam = room.publish("cam")?;
```

The node's grant has to let each peer publish under `publish_scope`.
`LiveLayout::config(&[Arc::new(iroh_live::rooms::publish_scope)])` does that.
`Rooms::shutdown`, or the router's shutdown through the handler, stops the
gossip and every room joined on it. See the crate docs and `tests/room.rs` for
the rest.

Built on [`iroh-live-node`](../iroh-live-node).

## Features

None.
