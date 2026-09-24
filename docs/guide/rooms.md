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
before the router, so the router can mount it:

```rust
use iroh_live::{Live, Moq, MoqConfig};
use iroh_rooms::{RoomConfig, RoomTicket, Rooms};

let moq = Moq::new(endpoint.clone(), MoqConfig::default());
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
announcement every 30 seconds, and one that stops is dropped two minutes after
its last one; a member that calls `room.leave()` says so and is dropped at once.
Ending a broadcast changes the member's `broadcasts`, not its membership.

A user interface that wants "joined" and "left" lines diffs two states.

## Publishing and subscribing

```rust
room.publish("cam", broadcast.consume())?;
let subscription = room.subscribe(peer, "cam").await?;
let remote = live.remote_broadcast(&subscription).await?;
```

`publish` places the broadcast at `rooms/<topic>/<this member>/<name>` with the
room's membership as its audience, so it is offered to members and to nobody
else who connects to this node. A path that names its publisher is also what
lets a relay serve a room later: a token granting `rooms/<topic>/**` admits
exactly that room.

`subscribe` resolves a member's broadcast on demand, directly or through a relay
that routes it, so a grid that shows four tiles of twelve subscribes to four.

## Chat

Chat is the room's own. Every member publishes a small chat broadcast at
`rooms/<topic>/<member>/.chat`, to members only, so chat works with the camera
off. `room.send_chat("hello")` writes to it, and `room.chat()` returns a
`ChatReceiver` with a buffer of its own: a receiver that falls behind gets
`ChatError::Lagged(n)` and the room carries on. A message carries its sender and
the sender's wall-clock time. A room's receivers carry other members' messages,
not this member's own.

Names starting with a dot are the room's own, and `publish` refuses them.

## Compatibility with the previous release

For one release a room interoperates with members on the previous one. It writes
an announcement the previous release reads (the new fields come after the old
ones, which postcard lets an older reader ignore), answers the previous
release's paths `rooms/<topic>/<name>` on direct sessions, and writes chat both
as the new `chat.v2` track and as the bare-text `chat` track the previous release
reads; its chat broadcast is listed among its broadcasts for that release. It
reads both announcement layouts, subscribes to an older member at the older
path, and reads an older member's chat from the `chat` track of the broadcasts
it publishes. An older member sees the chat broadcast as one more broadcast,
which carries no picture.

## Limitations

Every member that shows every other subscribes to all of them. There is no
selective forwarding, so this is a small-group design.

If every bootstrap endpoint in a ticket is offline, joining waits until some
member turns up. Including several bootstrap endpoints helps.

`irl room` shows a room as a grid of pictures with a chat panel, and is the
quickest way to see all of this working. See [the CLI reference](../cli.md) for
its flags.
