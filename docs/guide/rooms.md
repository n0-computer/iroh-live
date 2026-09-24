# Rooms

A room is a gossip topic whose members announce themselves and the names of the
broadcasts they publish into it. `iroh_rooms::Room` watches the membership and
subscribes to a member's broadcast when the application asks for it.

Rooms know nothing about media. `iroh-rooms` does not depend on `iroh-live-media`
or `hang`: a room publishes anything that implements moq-net's
`Consume<broadcast::Consumer>` and hands back a transport `Subscription`. What
the broadcast carries is the application's business.

## Joining

`Rooms` owns the one gossip instance rooms need. Create it on the node's `Moq`
before the router, so the router can mount it. The node must let members
publish their room broadcasts, under `iroh_rooms::publish_scope(member)`;
`iroh_live::moq_config()` does with the `rooms` feature:

```rust
use iroh_live::{Live, Moq};
use iroh_rooms::{RoomConfig, RoomTicket, Rooms};

let moq = Moq::new(endpoint.clone(), iroh_live::moq_config());
let rooms = Rooms::new(&moq);
let live = Live::builder(endpoint)
    .with_moq(moq)
    .with_router()
    .accept(iroh_rooms::ALPN, rooms.protocol_handler())
    .spawn();

let room = rooms
    .join(&RoomTicket::generate(), RoomConfig::default().with_display_name("ada"))
    .await?;
```

`Rooms::with_gossip` takes a gossip instance the application already runs.

Share `room.ticket()` with the people joining. It includes this member as a
bootstrap endpoint, so a joiner finds the topic without a directory service.

## Membership

`room.state()` is a watcher over the membership: every other member, its
display name, and the names of the broadcasts it publishes. A watcher coalesces,
so a slow reader sees the latest state rather than a backlog, and nothing it
does can stall the room.

A member is in the room while its gossip lease holds. Every member rewrites its
announcement every 30 seconds, and one that stops is dropped two to two and a
half minutes after its last one (the gossip map sweeps expired entries every 30
seconds); a member that calls `room.leave()` says so and is dropped at once.
Lease times are the member's own clock, so a member whose clock runs more than
two minutes behind is never seen.
Ending a broadcast changes the member's `broadcasts`, not its membership.

A user interface that wants "joined" and "left" lines diffs two states.

## Publishing and subscribing

```rust
room.publish("cam", &broadcast)?;
let subscription = room.subscribe(peer, "cam").await?;
let player = live.remote_broadcast(&subscription).play(PlayerConfig::default())?;
```

`publish` places the broadcast at `rooms/<topic>/<this member>/<name>` with the
room's membership as its audience, so it is offered to members and to nobody
else who connects to this node. A member that leaves or expires is cut off from
what it was reading. Unpublishing the returned `Publication`, or ending the
broadcast, takes the name out of this member's announcement. Names holding a
slash are refused, since they would reach into another member's part of the
room's path. A path that names its publisher is also what would let a relay
serve a room later: a token granting `rooms/<topic>/**` admits exactly that
room. Today room broadcasts go to members directly and never through a relay.

`subscribe` resolves a member's broadcast on demand over the session with that
member, so a grid that shows four tiles of twelve subscribes to four. It reads
what the member itself announces on that session, so no other peer can stand in
for the member. It waits until the member announces the name to this node,
which a member that never published it does not do, so bound the wait.
Subscriptions are the application's: leaving the room does not close them.
Failures come back as `iroh_rooms::Error`, whose `Transport` variant carries
the same `iroh_moq::Error` the facade's `Error::Transport` does.

A grid cannot drive its tiles from the room state alone. A member can end a
broadcast and publish it again faster than its announcement changes, and a
member that briefly stops counting this node as a member cuts off what this
node reads; in both cases the membership and the member's `broadcasts` stay
as they were, and a tile that only follows the state freezes. So a grid also
drops every tile whose `RemoteBroadcast::is_closed()` turned true, a failed
session included, and subscribes to the name again a moment later if the
member still lists it. `iroh-live-cli/src/room.rs` does exactly this. Keep in mind
that a broadcast following a route table reports closed only about three
seconds after it actually ended.

Rooms are private to whoever holds the ticket. Membership is self-declared:
anyone who knows the topic id can join the gossip topic and announce itself.

## Chat and other data

A room has no chat of its own. Anything members share besides media is one more
broadcast published into the room: `irl room` publishes a broadcast named
`chat`, with one message per group, and reads every other member's.

## Limitations

Every member that shows every other subscribes to all of them. There is no
selective forwarding, so this is a small-group design.

If every bootstrap endpoint in a ticket is offline, joining still succeeds, but
the room stays empty until some member reaches this one. Including several
bootstrap endpoints helps.

`irl room` shows a room as a grid of pictures with a chat panel, and is the
quickest way to see all of this working. See [the CLI reference](../cli.md) for
its flags.
