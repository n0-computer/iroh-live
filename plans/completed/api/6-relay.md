# Relay Support

How relay servers fit into the iroh-live / moq-media architecture. Covers both
direct broadcast relay and room-based relay, publishing to relays, and
subscribing from relays.

## Background: moq-relay

`moq-relay` (in `../moq/rs/moq-relay/`) is a content-agnostic MoQ relay server
that routes broadcasts between publishers and subscribers. Key capabilities:

- **Fan-out**: single publisher → N subscribers via `BroadcastConsumer` cloning
- **Caching**: in-memory group-based cache for late-join and dedup
- **Clustering**: mesh of relay nodes with root-based discovery (3-5 nodes)
- **Auth**: JWT-based path access control (`pub`, `sub`, `root` claims)
- **HTTP fallback**: `/fetch/*path` for group-level HTTP retrieval
- **Protocol**: standard MoQ Transport (IETF draft-17), QUIC-based

### How moq-relay works internally

The relay inverts the client/server perspective:

```
Client publishes → relay SUBSCRIBES to the client's tracks
Client subscribes → relay PUBLISHES its cached/forwarded tracks
```

Implemented via `OriginProducer`/`OriginConsumer` from moq-lite:

```rust
// connection.rs — the inversion
let session = self.request
    .with_publish(subscribe)   // we publish what clients can subscribe to
    .with_consume(publish)     // we consume what clients can publish
    .ok().await?;
```

Three origin pools in the cluster:
- `primary` — broadcasts from local clients (direct publishers)
- `secondary` — broadcasts from remote relay nodes (cluster peers)
- `combined` — merge of primary + secondary (what subscribers see)

### How MoqSession connects

In `iroh-moq`, `MoqSession` creates two `OriginProducer` instances:

```rust
// session_connect / session_accept
let publish_prod = OriginProducer::new();      // our outbound announcements
let subscribe_prod = OriginProducer::new();    // inbound announcements from remote
let subscribe = subscribe_prod.consume();      // consumer view of inbound

let client = moq_lite::Client::new()
    .with_publish(publish_prod.consume())      // remote sees our publishes
    .with_consume(subscribe_prod);             // we see remote's publishes
```

- `session.publish(name, broadcast)` → announces on `publish_prod`
- `session.subscribe(name)` → waits for announcement on `subscribe` consumer

This same mechanism works identically whether the remote is another peer or
a moq-relay server. The relay is just another MoQ endpoint.

---

## Architecture: Where Relay Fits

```
┌──────────────┐         ┌───────────┐         ┌──────────────┐
│  Publisher    │  QUIC   │ moq-relay │  QUIC   │  Subscriber  │
│  (iroh-live) │ ──────► │  server   │ ◄────── │  (iroh-live) │
│              │         │           │         │              │
│ Broadcast ──►│         │ fan-out + │         │◄── Subscription
│              │         │ caching   │         │              │
└──────────────┘         └───────────┘         └──────────────┘

Direct P2P (no relay):
┌──────────────┐  QUIC   ┌──────────────┐
│  Publisher    │ ──────► │  Subscriber  │
│              │         │              │
│ Broadcast ──►│         │◄── Subscription
└──────────────┘         └──────────────┘
```

The key insight: **from the API perspective, publishing to a relay and
publishing P2P use the exact same types.** A `Broadcast` doesn't care whether
its `BroadcastProducer` feeds a direct peer or a relay. A `Subscription`
doesn't care whether its `BroadcastConsumer` comes from a direct peer or a
relay.

The difference is only in **how the MoQ session is established**.

---

## Use Cases

### 1. Publish to relay for one-to-many distribution

A streamer publishes to a relay server. N viewers subscribe from the relay.
No direct P2P connections between publisher and viewers.

```rust
// Publisher connects to relay, publishes broadcast
let session = MoqSession::connect_url("https://relay.example.com/?jwt=TOKEN").await?;
let broadcast = Broadcast::new();
broadcast.video().set(camera, VideoCodec::H264, [P720, P360])?;
broadcast.audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;
session.publish("my-stream", broadcast.producer());
```

```rust
// Viewer connects to same relay, subscribes
let session = MoqSession::connect_url("https://relay.example.com/?jwt=TOKEN").await?;
let consumer = session.subscribe("my-stream").await?;
let sub = Subscription::new("my-stream", consumer).await?;
let video = sub.video()?;
```

