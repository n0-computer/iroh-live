# Rooms

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | iroh-live |
| Platforms | all |

Rooms provide multi-party media sessions where participants discover each other automatically. Each participant publishes their own broadcast into the room and subscribes to every other participant. Peer discovery is handled by iroh-gossip: when a new peer joins, the room announces its presence to all existing members.

This feature is experimental. The participant model and event API are still evolving.

## Creating a room

A room is identified by a gossip topic. The first participant generates a new `RoomTicket`, which contains a random topic ID:

```rust
let ticket = RoomTicket::generate();
let room = Room::join(&live, ticket.clone()).await?;
println!("Room ticket: {ticket}");
```

The printed ticket string can be shared with other participants.

## Joining a room

Other participants parse the ticket and join:

```rust
let ticket: RoomTicket = ticket_string.parse()?;
let room = Room::join(&live, ticket).await?;
```

When you join, the room's internal actor connects to bootstrap peers from the ticket, joins the gossip topic, and begins discovering other participants.

## Publishing into a room

Use the room handle to publish a broadcast. The room announces it to all peers via gossip:

```rust
let (handle, mut events) = room.split();
handle.publish("my-stream", broadcast_producer).await?;
```

The `split` method separates the publish handle (cloneable, shareable across tasks) from the event receiver.

## Receiving events

The event stream delivers notifications when remote participants join, leave, or update their broadcasts:

```rust
while let Some(event) = events.recv().await {
    match event {
        RoomEvent::PeerJoined { peer_id, session, remote } => {
            // A new participant's broadcast is ready to consume.
            // `remote` is a RemoteBroadcast with video and audio tracks.
        }
        RoomEvent::PeerLeft { peer_id } => {
            // A participant disconnected.
        }
    }
}
```

Each `PeerJoined` event includes a `RemoteBroadcast` that you can use to access video and audio tracks, exactly as you would with a direct subscription.

## The rooms example

The `rooms` example in `iroh-live/examples/` demonstrates the full flow:

```sh
# First participant creates the room
cargo run --release --example rooms

# Others join with the printed ticket
cargo run --release --example rooms -- <TICKET>
```

Each participant captures their camera and microphone, publishes into the room, and renders video from all other participants.

## Gossip and bootstrap

Rooms use iroh-gossip for peer discovery. The gossip topic acts as a rendezvous point: any peer that knows the topic can join the conversation. The `RoomTicket` includes bootstrap peer IDs so that new participants can find existing members without a central server.

When you call `RoomHandle::ticket()`, the returned ticket includes your own endpoint ID as a bootstrap node, making it easy to pass to someone who wants to join.

## Limitations

- The room actor spawns a connection to every discovered peer. There is no topology optimization (e.g., selective forwarding) for large groups.
- If a bootstrap peer is offline, joining may fail until other peers are discovered through gossip. Including multiple bootstrap peers in the ticket improves resilience.
- The event API is minimal. Future versions may add events for track changes, network quality, and participant metadata.
