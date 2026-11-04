# iroh-live-rooms

Multi-party rooms over [iroh-gossip](https://github.com/n0-computer/iroh-gossip)
and MoQ. Media-free: nothing here knows what a room's broadcasts carry.

Use it when several peers share a space. A room is a gossip topic; members
announce themselves and the broadcasts they publish. `Rooms::join` returns a
`Room` with its membership as a watched `RoomState`, `Room::publish` places a
broadcast at `rooms/<topic>/<member>/<name>` visible to the members only, and
`Room::subscribe` reads a member's broadcast on demand. Most applications get
this through `iroh_live::rooms` and `LiveBuilder::with_rooms`.

```rust
use iroh_live_rooms::{RoomTicket, Rooms};

let rooms = Rooms::new(&node);
// Mount `rooms.protocol_handler()` under `iroh_live_rooms::ALPN`.
let room = rooms.join(&RoomTicket::generate(), Some("ada".into())).await?;
let _cam = room.publish("cam")?;
```

The node's grant has to let each peer publish under `publish_scope`;
`iroh_live::node_config_with` does that. See the crate docs and
`tests/room.rs` for the rest.

Built on [`iroh-live-node`](../iroh-live-node).

## Features

None.