### 2. Room via relay (SFU-like)

Room participants connect through a relay instead of forming a full mesh.
Reduces bandwidth: each participant uploads once to the relay, relay fans
out to all other participants.

```
Participant A ──► relay ──► Participant B
Participant B ──► relay ──► Participant A
Participant C ──► relay ──► Participant A, B
```

vs full mesh (current):
```
A ──► B, C
B ──► A, C
C ──► A, B
```

With N participants: mesh = N×(N-1) streams, relay = 2×N streams.

### 3. Hybrid: room gossip + relay transport

Gossip still handles peer discovery and room membership. But media transport
routes through a relay instead of direct P2P. This gives:
- Gossip's decentralized discovery (no central signaling server)
- Relay's efficient fan-out (no quadratic bandwidth)
- Relay's caching (late-join participants get current keyframe immediately)

### 4. Cascading relays (CDN)

Multiple relay nodes in different regions. Publisher → nearest relay →
cluster mesh → viewer's nearest relay → viewer.

```
Publisher (US-West) → usc.relay → euc.relay → Viewer (EU)
                                → sea.relay → Viewer (Asia)
```

### 5. Edge relay (local re-publish)

An iroh-live peer acts as a local relay by subscribing from a remote source
and re-publishing locally. This is the `Broadcast::relay()` pattern — pure
packet forwarding, no decode/re-encode.

```rust
let sub = live.subscribe(source_addr, "stream").await?;
let relay = Broadcast::relay(&sub)?;
live.publish("local-stream", relay.producer()).await?;
```

---

## API Design

### Connecting to a relay

The relay is a standard MoQ endpoint, accessible via URL (HTTPS/WebTransport).
iroh-moq currently only connects via iroh `EndpointAddr` (QUIC + iroh
addressing). To support relay servers, we need URL-based connection.

**Option A: Add URL connect to MoqSession**

```rust
impl MoqSession {
    /// Connects to a MoQ endpoint by URL (relay server).
    ///
    /// Uses WebTransport over HTTPS. The URL may include authentication
    /// as a query parameter (e.g., `?jwt=TOKEN`).
    pub async fn connect_url(url: impl Into<Url>) -> Result<Self, Error> { ... }

    /// Connects to a MoQ endpoint by iroh address (direct P2P).
    pub async fn connect(
        endpoint: &Endpoint,
        remote: impl Into<EndpointAddr>,
    ) -> Result<Self, Error> { ... }
}
```

**Option B: Add relay support to Live**

```rust
impl Live {
    /// Connects to a relay server for publishing and subscribing.
    ///
    /// Returns a session that works identically to a direct peer session.
    pub async fn connect_relay(&self, url: impl Into<Url>) -> Result<MoqSession> { ... }

    /// Publishes a broadcast through a relay server.
    pub async fn publish_to_relay(
        &self,
        relay_url: impl Into<Url>,
        name: &str,
        producer: BroadcastProducer,
    ) -> Result<()> { ... }

    /// Subscribes to a broadcast from a relay server.
    pub async fn subscribe_from_relay(
        &self,
        relay_url: impl Into<Url>,
        name: &str,
    ) -> Result<Subscription> { ... }
}
```

**Recommendation: Both.** Option A at the transport layer (iroh-moq),
Option B as convenience on Live. The relay is transparent to Broadcast /
Subscription — they don't know or care.

### Relay in Room context

Two approaches for relay-backed rooms:

**Approach 1: Room with relay transport**

The room uses gossip for membership but routes media through a relay.

```rust
let room = live.join_room(
    RoomTicket::generate(),
    RoomOptions::default()
        .relay(RelayConfig {
            url: "https://relay.example.com".parse()?,
            token: jwt_token,
        }),
).await?;

// Publishing and subscribing work exactly the same
room.local().broadcast().video().set(camera, codec, presets)?;
```

Internally, the room actor:
1. Connects to the relay on join
2. Publishes local broadcast to the relay (not directly to peers)
3. Subscribes to remote broadcasts from the relay (not from peers directly)
4. Gossip still handles membership announcements

This is conceptually an SFU (Selective Forwarding Unit) architecture using
a moq-relay as the SFU.

**Approach 2: Relay-only room (no gossip)**

