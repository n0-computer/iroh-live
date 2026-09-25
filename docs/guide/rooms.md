# Rooms

A room is a gossip topic. Its members announce themselves and the names of the
broadcasts they publish into the room. `iroh_live_rooms::Room` tracks the
membership and subscribes to a member's broadcast when the application asks.

Rooms do not know about media. `iroh-live-rooms` publishes anything that
implements moq-net's `Consume<broadcast::Consumer>`, and returns an
`iroh_moq::Subscription`.

## Joining

`Rooms` owns the gossip instance. Create it on the builder's `Moq` before the
router, so that the router can mount it. The node's grant must let members
publish under `iroh_live_rooms::publish_scope(member)`.
`iroh_live::moq_config()` does that when `iroh-live` has the `rooms` feature:

```rust
use iroh_live::{Live, rooms::{RoomTicket, Rooms}};

let builder = Live::builder(endpoint).with_router();
let rooms = Rooms::new(builder.moq());
let live = builder
    .accept(iroh_live::rooms::ALPN, rooms.protocol_handler())
    .spawn();

let room = rooms
    .join(&RoomTicket::generate(), Some("ada".into()))
    .await?;
```

`Rooms::with_gossip` takes a gossip instance the application already runs.

Share `room.ticket()` with the people who join. It lists this member as the
bootstrap endpoint.

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
room.publish("cam", &broadcast)?;
let subscription = room.subscribe(peer, "cam").await?;
let player = live
    .remote_broadcast(&subscription)
    .play(PlayerConfig::default())?;
```

`publish` puts the broadcast at `rooms/<topic>/<this member>/<name>`, and only
the room's members can read it. A member that leaves or expires loses what it
was reading. Unpublishing the returned `Publication` or ending the broadcast
removes the name from this member's announcement. A name must not be empty or
contain a slash.

`subscribe` dials the member if needed and reads the broadcast over the session
with that member, so no other peer can stand in for it. It waits until the
member announces the name, so bound the wait with a timeout. Leaving the room
does not close subscriptions. Errors are `iroh_live_rooms::Error`, and its
`Transport` variant holds an `iroh_moq::Error`.

A grid cannot follow the room state alone. A member can end a broadcast and
publish it again before its announcement changes. A member can also drop this
node from its membership for a moment, which cuts off what this node reads. In
both cases the state stays the same and the tile freezes. So a grid also drops
every tile whose `RemoteBroadcast::is_closed()` is true, on a timer, and
subscribes again if the member still lists the name.
`iroh-live-cli/src/room.rs` does this.

Anyone who knows the topic id can join the room and announce itself.

## Chat and other data

A room carries only broadcasts. `irl room` sends chat as one more broadcast,
named `chat`, which carries `moq-room`'s chat track.

## Limitations

Every member that shows every other member subscribes to all of them. There is
no selective forwarding, so rooms suit small groups.

If every bootstrap endpoint in a ticket is offline, joining succeeds but the
room stays empty until another member reaches this one.

`irl room` is the quickest way to try a room. See [the CLI
reference](../cli.md#irl-room).
