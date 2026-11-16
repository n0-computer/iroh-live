# Run a room

A room holds several participants over a gossip topic. Each member publishes
into the room and plays the others. [Calls and rooms](../concepts/calls-and-rooms.md)
explains the model.

## With `irl`

```sh
irl room --display-name alice           # opens a new room, prints its ticket
irl room --display-name bob <TICKET>    # joins it
```

The window shows the others in a grid and has a chat panel. Pass on the
ticket that the newest window prints, since it lists that member as the one
to join through.

## From Rust

Rooms need the `rooms` feature. The node's config must let members publish
under their room paths, so it takes the union of the live layout and
`RoomsLayout`. `Rooms::mount` adds the gossip protocol to the router next to
the transport.

This program creates a room, prints its ticket, publishes a test pattern, and
plays the first broadcast another member announces. Pass a ticket to join
instead.

```rust,no_run
use iroh::{Watcher, protocol::Router};
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        LocalBroadcast, PlayerConfig, SubscriptionExt, VideoEncoding, VideoRendition, VideoSource,
        video,
    },
    rooms::{RoomTicket, Rooms},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let layout = LiveLayout::default();
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(
        endpoint.clone(),
        iroh_live::layout::config((layout, iroh_live::rooms::RoomsLayout)),
    );
    let rooms = Rooms::new(&node);
    // Members dial in over gossip and moq.
    let _router = rooms
        .mount(node.mount(Router::builder(endpoint)))
        .spawn();
    let ticket = match std::env::args().nth(1) {
        Some(ticket) => ticket.parse()?,
        None => RoomTicket::generate(),
    };
    let room = rooms.join(&ticket, Some("ada".into())).await?;
    println!("room ticket: {}", room.ticket());

    let broadcast: LocalBroadcast = room.publish("cam")?;
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(video::Size::new(640, 360), rate)?,
        VideoEncoding::single(VideoRendition::p360()),
    )?;

    let mut state = room.state();
    let (peer, name) = loop {
        let found = state.get().members.iter().find_map(|(peer, member)| {
            member
                .broadcasts
                .iter()
                .next()
                .map(|name| (*peer, name.clone()))
        });
        if let Some(found) = found {
            break found;
        }
        state.updated().await?;
    };
    let subscription = room.subscribe(peer, &name).await?;
    let player = subscription.remote_broadcast().play(PlayerConfig::default())?;
    let mut frames = player.video_frames();
    while let Some(frame) = frames.next().await {
        println!("{peer} {name}: {}", frame.size());
    }
    Ok(())
}
```

Only an application that passes `RoomsLayout` to its config lets members
publish room broadcasts. The `rooms` feature alone grants nothing. A node
that joins a room without `Rooms::mount` logs a warning, since the other
members cannot reach its gossip.

## Membership

`Room::state()` watches a `RoomState`: every other member, its display name,
the names of its broadcasts, and its connection. A slow reader sees the
latest state and no backlog. To show "joined" and "left", compare two states.

A member renews its announcement every 30 seconds. A member that stops
renewing drops out two to two and a half minutes after its last renewal, and
one that calls `Room::leave()` drops out at once. Lease times use the
member's own clock, so a member whose clock is more than two minutes behind
is never seen.

A member that crashes or loses its network stays listed until its lease runs
out. `RoomMember::connection` tells sooner whether the member is still there.
It reads the node's direct sessions, dialed or accepted, and has three states:

- `Connected`: a direct session with the member is open.
- `Disconnected { since }`: the last direct session with the member closed,
  and none is open. A new session makes the member `Connected` again.
- `Unknown`: this node had no direct session with the member while it was
  listed. This is the case for a member that nobody subscribed to in either
  direction, and for one reached only through a relay.

A member whose process exits closes its sessions, so it turns `Disconnected`
at once. A crash or a network loss closes nothing, so the session ends only
after the QUIC idle timeout, 30 seconds by default. A session that this node
closes itself also counts. Stop retrying a `Disconnected` member's broadcasts,
or show its tile as gone, and keep `Unknown` members as they are. Gossip still
removes the member when its lease runs out.

## Publishing and subscribing

`Room::publish(name)` puts the broadcast at `rooms/<topic>/<this member>/<name>`,
where only the room's members can read it. A member that leaves or expires
loses the path at once. `Room::unpublish(name)`, or closing the broadcast,
removes the name from this member's announcement. A name must not be empty or
contain a slash.

`Room::subscribe(peer, name)` dials the member if needed, and reads the
broadcast over the session with that member, so no other peer can stand in
for it. It waits until the member announces the name, so bound the wait with
a timeout. Leaving the room does not close subscriptions.

A grid cannot follow the room state alone. A member can end a broadcast and
publish it again before its announcement changes, and a member can drop this
node from its membership for a moment. In both cases the state stays the same
and the tile freezes. So a grid also checks `RemoteBroadcast::is_closed()` on a
timer, drops every closed tile, and subscribes again if the member still
lists the name. `iroh-live-cli/src/room.rs` does this.

## Limits

- Rooms have no admission of their own. The ticket is a bearer secret:
  anyone who has it can join, announce itself, and read the members' room
  broadcasts. Share it only with the people you invite.
- Every member that shows every other member subscribes to all of them, so
  rooms suit small groups.
- If every member listed in a ticket is offline, joining succeeds, but the
  room stays empty until another member reaches this one.

A room carries only broadcasts. `irl room` sends chat as one more broadcast
named `chat`, whose `chat` track is a JSON window of the last ten seconds of
messages, the format `@moq/room` reads.

## Where to go next

- [Watch a broadcast and render it](watch.md) draws each member's frames.
- [Make a one-to-one call](call.md) for two people.