For environments where gossip is unavailable (e.g., browser clients, firewall
restrictions), the relay IS the room coordination point.

```rust
// All participants connect to the same relay and use a shared namespace
let session = live.connect_relay("https://relay.example.com?jwt=ROOM_TOKEN").await?;

// Publish under room namespace
let broadcast = Broadcast::new();
broadcast.video().set(camera, codec, presets)?;
session.publish(format!("room/{room_id}/{my_id}"), broadcast.producer());

// Subscribe to other participants by discovering announcements
// (relay announces all broadcasts under the room/ prefix)
```

This requires relay-side announcement listing, which moq-relay already
supports via the HTTP `/announced/*prefix` endpoint and the MoQ announce
protocol.

**Recommendation: Start with Approach 1** (gossip + relay transport). It
composes with the existing room model cleanly. Approach 2 is a future
optimization for browser/restricted environments.

### RelayConfig type

```rust
/// Configuration for connecting to a moq-relay server.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RelayConfig {
    /// Relay server URL (HTTPS).
    pub url: Url,
    /// JWT authentication token. `None` for anonymous/public access.
    pub token: Option<String>,
}

impl RelayConfig {
    pub fn new(url: impl Into<Url>) -> Self {
        Self { url: url.into(), token: None }
    }
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into()); self
    }
}
```

### RoomOptions with relay

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RoomOptions {
    /// Optional relay server for media transport.
    ///
    /// When set, media is published to and subscribed from the relay
    /// instead of direct P2P connections. Gossip is still used for
    /// membership discovery.
    pub relay: Option<RelayConfig>,

    /// Auto-subscribe to all participants' media. Default: true.
    pub auto_subscribe: bool,
}

impl RoomOptions {
    pub fn relay(mut self, config: RelayConfig) -> Self {
        self.relay = Some(config); self
    }
    pub fn auto_subscribe(mut self, enabled: bool) -> Self {
        self.auto_subscribe = enabled; self
    }
}
```

---

## Implementation Plan

### Phase 1: URL-based MoqSession connect (iroh-moq)

Add WebTransport URL connect alongside existing iroh address connect.

**Dependency**: `moq-native` client library, which already supports URL-based
WebTransport connections. The relay cluster code uses it:

```rust
// moq-relay cluster.rs
let session = self.client.clone()
    .with_publish(self.primary.consume())
    .with_consume(self.secondary.clone())
    .connect(url.clone())
    .await?;
```

We need to either:
1. Add `moq-native` as a dependency to iroh-moq and use its `Client`, or
2. Implement WebTransport URL connect directly using `web-transport-iroh`

Option 1 is simpler. `moq-native::Client` handles WebTransport negotiation,
QUIC setup, TLS, and MoQ session establishment.

```rust
// iroh-moq additions
impl MoqSession {
    pub async fn connect_url(url: impl Into<Url>) -> Result<Self, Error> {
        let client = moq_native::Client::new();
        let publish_prod = OriginProducer::new();
        let subscribe_prod = OriginProducer::new();
        let subscribe = subscribe_prod.consume();

        let session = client
            .with_publish(publish_prod.consume())
            .with_consume(subscribe_prod)
            .connect(url.into())
            .await?;

        Ok(Self {
            publish: publish_prod,
            subscribe,
            // Note: wt_session needs adaptation since moq-native
            // returns a moq_lite::Session, not a web_transport_iroh::Session
            ..
        })
    }
}
```

**Challenge**: `MoqSession` currently stores a `web_transport_iroh::Session`
for connection stats and close handling. `moq-native::Client::connect()`
returns a `moq_lite::Session`. We need to either:
- Extract the underlying transport session from moq-native, or
- Store an enum `{ Iroh(web_transport_iroh::Session), Native(moq_lite::Session) }`
- Or restructure `MoqSession` to work with just `moq_lite::Session`

The cleanest approach: `MoqSession` stores `moq_lite::Session` + optional
connection handle for stats. The iroh-specific connection info is available
only for iroh-based sessions.

### Phase 2: Broadcast::relay() (moq-media)

Already planned in `4-impl.md` Phase 5. Zero-transcode packet forwarding.

Implementation detail: the forwarding does NOT use `forward_packets()` from
pipeline.rs (that's for decoded MediaPacket → decoder channel). Instead,
relay uses raw moq-lite track forwarding:

```rust
impl Broadcast {
    pub fn relay(subscription: &Subscription) -> Result<Self, PublishError> {
        let broadcast = Broadcast::new_raw(); // create without encoder setup

        // Copy catalog
        let catalog = subscription.catalog();
        broadcast.set_catalog(catalog);

        // For each track in catalog, spawn forwarding task
        for rendition in catalog.video_renditions().chain(catalog.audio_renditions()) {
            let consumer = subscription.raw_track_consumer(rendition)?;
            let producer = broadcast.raw_track_producer(rendition)?;
            tokio::spawn(async move {
                forward_track(consumer, producer).await;
            });
        }

        // Watch for catalog changes
        let catalog_watcher = subscription.catalog_watcher();
        tokio::spawn(async move {
            // When new renditions appear, spawn new forwarding tasks
            // When renditions disappear, drop forwarding tasks
        });

        Ok(broadcast)
    }
}

