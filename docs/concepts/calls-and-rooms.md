# Calls and rooms

A call and a room are both conventions over broadcasts. Neither adds a new
kind of connection.

## Calls

In a one-to-one call, each side publishes one broadcast at
`live/<its id>/call` and subscribes to the other's. The call broadcast starts
visible to nobody. *Ringing* a peer adds it to the broadcast's audience, and
the peer sees the path appear in its route table. Answering subscribes to the
caller's side. Hanging up takes the peer out of the audience again, which ends
its read.

`iroh_live::calls::Calls` implements this. `irl call` and the Android demo use
the same convention, so they can call each other. A policy that admits only
contacts also keeps strangers from ringing.

## Rooms

A room is an [iroh-gossip](https://github.com/n0-computer/iroh-gossip) topic.
Its members announce themselves and the names of the broadcasts they publish
into the room. Each member publishes at `rooms/<topic>/<its id>/<name>`, to
an audience of the current members, and subscribes to the members it wants
to show.

- A *room ticket* holds the topic and some members to join through. Anyone who
  has it can join and read the room, so it is a secret.
- A member that leaves, or stops renewing its announcement, drops out, and
  loses access to the others' room broadcasts at once.
- A member that vanishes without leaving stays listed for about two minutes.
  If this node had a direct session with it, the member is marked as
  disconnected once the last one closes, which tells sooner that it went away.
- Every member that shows every other member subscribes to all of them. There
  is no server that mixes or forwards, so rooms suit small groups.

Rooms live in the `iroh-live-rooms` crate, behind the `rooms` feature of
`iroh-live`. They know nothing about media.

## Where to go next

- [Make a one-to-one call](../how-to/call.md)
- [Run a room](../how-to/rooms.md)
