# Rooms

A room is a gossip topic plus the MoQ subscriptions that follow from it. Peers
publish the *names* of their broadcasts into a replicated key-value map on the
topic, and `iroh_rooms::Room` turns every name it sees into a subscription
against that peer.

Rooms know nothing about media. `iroh-rooms` does not depend on `moq-media` or
`hang`, and a subscription arrives as a raw `moq_net::broadcast::Consumer`. What
the broadcast carries is the application's business.

This crate is a holding pattern. It was cut out of `iroh-live` during the v2
rewrite so the media stack could be replaced without carrying rooms along, and
the intent is to rebuild it on moq's own announce bus with moq-token path scoping,
keeping gossip only for bootstrap. Expect the API to change.

## Joining

`Room::new` takes the three things it needs rather than an application type:

```rust
use iroh_rooms::{Room, RoomEvent, RoomTicket};

let mut room = Room::new(&endpoint, &moq, &gossip, RoomTicket::generate()).await?;
```

`iroh-live` supplies all three: `live.endpoint()`, `live.transport()`, and
`live.gossip()`, the last of which needs `LiveBuilder::with_gossip()`.

Share `room.ticket()` with the people joining. It includes the calling peer as a
bootstrap endpoint, so a joiner can find the topic without a directory service.

## Publishing and receiving

```rust
let mut broadcast = room.publish("cam").await?;
```

`publish` creates a broadcast on the node origin and announces its name into the
room's state map. It returns the bare `moq_net::broadcast::Producer`. To publish
media, wrap it: `moq_media::publish::LocalBroadcast::new(producer)` is what
`Live::publish` does. Dropping the producer un-announces the name.

Events arrive on the room itself, or on the receiver half if you split it:

| Event | Meaning |
|---|---|
| `PeerJoined` | A peer appeared in the topic, with its display name if it set one |
| `RemoteAnnounced` | A peer listed the broadcast names it publishes |
| `BroadcastSubscribed` | We subscribed to one of them; carries the session and the consumer |
| `ChatReceived` | A chat message from a peer |
| `PeerLeft` | Every broadcast we held from a peer closed |

`RemoteAnnounced` is followed by a `BroadcastSubscribed` for each name, because
the room subscribes on your behalf.

`Room::split()` returns a `RoomEvents` receiver and a cloneable `RoomHandle`, for
an application that reads events on one task and publishes from another. The
actor stops when the room and every handle are dropped.

## Chat

Chat lives on a well-known track named `chat`, at a priority below audio and
video. Each message is one group holding one frame of UTF-8 text, so there is no
framing beyond the string. The sender's identity comes from the broadcast
carrying the track rather than the payload.

`room.send_chat("hello")` writes through the publisher registered with
`set_chat_publisher`, and incoming messages arrive as `RoomEvent::ChatReceived`.
`ChatPublisher::finish` matters: dropping a publisher without it discards the
cache and loses the last message.

## Discovery

Peer state is an `iroh-smol-kv` map on the gossip topic, holding each peer's
broadcast names and optional display name. Anti-entropy runs every 60 seconds,
with a one-second fast interval while things are changing and a two-minute expiry
horizon.

`PeerLeft` is derived from every subscribed consumer of that peer closing rather
than from a gossip signal, so it reflects the transport rather than the
membership map.

## Limitations

Every peer subscribes to every other peer's broadcasts. There is no selective
forwarding and no topology optimisation, so this is a small-group design.

If every bootstrap endpoint in a ticket is offline, joining waits until some peer
turns up. Including several bootstrap endpoints helps.

`irl room` shows a room as a grid of pictures with a chat panel, and is the
quickest way to see all of this working. See [the CLI reference](../cli.md) for
its flags.