/// Forwards raw groups from a TrackConsumer to a TrackProducer.
///
/// No decoding or re-encoding. Packets are forwarded verbatim.
async fn forward_track(
    mut consumer: OrderedConsumer,  // from moq-lite via hang
    producer: TrackProducer,        // from moq-lite
) {
    while let Ok(Some(group)) = consumer.next_group().await {
        let mut group_producer = producer.create_group(group.sequence);
        while let Ok(Some(frame)) = group.read_frame().await {
            group_producer.write_frame(frame.payload).ok();
        }
    }
}
```

Key: raw track consumer/producer access needs to be exposed on Subscription
and Broadcast. Currently `SubscribeBroadcast` has `self.broadcast` (a
`BroadcastConsumer`) which provides `subscribe_track()`. Similarly,
`PublishBroadcast` has `self.producer` (a `BroadcastProducer`) which provides
`insert_track()` / `create_track()`.

Needed additions to `Subscription`:
```rust
impl Subscription {
    /// Returns the underlying BroadcastConsumer for raw track access.
    ///
    /// Advanced API for relay and custom forwarding. Most users should
    /// use `video()` / `audio()` instead.
    pub fn raw_consumer(&self) -> &BroadcastConsumer { ... }
}
```

Needed additions to `Broadcast`:
```rust
impl Broadcast {
    /// Returns the underlying BroadcastProducer for raw track insertion.
    ///
    /// Advanced API for relay. Most users should use `video().set()` instead.
    pub fn raw_producer(&self) -> &BroadcastProducer { ... }
}
```

### Phase 3: Live relay convenience methods (iroh-live)

```rust
impl Live {
    /// Connects to a moq-relay server.
    pub async fn connect_relay(&self, config: RelayConfig) -> Result<MoqSession> {
        let mut url = config.url;
        if let Some(token) = config.token {
            url.query_pairs_mut().append_pair("jwt", &token);
        }
        MoqSession::connect_url(url).await
    }

    /// Publishes a broadcast to a relay.
    ///
    /// Convenience for `connect_relay()` + `session.publish()`.
    pub async fn publish_to_relay(
        &self,
        config: RelayConfig,
        name: &str,
        producer: BroadcastProducer,
    ) -> Result<MoqSession> {
        let session = self.connect_relay(config).await?;
        session.publish(name.to_string(), producer);
        Ok(session) // caller keeps session alive
    }

