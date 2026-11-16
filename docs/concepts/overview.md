# How it fits together

A *node* publishes broadcasts at paths and subscribes to other nodes'
broadcasts. Every connection a node holds, direct or through a relay, feeds
one route table, and a subscription reads whichever route serves its path.

```text
                 your process
  +--------------------------------------------------+
  |  iroh Endpoint  (key = endpoint id)              |
  |      |                                           |
  |  Node --- policy: who may connect, read, publish |
  |   |                                              |
  |   +-- route table: path -> routes                |
  |   |      ^        ^                              |
  |   |      |        |                              |
  |   |  session    relay link                       |
  |   |  (direct)   (attach_relay)                   |
  |   |      |        |                              |
  |   +-- publications: live/<my id>/hello, ...      |
  +----------|--------|------------------------------+
             |        |
         peer node   iroh-live relay ---- browsers
```

## The pieces

- The **endpoint** is iroh's. Its key is the node's identity, and the public
  half is the *endpoint id*. See [Nodes and endpoints](nodes.md).
- A **broadcast** is a set of tracks that one node publishes: video
  renditions, audio, and any data. See
  [Broadcasts, tracks and renditions](broadcasts.md).
- A **path** names a broadcast, such as `live/<endpoint id>/hello`. A
  *layout* is the rule for choosing paths. See [Paths and layouts](paths.md).
- A **session** is a MoQ connection with one peer. A **relay link** is a
  session with a relay that the node keeps open. Both feed the **route
  table**. See [Routes, sessions and reach](routes.md).
- The **policy** decides which peers may connect, and what each may read and
  publish. See [Access control](access.md).
- A **ticket** is a string that names a publisher and a path, for a viewer to
  paste or scan. See [Tickets](tickets.md).

## Publishing

1. The application binds an endpoint and creates a `Node` with a policy.
2. It mounts the node on an iroh `Router`, so peers can connect to it.
3. It publishes a broadcast at a path, and fills it with sources.
4. It shares a ticket for that path.

Encoders run only while someone watches. A broadcast is visible to every
session whose grant covers its path, or to a set of peers that the
application names.

## Subscribing

1. The viewer creates a `Node`. A viewer only dials, so it needs no router.
2. It subscribes to the ticket. The node looks the path up in its route
   table. If no route is there yet, it dials the publisher or waits for a
   relay.
3. It reads the subscription as media and starts a player.

When the route changes, for example from a relay to a direct session, the
subscription moves with it and does not end. The player measures the link
that serves it, and picks a rendition to match.

## Where to go next

- [Quickstart: library](../getting-started/library-quickstart.md) shows both
  sides as programs.
- [The transport architecture](../architecture/transport.md) explains the
  route table in depth.
