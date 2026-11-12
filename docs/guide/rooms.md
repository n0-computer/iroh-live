# Rooms

A room is a gossip topic. Its members announce themselves and the names of the
broadcasts they publish into the room. `iroh_live_rooms::Room` tracks the
membership and subscribes to a member's broadcast when the application asks.

Rooms do not know about media. `Room::publish` creates a broadcast in the
node's route table and returns it as any type built from a moq-net
`broadcast::Producer`, such as a `LocalBroadcast`. `Room::subscribe` returns an
`iroh_live_node::Subscription`.

## Joining

`Rooms` owns the gossip instance, and the router mounts it. The node's policy
must let members publish under their `iroh_live::rooms::RoomsLayout` scope,
`rooms/<topic>/<member>/`. Give the node the union of the live layout and the
rooms layout with `iroh_live::layout::config`, and mount the gossip with
`Rooms::mount` next to the transport. Only an application that passes the
rooms layout grants it. Enabling the `rooms` feature alone grants nothing:

```rust
use iroh::protocol::Router;
use iroh_live::{
    Node,
    layout::{self, LiveLayout},
    rooms::{RoomTicket, Rooms, RoomsLayout},
};

let node = Node::new(
    endpoint.clone(),
    layout::config((LiveLayout::default(), RoomsLayout)),
);
let rooms = Rooms::new(&node);
let router = rooms.mount(node.mount(Router::builder(endpoint))).spawn();

let room = rooms
    .join(&RoomTicket::generate(), Some("ada".into()))
    .await?;
```

A node that joins a room without the mount logs a warning, since the other
members cannot reach its gossip. `Router::shutdown` shuts down the node and the
rooms. To shut the rooms down yourself, call `Rooms::shutdown`.

Share `room.ticket()` with the people who join. It lists this member as the
bootstrap endpoint. The ticket is a bearer secret: anyone who has it can join
and read the room's broadcasts.

## Membership

`room.state()` is a watcher over a `RoomState`: every other member, its display
name, and the names of its broadcasts. A slow reader sees the latest state and
no backlog.

A member stays in the room while its gossip lease holds. Each member renews
its announcement every 30 seconds. A member that stops renewing drops out two
to two and a half minutes after its last renewal. A member that calls
`room.leave()` drops out at once. Lease times use the member's own clock, so a
member whose clock is more than two minutes behind is never seen.

Ending a broadcast changes the member's `broadcasts`, not its membership. To
show "joined" and "left", compare two states.

## Publishing and subscribing

```rust
use iroh_live::media::{LocalBroadcast, PlayerConfig, remote_broadcast};

let broadcast: LocalBroadcast = room.publish("cam")?;
broadcast.set_video(source, encoding)?;
let subscription = room.subscribe(peer, "cam").await?;
let player = remote_broadcast(&subscription).play(PlayerConfig::default())?;
```

`publish` puts the broadcast at `rooms/<topic>/<this member>/<name>`, and only
the room's members can read it. A member that leaves or expires loses the path
at once, and what it reads there ends. `Room::unpublish(name)` or closing the
broadcast removes the name from this member's announcement. A name must not be empty or
contain a slash.

`subscribe` dials the member if needed and reads the broadcast over the session
with that member, so no other peer can stand in for it. It waits until the
member announces the name, so bound the wait with a timeout. Leaving the room
does not close subscriptions. Errors are `iroh_live_rooms::Error`, and its
`Node` variant holds an `iroh_live_node::Error`.

A grid cannot follow the room state alone. A member can end a broadcast and
publish it again before its announcement changes. A member can also drop this
node from its membership for a moment, which retracts its paths from this node. In
both cases the state stays the same and the tile freezes. So a grid also drops
every tile whose `RemoteBroadcast::is_closed()` is true, on a timer, and
subscribes again if the member still lists the name.
`iroh-live-cli/src/room.rs` does this.

Rooms have no admission of their own. Anyone who knows the topic id can join
the room, announce itself, and read the members' room broadcasts, since every
member admits every other member to its room paths. Closed rooms, with a
membership the members check, are not built yet.

## Chat and other data

A room carries only broadcasts. `irl room` sends chat as one more broadcast,
named `chat`, whose `chat` track is a JSON window of the last ten seconds of
messages, the format `@moq/room` reads.

## Limitations

Every member that shows every other member subscribes to all of them. There is
no selective forwarding, so rooms suit small groups.

If every bootstrap endpoint in a ticket is offline, joining succeeds but the
room stays empty until another member reaches this one.

`irl room` is the quickest way to try a room. See [the CLI
reference](../cli.md#irl-room).