    /// Subscribes to a broadcast from a relay.
    ///
    /// Convenience for `connect_relay()` + `session.subscribe()` + `Subscription::new()`.
    pub async fn subscribe_from_relay(
        &self,
        config: RelayConfig,
        name: &str,
    ) -> Result<(MoqSession, Subscription)> {
        let mut session = self.connect_relay(config).await?;
        let consumer = session.subscribe(name).await?;
        let sub = Subscription::new(name, consumer).await?;
        Ok((session, sub))
    }
}
```

### Phase 4: Room relay transport (iroh-live)

Modify the room actor to optionally route media through a relay.

**Changes to room actor:**

```rust
struct Actor {
    // ...existing fields...
    relay_session: Option<MoqSession>,  // NEW: relay connection
    relay_config: Option<RelayConfig>,  // NEW: relay config
}
```

When `relay_config` is set:

1. **On room join**: connect to relay via `MoqSession::connect_url()`
2. **On local publish**: publish to relay session instead of peer sessions
   ```rust
   // Current (P2P):
   self.live.publish(name, producer).await?;
   // With relay:
   self.relay_session.publish(name, producer.consume());
   ```
3. **On remote announced**: subscribe from relay instead of connecting to peer
   ```rust
   // Current (P2P):
   let session = live.connect_and_subscribe(remote, &name).await;
   // With relay:
   let consumer = self.relay_session.subscribe(&name).await?;
   let broadcast = Subscription::new(&name, consumer).await?;
   ```
4. **Gossip unchanged**: still used for membership and broadcast announcements
5. **Fallback**: if relay connection fails, can optionally fall back to P2P

**Broadcast naming in relay context:**

When using a relay, broadcasts need unique names to avoid collisions between
participants. Convention: `{room_id}/{participant_id}/{broadcast_name}`.

The relay's path-based auth (JWT `pub`/`sub` claims) can restrict each
participant to their own namespace:
- Participant A gets JWT with `pub: ["room123/alice/*"]`, `sub: ["room123/*"]`
- Participant B gets JWT with `pub: ["room123/bob/*"]`, `sub: ["room123/*"]`

This is how moq-relay already works — no changes needed on the relay side.

### Phase 5: Relay discovery and selection (future)

Advanced features for production deployments:

1. **GeoDNS-based relay selection**: client resolves `relay.example.com` →
   nearest relay node via DNS
2. **Relay health monitoring**: periodic RTT/bandwidth probes to available
   relays, auto-switch to better relay
3. **Multi-relay failover**: if primary relay goes down, reconnect to backup
4. **Relay-assisted NAT traversal**: relay as TURN-like fallback when direct
   QUIC fails

These are deployment/operations features, not API changes.

---

## Transport Layer Considerations

### moq-native dependency

`moq-native` (in `../moq/rs/moq-native/`) provides the WebTransport client
that moq-relay uses. It handles:
- TLS certificate validation (including self-signed via fingerprint)
- QUIC/WebTransport negotiation
- HTTP/3 fallback
- MoQ session establishment

Adding `moq-native` as a dependency to iroh-moq gives us relay connectivity
for free. However:
- `moq-native` depends on `quinn` directly (not `iroh`)
- It uses `web-transport-quinn` (not `web-transport-iroh`)
- The session type differs

**Options:**
1. **Add moq-native dep**: simplest, but two QUIC stacks in process
2. **Port moq-native's URL connect to web-transport-iroh**: more work,
   cleaner dependency tree
3. **Separate relay-client crate**: isolate the moq-native dependency

**Recommendation**: Start with option 1 for rapid iteration. The two QUIC
stacks (quinn direct + iroh/quinn) coexist fine — iroh already uses quinn
internally. Optimize to option 2 later if the dependency weight matters.

### Session type unification

`MoqSession` currently stores `web_transport_iroh::Session` for connection
stats. Relay connections via moq-native don't have this type. Solutions:

```rust
pub struct MoqSession {
    _moq_session: Arc<moq_lite::Session>,
    publish: OriginProducer,
    subscribe: OriginConsumer,
    // Replace concrete type with trait or enum:
    connection: SessionConnection,
}

enum SessionConnection {
    /// Direct iroh connection (P2P or relay via iroh endpoint)
    Iroh {
        wt_session: web_transport_iroh::Session,
    },
    /// Native WebTransport connection (relay via URL)
    Native {
        // Limited stats available
    },
}

impl MoqSession {
    /// Returns the iroh connection, if this is an iroh-based session.
    ///
    /// Returns `None` for relay URL connections.
    pub fn iroh_conn(&self) -> Option<&Connection> { ... }

    /// Returns the remote endpoint ID, if known.
    pub fn remote_id(&self) -> Option<EndpointId> { ... }
}
```

---

## Examples

### One-to-many via relay

```rust
use iroh_live::prelude::*;

// Publisher
async fn publish_via_relay(live: Live) -> Result<()> {
    let relay = RelayConfig::new("https://relay.example.com")
        .token("publisher-jwt-token");

    let broadcast = Broadcast::new();
    broadcast.video().set(camera, VideoCodec::H264, [P720])?;
    broadcast.audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    let session = live.publish_to_relay(relay, "concert", broadcast.producer()).await?;

    // Keep alive until done
    tokio::signal::ctrl_c().await?;
    Ok(())
}

