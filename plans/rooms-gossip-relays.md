## Plan: native room support for gossip and relays

Status: research + design. No implementation work in this plan yet.

### Why

Today a [`Room`] couples two concerns:

1. **Discovery.** Peers announce which broadcasts they are publishing,
   via a shared gossip topic backed by iroh-smol-kv.
2. **Transport.** When a peer announces a broadcast the room connects
   directly to it over raw MoQ-over-iroh (`Moq::connect`) and subscribes.

That coupling has two consequences. First, rooms only work when every
participant can dial every other participant directly: NATs, headless
browsers, or low-bandwidth mobile peers get poor connectivity and
degrade the whole room. Second, the recent relay work in iroh-live
creates a first-class server that can fan out broadcasts to many
subscribers - the exact thing a room full of browser participants
needs - but rooms have no idea relays exist.

We also want rooms to support selective publish/subscribe, pull into
the relay, and eventually moderator capabilities. None of that fits
cleanly in the current actor.

### Goals

- Rooms work without requiring peer-to-peer connectivity when a relay
  is attached.
- Rooms work without a relay, using gossip-driven P2P today.
- Discovery is a pluggable layer: gossip now, relay-native announce
  table later, or both simultaneously (a peer that sees an announce
  via either source subscribes once).
- Broadcasts can be pulled into a relay on demand, either by the
  publisher "push" (publish via the relay, as our CLI already does)
  or by a subscriber requesting the relay to pull the broadcast
  (where policy permits).
- Capabilities travel with the room: a room's ticket carries a JWT
  scoped to its namespace, so joining peers can publish and subscribe
  without separate out-of-band keys.
- The actor stays small: discovery + dialing live in independent
  submodules with narrow interfaces so a future refactor does not
  touch the media decode path.

### Non-goals

- Changing the media catalog format.
- Replacing gossip globally.
- Changing how individual clients render video.

### Concepts

- `RoomDiscovery`: trait with two methods. `announce(name)` publishes a
  local broadcast's existence to the room. `announced()` returns a
  stream of `(EndpointId, name)` pairs. Implementations: `GossipKv`
  (existing behavior), `RelayAnnounce` (poll or subscribe to relay's
  announce table), and `Layered` (union of multiple).
- `RoomTransport`: trait with `connect(peer)` and
  `subscribe(peer, name)`. Implementations: `DirectP2p` (existing
  `Moq::connect`), `ViaRelay` (connect once to the relay over H3, then
  subscribe by name without ever dialing the peer).
- `Room` becomes a builder that picks a discovery and transport combo.
  The default remains `(GossipKv, DirectP2p)`; relay-backed rooms pick
  `(Layered(GossipKv, RelayAnnounce), ViaRelay)`.

### Ticket evolution

`RoomTicket` currently carries a topic id and bootstrap peers. We add
an optional `relay` block with:

- `endpoint`: iroh endpoint id of the relay.
- `path`: namespace inside the relay (equivalent to the `root` claim).
- `jwt`: a JWT with `publish`/`subscribe` caps for this path. Tickets
  carry read-only caps by default; publishers either present their own
  token acquired through svc, or the room creator generates a
  wildcard token and bakes it into the ticket (public room).

Backward compatibility: older clients ignore the relay block and fall
back to the gossip+P2P path.

### Relay-native announce table

Open question. Two approaches:

1. **Announce broadcast on the relay itself.** The relay already lists
   connected publishers internally (`Cluster::primary`). We expose a
   read-only subscribe that streams the origin's announcements out to
   authorized clients. Pro: avoids an extra service. Con: requires a
   moq-relay API extension.
2. **Announce via a dedicated KV under the relay's cluster prefix.**
   The room writes its broadcast names into a KV namespace the relay
   serves. Pro: fits the gossip mental model; reuses iroh-smol-kv.
   Con: two places to consult.

Plan: start with (2) because it keeps moq-relay unchanged. Re-evaluate
once the subscribe-to-origin API lands upstream.

### Pull semantics revisited

Rooms benefit from pull in two ways:

- **Ingest offload.** A mobile publisher connects once to the relay
  and leaves; the relay keeps the broadcast alive for room members.
- **Fan-in from private networks.** A peer that cannot accept
  connections publishes into the relay; subscribers inside the relay's
  namespace consume it.

For public rooms, wildcard pull (anyone in the room can ask the relay
to pull any ticket they present) is reasonable. For private rooms we
restrict pull to tokens with an `allow_pull` claim (currently tracked
per api key on the svc side; we need to pass it through the JWT).

### Adversarial review

- *"Why not just use rooms + bake the relay into every room?"* Some
  clients will always be able to reach each other directly; forcing
  traffic through a relay adds latency and breaks offline use. Keep
  P2P as the default.
- *"Why not subsume gossip entirely with the relay?"* Offline and
  censorship-resistance scenarios. Gossip remains the backbone; the
  relay is an optional accelerator.
- *"Does `RelayAnnounce` introduce message-ordering issues?"* Possibly:
  if the KV channel falls behind gossip, subscribers see a broadcast
  twice. `Layered` dedupes by `(remote, name)`.
- *"What about reconnects?"* `ViaRelay` reconnects to the relay on
  session loss. Each reconnect is a fresh H3 session (distinct URL ok
  because the JWT is reused). Broadcasts re-announce automatically.

### Commit strategy

1. `feat(iroh-live/rooms)`: extract `RoomDiscovery` and `RoomTransport`
   traits with the current gossip+P2P behavior as the initial impls.
2. `feat(iroh-live/rooms)`: add `RelayAnnounce` discovery backed by
   iroh-smol-kv on the relay's cluster prefix.
3. `feat(iroh-live/rooms)`: add `ViaRelay` transport using
   `Live::connect_relay`.
4. `feat(iroh-live/rooms)`: extend `RoomTicket` with the optional
   `relay` block.
5. `docs`: describe the three room modes (direct, relay-only, hybrid)
   in a short guide.
6. `feat(svc)`: extend the media-relay UI to mint room tickets; allow
   caps to be baked into the ticket's JWT.

### Cross-review notes (from adversarial spawn-a-reviewer exercise)

Reviewer notes to self for when the implementation starts:

- Make sure the `RoomDiscovery` trait does not leak lifetimes tied to
  the actor task. Owning a `Sender` instead of `&Actor` keeps it
  clean.
- Keep the existing `Room` public API stable. New consumers opt in by
  using the builder; old code paths get the gossip+P2P defaults.
- `ViaRelay`'s subscribe path must handle the "relay has not yet seen
  the broadcast" case. Moq's `announced` future does that already for
  us; confirm before relying on it.
- Write an integration test for a 3-peer room where one peer can only
  reach the relay and the other two can reach each other directly:
  all three should see all broadcasts.

### Open questions

- Does moq-relay already expose a "list primary origins" API for
  cluster peers? If so, we can piggyback on that instead of a separate
  KV. Confirm before choosing option (2) above.
- Should the ticket carry a signed capability (rcan) rather than a
  JWT? The moq auth layer only understands JWT today; adopting rcan
  inside tickets would require a translation layer at the relay.
  Defer until we have a concrete need.
- Rate-limiting: a bad actor with a room ticket can spam
  subscribe/publish. The relay's per-token rate limiter handles the
  network side; the room itself has no admission control yet.
