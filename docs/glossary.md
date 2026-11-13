# Glossary

These docs use each term below in one meaning only.

| Term | Meaning |
|---|---|
| Audience | Who sees one publication: every granted session (`Audience::Granted`), or a set of peers (`Audience::Peers`). See [Access control](concepts/access.md#audience) |
| Broadcast | A named set of tracks that one node publishes, such as a camera's video and audio. See [Broadcasts](concepts/broadcasts.md) |
| Catalog | The track that describes a broadcast's other tracks: codec, size, bitrate |
| Claim | A path that `Node::subscribe_ticket` lets a ticket's publisher publish, while the subscription lives. See [Access reference](reference/access.md#ticket-claims) |
| Endpoint | iroh's QUIC socket with a key pair. One node runs on one endpoint |
| Endpoint id | The public key of an endpoint, which identifies a node. Not "node id" or "peer id". See [Nodes and endpoints](concepts/nodes.md) |
| Grant | The paths a peer may read and publish, as two sets of patterns |
| Group | A run of frames that starts with a keyframe. A late reader skips to the newest group |
| iroh relay | A server that forwards encrypted packets between endpoints that cannot reach each other. Knows nothing about media. See [Relays](concepts/relays.md) |
| iroh-live relay | The `iroh-live-relay` server: a MoQ relay that serves broadcasts to many viewers, browsers included. See [Relays](concepts/relays.md) |
| Ladder | The renditions of one source, from small to large |
| Layout | A rule for paths: where each peer publishes, and which peer owns a path. `LiveLayout` and `RoomsLayout` are layouts. See [Paths and layouts](concepts/paths.md) |
| MoQ | Media over QUIC, the publish and subscribe protocol iroh-live speaks |
| Node | The iroh-live state on one endpoint: route table, publications, sessions, policy |
| Path | The name of a broadcast, such as `live/<endpoint id>/hello` |
| Pattern | A path with wildcards: `*` for one segment, `**` for the rest |
| Player | Decodes a subscribed broadcast, picks a rendition, and keeps audio and video in step |
| Policy | What decides which peers may connect, and with which grant. See [Access control](concepts/access.md) |
| Publication | A broadcast that a node publishes at a path, with its audience |
| Pull | The iroh-live relay fetching a broadcast from its publisher, because a viewer named its ticket |
| Reach | Where `Node::subscribe` looks for a path it has no route to: the publisher, the relays, or both. See [Routes](concepts/routes.md#reach) |
| Relay link | A node's open session with a MoQ relay, from `Node::attach_relay` |
| Rendition | One encoding of a video source, at one size and bitrate, carried on its own track |
| Route | A way to reach a broadcast: a session or relay link that announces its path |
| Route table | A node's map from paths to routes, fed by every session and relay link |
| Session | A MoQ connection between two nodes, or between a node and a relay |
| Ticket | A string that names a broadcast (`BroadcastTicket`) or a room (`RoomTicket`). See [Tickets](concepts/tickets.md) |
| Track | One stream inside a broadcast: a rendition, the audio, or data |