// Subscriber
async fn subscribe_via_relay(live: Live) -> Result<()> {
    let relay = RelayConfig::new("https://relay.example.com")
        .token("subscriber-jwt-token");

    let (_session, sub) = live.subscribe_from_relay(relay, "concert").await?;
    let video = sub.video()?;
    let audio = sub.audio(&audio_backend).await?;

    loop {
        if let Some(frame) = video.current_frame() {
            render(frame);
        }
    }
}
```

### Room via relay

```rust
use iroh_live::prelude::*;

async fn room_via_relay(live: Live, ticket: RoomTicket) -> Result<()> {
    let room = live.join_room(
        ticket,
        RoomOptions::default().relay(
            RelayConfig::new("https://relay.example.com")
                .token("room-jwt-token"),
        ),
    ).await?;

    // Identical API — relay is transparent
    room.local().broadcast().video().set(camera, codec, presets)?;

    use futures::StreamExt;
    let mut events = room.events();
    while let Some(event) = events.next().await {
        match event {
            RoomEvent::ParticipantJoined(p) => {
                let video = p.subscription().video()?;
                // ...
            }
            _ => {}
        }
    }

    Ok(())
}
```

### Edge relay (local re-broadcast)

```rust
// Subscribe from relay, re-publish locally
async fn edge_relay(live: Live) -> Result<()> {
    let relay = RelayConfig::new("https://cdn.example.com")
        .token("edge-jwt-token");

    let (_session, sub) = live.subscribe_from_relay(relay, "live-event").await?;

    // Zero-transcode local re-broadcast
    let local_relay = Broadcast::relay(&sub)?;
    live.publish("local-live-event", local_relay.producer()).await?;

    sub.closed().await;
    Ok(())
}
```

### Cascading relay chain

```rust
// Relay A subscribes from source, publishes to relay B
async fn cascade(live: Live) -> Result<()> {
    // Subscribe from source relay
    let source = RelayConfig::new("https://origin.example.com").token("source-token");
    let (_session, sub) = live.subscribe_from_relay(source, "stream").await?;

    // Forward to edge relay
    let edge = RelayConfig::new("https://edge.example.com").token("edge-token");
    let relay_broadcast = Broadcast::relay(&sub)?;
    let _edge_session = live.publish_to_relay(edge, "stream", relay_broadcast.producer()).await?;

    sub.closed().await;
    Ok(())
}
```

---

## Bandwidth comparison

| Topology | Upload per participant | Total streams |
|---|---|---|
| Full mesh (N peers) | N-1 streams | N × (N-1) |
| Relay (SFU) | 1 stream to relay | 2 × N |
| Cascading relay | 1 stream to nearest relay | 2 × N + inter-relay |

For a 5-person room:
- Full mesh: 20 streams, each participant uploads 4×
- Relay: 10 streams, each participant uploads 1×

The relay wins at N ≥ 3. For 2 participants, P2P is simpler.

---

## Open Questions

1. **Should `join_room()` accept `RoomOptions`?** Currently it only takes a
   ticket. Adding options changes the signature. Could instead use
   `live.join_room_with(ticket, options)` or have `RoomTicket` carry a
   relay URL.

2. **Auth token management.** Who generates the JWT? For self-hosted relays,
   the app server. For managed relays, a token service. The API should be
   token-agnostic — accept a string, don't prescribe how it's obtained.

3. **Mixed relay + P2P rooms.** Can some participants use the relay while
   others connect P2P? This is technically possible (both paths result in
   the same MoQ protocol) but complicates the room actor. Defer to future.

4. **Relay reconnection.** If the relay connection drops, should the room
   auto-reconnect? Yes — with exponential backoff, same as moq-relay's
   `run_remote()` pattern. The `Subscription` and `Broadcast` handles
   should survive reconnection (re-announce, re-subscribe).

5. **moq-native vs web-transport-iroh for relay connect.** The cleanest
   long-term solution is adding WebTransport URL connect to
   `web-transport-iroh` directly, so iroh-moq doesn't need moq-native.
   But moq-native already works and is battle-tested. Start pragmatic.

6. **Should `Broadcast::relay()` work across relay hops?** Yes — the relay
   broadcast is just another `BroadcastProducer`. It can be published to
   any session, including a relay session. The relay doesn't know it's
   receiving relayed content.
