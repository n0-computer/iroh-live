# API Review: Public APIs in `iroh-live`

This review is about API structure and ergonomics, not docs quality.
The target is maximally-ergonomic Rust APIs for:

- one-to-one live video/audio
- manual processing / custom setup
- simple RTC-style rooms

I reviewed:

- `iroh-live`
- `moq-media`
- `iroh-moq`
- examples in `iroh-live/examples`
- usage in `../iroh-live-apps`
- LiveKit JS/TS SDK shape for comparison and inspiration

## Executive Summary

> **[Note]:** This section is well-written. Adding a few thoughts on the Rust-specific dimension of "product-first" APIs below.

The current stack has strong low-level building blocks, but the public API is not yet shaped around the way application developers think about real-time media.

The main issue is that the exposed model is transport-first, not product-first.
Users have to reason about:

- `MoqSession`
- `BroadcastProducer` / `BroadcastConsumer`
- `SubscribeBroadcast`
- track names and catalog entries
- actor split handles
- separate "camera broadcast" vs "screen broadcast"

before they can express simple intents like:

- "start a call"
- "accept an incoming call"
- "join a room"
- "publish my camera and microphone"
- "subscribe to the remote participant's camera"

LiveKit's JS/TS SDK is a useful contrast: it exposes a small stable object model centered on `Room`, `Participant`, `TrackPublication`, and events. Transport details exist, but they are not the first thing an app has to compose.

The local Hang JS API in `../moq/js` is also an important reference, but for a different reason. It shows that a broadcast-centered API can still be ergonomic if it exposes:

- one cohesive `Broadcast` object instead of raw producers/consumers
- explicit status/catalog state
- structured subcomponents (`video`, `audio`, `chat`, `location`, `preview`)
- declarative selection inputs (`target`, `supported`)
- internal lifecycle management instead of pushing it to the app

My assessment:

- `moq-media` is a strong systems layer.
- `iroh-moq` is a usable transport layer, but too directly exposed.
- `iroh-live` does not yet provide a sufficiently opinionated product layer.
- the current room API is closer to "automatic gossip subscription of named broadcasts" than to a real RTC room abstraction.
- the one-to-one story is missing a first-class incoming/accepting model.

> **[Note]:** One dimension not mentioned: the **error story is inconsistent across layers**. `moq-media` uses `anyhow::Result` throughout (publish.rs, subscribe.rs, pipeline.rs), while `iroh-moq` uses `n0_error::stack_error!` for structured errors (`Error`, `SubscribeError`, `ConnectError`). The product layer (`iroh-live`) inherits both. A clean three-layer API needs a coherent error strategy:
>
> - Product layer: small, actionable error enums (e.g. `CallError`, `RoomError`, `PublishError`)
> - Broadcast layer: domain-specific errors (codec failure, catalog unavailable, rendition not found)
> - Raw layer: transport errors (as today)
>
> The current `PublishUpdateError` in `moq-media/src/publish/controller.rs:78-98` is actually a good pattern — it collects per-stream `(StreamKind, AnyError)` failures for partial success reporting. That pattern should be generalized: multi-rendition publish can partially succeed, and the API should express that.

The right direction is not to remove low-level control. It is to add a better top-level API that:

- names things around participants, publications, and tracks
- makes the simple path obvious
- still allows dropping down to manual transport/media APIs when needed

> codex: This is the center of the whole review. The repo does not need more transport helpers first. It needs a better default object model, with transport and media power preserved below that surface.

## Scope Reviewed

### Top-level public surface

- `iroh-live/src/lib.rs`
- `iroh-live/src/live.rs`
- `iroh-live/src/node.rs`
- `iroh-live/src/rooms.rs`
- `iroh-live/src/ticket.rs`
- `moq-media/src/lib.rs`
- `moq-media/src/publish.rs`
- `moq-media/src/publish/controller.rs`
- `moq-media/src/subscribe.rs`
- `moq-media/src/audio_backend.rs`
- `iroh-moq/src/lib.rs`

### Usage reviewed

- `iroh-live/examples/publish.rs`
- `iroh-live/examples/watch.rs`
- `iroh-live/examples/rooms.rs`
- `iroh-live/examples/push.rs`
- `iroh-live/examples/room-publish-file.rs`
- `../iroh-live-apps/iroh-live-gpui/src/views/room.rs`
- `../iroh-live-apps/iroh-live-gpui/src/main.rs`

### External reference

LiveKit JS/TS SDK docs and examples:

- https://docs.livekit.io/home/client/connect/
- https://docs.livekit.io/home/client/tracks/subscribe/
- https://docs.livekit.io/reference/client-sdk-js/classes/Room.html
- https://docs.livekit.io/reference/client-sdk-js/classes/LocalParticipant.html
- https://docs.livekit.io/reference/client-sdk-js/classes/RemoteTrackPublication.html

Hang JS and related local APIs:

- `../moq/js/hang/src/index.ts`
- `../moq/js/watch/src/broadcast.ts`
- `../moq/js/publish/src/broadcast.ts`
- `../moq/js/watch/src/video/source.ts`
- `../moq/js/watch/src/audio/source.ts`
- `../moq/js/publish/src/video/index.ts`
- `../moq/js/publish/src/audio/encoder.ts`

## What Is Good Today

### 1. The low-level pieces are real, not fake

This repo already has good foundational pieces:

- explicit media sources/sinks/encoders/decoders in `moq-media`
- usable transport independence in the pipelines
- local preview support via `PublishBroadcast::watch_local`
- direct access to manual encode/decode pipelines
- usable audio backend abstraction

This is valuable. The redesign should preserve it.

> **[Note]:** Two more strengths worth calling out explicitly:
>
> - **Drop-based cleanup is already the norm.** `PipelineGuard` (moq-media/src/pipeline.rs:113-121) bundles `DropGuard` + `AbortOnDropHandle` + `JoinHandle` so pipelines clean up automatically. `AbortOnDropHandle` is used consistently for async task lifetimes (rooms.rs, subscribe.rs, publish.rs). This is excellent idiomatic Rust — resources are tied to ownership, no manual `close()` calls needed. The new API should preserve this property universally: dropping a `Call` should clean up its session, dropping a `Room` should leave it, dropping a `LocalTrackPublication` should unpublish.
>
> - **The `Watchable`/`Watcher` pattern** (from `n0_watcher`) is already used for reactive state in `SubscribeBroadcast` (catalog changes), `VideoDecoderHandle` (viewport), and `PublishCaptureController` (capture opts). This is a better fit than tokio watch channels for the "current state + change notification" pattern that a product API needs. The new `Participant`, `Publication`, and `Room` types should use `Watchable` internally for state that UIs need to observe (connection status, available renditions, active speaker, etc.).

### 2. `moq-media` already separates media from transport better than the top-level crate does

`VideoEncoderPipeline`, `VideoDecoderPipeline`, `AudioEncoderPipeline`, `AudioDecoderPipeline`, `VideoSource`, `AudioSource`, `PacketSink`, and `PacketSource` are the right kind of primitives for advanced users.

### 3. Tickets are conceptually useful

`LiveTicket` and `RoomTicket` are the right general idea: a small portable join handle.

> **[Note]:** Tickets should implement `Display` + `FromStr` (for copy-paste in terminals/chat), `Serialize`/`Deserialize` (for storage/APIs), and possibly `Into<Url>` for deep-linking. They're one of the few types that genuinely benefit from maximum conversion ergonomics.

## Main Problems

## 1. The public model is transport-shaped instead of user-shaped

Examples:

- `Live::connect()` returns `MoqSession` directly (`iroh-live/src/live.rs:24`)
- `Live::connect_and_subscribe()` returns `(MoqSession, SubscribeBroadcast)` (`iroh-live/src/live.rs:28`)
- `Live::watch_and_listen()` returns `(MoqSession, AvRemoteTrack)` (`iroh-live/src/live.rs:40`)
- `RoomEvent::BroadcastSubscribed` exposes `session` plus `broadcast` (`iroh-live/src/rooms.rs:137`)

This pushes transport and catalog concepts directly into app code. The application has to assemble its own participant model out of raw session objects and named broadcasts.

That is the wrong default shape for:

- calls
- rooms
- "accept incoming" flows

## 2. `Live` leaks internals and lacks a coherent top-level opinion

`Live` is currently:

- a thin wrapper around `Moq`
- with `pub moq: Moq` exposed (`iroh-live/src/live.rs:13`)
- plus a few convenience methods layered on top

This creates a confused boundary:

- is `Live` the intended product API?
- or is it just a transport helper?
- should users call `live.protocol_handler()`, `live.moq.protocol_handler()`, or build `Router` themselves?

Today, the answer is effectively "all of the above".
That is not an ergonomic public surface.

> **[Note]:** A concrete Rust idiom for solving this: **the "onion" pattern** where each layer wraps the inner and exposes a method to unwrap. Like `hyper::Response` wrapping `http::Response`, or how `reqwest::Client` wraps `hyper::Client` but exposes `.inner()` or a From impl for escape hatching. Applied here:
>
> ```rust
> impl Live {
>     /// Access the underlying MoQ transport layer.
>     ///
>     /// This is an escape hatch for advanced use cases that need direct
>     /// transport control. Most applications should not need this.
>     pub fn transport(&self) -> &Moq { &self.moq }
> }
> ```
>
> This is better than `pub moq` because: (1) the field name can change without breaking users, (2) docs can discourage casual use, (3) you can add validation or lazy init later, (4) clippy won't warn about unused pub fields.

## 3. The one-to-one API has no first-class incoming/accepting model

This is the biggest product gap.

Current state:

- outbound exists: `Live::connect`, `Moq::connect`, `MoqSession::connect`
- inbound is hidden behind the protocol handler actor in `iroh-moq`
- the accepted side is stuffed into internal session maps in `iroh-moq`
- there is no public stream of incoming peers / calls / sessions

This means the library does not currently expose enough to support:

- "accept the next incoming call"
- "show incoming call UI, then accept/reject"
- "inspect who is calling before subscribing"
- "handle multiple incoming direct peers sanely"

This is exactly the issue the user called out, and I agree with it.

> codex: I would treat incoming accept/reject support as a hard requirement for calling the one-to-one API "complete." Without it, the callee side is an afterthought.

> **[Note]:** Looking at the actual protocol handler code (`iroh-moq/src/lib.rs:135-162`), `MoqProtocolHandler::handle_connection` immediately calls `MoqSession::session_accept()` and sends the fully-accepted session to the actor. There is no point where app code can intercept. The fix needs to happen at two levels:
>
> 1. **Transport level**: split `handle_connection` into "receive connection metadata" + "accept/reject". The QUIC handshake gives you `remote_id()` before accepting the WebTransport session.
> 2. **Product level**: `IncomingCall` should be a **type-state** type that enforces the accept/reject flow at compile time:
>
> ```rust
> /// A pending incoming call that must be accepted or rejected.
> /// Dropping without accepting or rejecting will reject automatically.
> pub struct IncomingCall { /* ... */ }
>
> impl IncomingCall {
>     pub fn remote_id(&self) -> EndpointId { /* ... */ }
>     pub async fn accept(self) -> Result<Call> { /* consumes self */ }
>     pub async fn reject(self, reason: RejectReason) -> Result<()> { /* consumes self */ }
> }
>
> impl Drop for IncomingCall {
>     fn drop(&mut self) { /* send reject if not yet handled */ }
> }
> ```
>
> The `self`-consuming accept/reject is idiomatic Rust: it makes double-accept impossible at the type level. The auto-reject on drop prevents leaked pending calls.

## 4. `Room` is not yet a real room object model

`Room` today is a gossip-driven auto-subscriber over named broadcasts:

- peer state is just `broadcasts: Vec<String>` (`iroh-live/src/rooms.rs:145`)
- subscription behavior is "when remote announces a broadcast name, connect and subscribe"
- events are `RemoteAnnounced`, `RemoteConnected`, `BroadcastSubscribed`

Problems:

- there is no participant abstraction
- there is no local participant abstraction
- there is no publication abstraction
- there is no stable identity for a logical track/publication beyond a string name
- room policy is embedded in actor behavior rather than represented as public concepts

This forces apps to reconstruct their own higher-level model. The GPUI app is effectively doing that.

## 5. Naming is inconsistent across layers

Examples:

- `PublishBroadcast` vs `SubscribeBroadcast`
- `WatchTrack` vs `AudioTrack` vs `AvRemoteTrack`
- `PublishOpts` controlling local capture state
- `RoomPublisherSync` doing async publication by spawning tasks
- `broadcast_name` at the top level, but track/rendition names below that
- `watch`, `listen`, `watch_and_listen`, `connect_and_subscribe`

The API mixes:

- transport terms: session, broadcast, consumer, producer
- media terms: audio, video, track, rendition
- app terms: room, ticket

without a consistent domain model.

> **[Note]:** The naming inconsistency also extends to **method verb conventions**. Idiomatic Rust has strong conventions here (per the API guidelines):
>
> - `new` / `with_*` for construction
> - `get_*` only when there's a corresponding `set_*` (otherwise just the noun: `.catalog()` not `.get_catalog()`)
> - `into_*` for consuming conversions, `as_*` for borrowed views, `to_*` for expensive conversions
> - `is_*` / `has_*` for boolean queries
> - `try_*` for fallible variants of infallible methods
>
> The current API partially follows this but not consistently. For example:
> - `watch_local()` should be `preview()` or `local_preview()` (noun, not verb)
> - `watch_and_listen()` should be `subscribe()` or `subscribe_all()` (standard verb)
> - `set_opts()` returning a `BroadcastProducer` violates the convention that setters return `()` or `&mut Self`
> - `camera_producer()` exposes an implementation detail in its name

## 6. The current API over-indexes on "broadcast" as the unit users should think in

A broadcast is not the right default top-level concept for rooms/calls, but it is a valid mid-level concept.
For top-level app authors, the meaningful objects are:

- participant
- published source
- subscription
- camera
- screen share
- microphone

Not:

- a "camera broadcast" with an embedded audio track
- plus an optional separate "screen broadcast"

The current `PublishCaptureController` explicitly creates:

- one eager camera+audio broadcast
- one lazy screen broadcast

That is practical internally, but the public API should not force users to think in those terms.

After looking at Hang JS, I would refine the recommendation here:

- do not remove broadcast as a public concept entirely
- do remove raw `BroadcastProducer` / `BroadcastConsumer` from the normal path
- add a proper mid-level `LocalBroadcast` / `RemoteBroadcast` object layer

Hang's `Broadcast` classes are a useful example:

- watch-side `Broadcast` owns `enabled`, `name`, `status`, `reload`, `active`, and `catalog` (`../moq/js/watch/src/broadcast.ts:6`)
- publish-side `Broadcast` owns `audio`, `video`, `chat`, `location`, `preview`, and serves tracks internally (`../moq/js/publish/src/broadcast.ts:11`)

That is substantially more ergonomic than the Rust API's current exposure of raw producers/consumers plus helper wrappers.

> codex: This is the most important correction from reviewing Hang JS. Broadcast is not the wrong abstraction. Unstructured raw transport primitives are. A good broadcast layer is the bridge between product APIs and MoQ internals.

## 7. `PublishCaptureController` is helpful but not yet product-grade

What is good:

- it is transport-agnostic
- it centralizes device/codec selection

What is weak:

- `PublishOpts` is too UI-toggle-shaped and too specific (`camera`, `screen`, `audio`)
- `set_opts()` returns a transport detail, `new_screen: Option<BroadcastProducer>` (`moq-media/src/publish/controller.rs:69`)
- `camera_producer()` and `screen_broadcast()` expose the internal "main broadcast vs screen broadcast" split
- the type name `PublishCaptureController` is implementation-oriented, not task-oriented

This should become a capture/publish manager with stable local publications, not a controller that sometimes hands back producers.

## 8. The subscription API is powerful but awkward as a top-level API

`SubscribeBroadcast` currently owns:

- catalog watching
- video subscription selection
- audio subscription selection
- shutdown lifecycle

and offers:

- `watch()`
- `watch_with()`
- `watch_rendition()`
- `listen()`
- `listen_with()`
- `listen_rendition()`
- `watch_and_listen()`

Issues:

- `watch` is video-only jargon
- `listen` is audio-only jargon
- `watch_and_listen` is cute but not composable
- `AvRemoteTrack` is a bundling type rather than a model type
- quality-based selection is hidden at the broadcast layer, not exposed as track subscription policy

This wants to become a publication/subscription API.

## 9. `RoomPublisherSync` is a smell

`RoomPublisherSync`:

- is named "Sync" but spawns async tasks (`iroh-live/src/rooms/publisher.rs:34`, `:62`)
- exists mainly to glue `PublishCaptureController` into room transport
- hardcodes two broadcast names: `camera`, `screen`

This is not the kind of type that should survive in a polished public API.

## 10. App code is compensating for the library API instead of merely using it

The sibling app shows this clearly:

- it builds its own remote tile and peer model
- it reacts to `RoomEvent::BroadcastSubscribed` and then constructs playback on top
- it recreates audio backends around device changes
- it has library/API drift, which is another sign the public surface is still moving underneath it

That is normal during early development, but it confirms the current public API is not yet the final shape.

## Design Principles For The New API

## 1. Product-first top layer, systems-first lower layers

Three layers should exist clearly:

1. `iroh_live` top-level product API
2. `iroh_live::media` advanced media composition
3. `iroh_live::moq` / lower transport internals

The simple path should stay almost entirely in layer 1.

> **[Note]:** This layering maps naturally to Rust's **feature flag** system. Consider:
>
> ```toml
> [features]
> default = ["rooms", "calls"]
> rooms = []           # Room + Participant API
> calls = []           # Call + IncomingCall API
> broadcast = []       # LocalBroadcast + RemoteBroadcast (mid-level)
> transport = []       # Raw MoqSession, BroadcastProducer/Consumer
> ```
>
> Feature flags serve double duty: they gate compilation (smaller binaries for embedded/WASM) and they signal intent ("I'm an advanced user who needs transport access"). This is the pattern `hyper` uses (client/server features) and `tower` uses (per-middleware features).
>
> However: do not over-gate. The main decision should be whether the *types compile in*, not whether they're accessible. A simpler approach is just module visibility + `#[doc(hidden)]` for internal-but-accessible types, with feature flags only for heavy optional dependencies (wgpu, vaapi, etc.).

## 2. Stable object model

Top-level users should mostly interact with:

- `Live`
- `Call`
- `IncomingCall`
- `Room`
- `Participant`
- `LocalParticipant`
- `RemoteParticipant`
- `TrackPublication`
- `RemoteTrack`
- `LocalTrack`

> **[Note]:** All of these types should be **cheaply cloneable handles** (i.e. `Arc`-based internally), not owned values. This is critical for Rust ergonomics in async/UI code. The pattern:
>
> ```rust
> #[derive(Clone)]
> pub struct Room {
>     inner: Arc<RoomInner>,
> }
> ```
>
> This is already the pattern used by `RoomHandle` (which wraps `Arc<AbortOnDropHandle>`), but the naming doesn't communicate it. When users see a non-Clone struct, they assume they need to manage ownership carefully. Clone handles let users freely pass `Room` to multiple async tasks, UI callbacks, etc. This is how `reqwest::Client`, `tonic::Channel`, and `iroh::Endpoint` all work.
>
> Corollary: types like `Call` and `Room` should NOT implement `Drop` with side effects on the handle clone — only the last clone's drop (i.e. the Arc's drop) should trigger cleanup. The current `AbortOnDropHandle` pattern already gets this right.

## 3. Publish/subscribe should be explicit, but not transport-flavored

Good user verbs:

- `publish_camera`
- `publish_microphone`
- `publish_screen`
- `unpublish`
- `subscribe`
- `unsubscribe`
- `set_enabled`
- `set_quality`

Avoid exposing `BroadcastProducer` and `BroadcastConsumer` in the default path.

## 4. Manual processing must remain first-class

There should still be a strong manual path for:

- custom sources
- custom sinks
- custom encoders/decoders
- manual frame processing
- transport bypass / direct track pipes

But that should live behind explicit advanced APIs.

> **[Note]:** The current trait design for extensibility is good but could be more ergonomic. Specific observations:
>
> **Trait objects vs generics**: The codebase uses both `Box<dyn VideoSource>` (publish.rs) and generic `D: VideoDecoder` (subscribe.rs). The inconsistency is understandable — sources are stored (need dyn), decoders are constructed (need generic for type-specific config). But this creates a confusing surface. Recommendation: **use trait objects at API boundaries, generics for construction**:
>
> ```rust
> // Construction uses generics (zero-cost, type-checked config)
> let track = publication.subscribe_video::<VaapiH264Decoder>(opts).await?;
>
> // Storage uses trait objects (uniform collections)
> let tracks: Vec<RemoteVideoTrack> = ...; // internally Box<dyn VideoDecoder>
> ```
>
> **The `Decoders` associated-type trait** (`moq-media/src/traits.rs`) is clever for bundling audio+video decoder selection, but `DefaultDecoders` as a type parameter is verbose. Consider a codec-selection builder instead:
>
> ```rust
> // Current (generic, verbose)
> broadcast.watch_and_listen::<DefaultDecoders>(&audio, config).await?;
>
> // Proposed (builder, discoverable)
> broadcast.subscribe()
>     .video(DecoderBackend::Auto)
>     .audio(DecoderBackend::Auto)
>     .build().await?;
> ```
>
> **Sealed + extension trait pattern**: For traits that users implement (VideoSource, AudioSource), consider a sealed internal trait for the parts that must not change, and a public extension trait for the user-facing API:
>
> ```rust
> // User implements this (stable)
> pub trait VideoSource: Send + 'static {
>     fn format(&self) -> VideoFormat;
>     fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
> }
>
> // Library provides this (can evolve without breaking impls)
> pub trait VideoSourceExt: VideoSource {
>     fn into_boxed(self) -> Box<dyn VideoSource> where Self: Sized {
>         Box::new(self)
>     }
> }
> impl<T: VideoSource> VideoSourceExt for T {}
> ```

## 5. Accepting inbound peers must be a first-class concept

For one-to-one, a user needs:

- a listener/acceptor stream
- metadata about the remote
- accept/reject ability
- access to announced publications after accept
- a consistent session lifecycle

> **[Note]:** The acceptor stream should be a proper `Stream` impl, not a custom `recv()` method. This enables composition with `StreamExt` combinators:
>
> ```rust
> use futures::StreamExt;
>
> // Filter, map, buffer — all for free
> let mut calls = live.incoming_calls()
>     .filter(|c| future::ready(allowlist.contains(&c.remote_id())))
>     .take(1); // only accept one call
>
> if let Some(incoming) = calls.next().await {
>     let call = incoming.accept().await?;
> }
> ```
>
> The current `Room` uses `mpsc::Receiver<RoomEvent>` internally but wraps it in custom `recv()`/`try_recv()` methods. That's fine for now, but the new API should expose `impl Stream<Item = RoomEvent>` (or a named stream type) so users get the full `StreamExt` ecosystem. The `async_stream` crate or `futures::stream::unfold` can bridge from mpsc easily.

## What To Copy From LiveKit JS/TS

Not the exact transport architecture. The object model.

LiveKit gets several things right:

- `connect()` yields a `Room`
- local publishing hangs off `room.localParticipant`
- remote activity arrives as participant/track events
- subscriptions are tied to publications
- publication objects exist separately from currently attached media
- incoming remote tracks are surfaced as events, not hidden inside transport sessions

Key ideas worth adopting:

- top-level `Room` object as the center of the experience
- participants are first-class
- tracks/publications are first-class
- a clear local-vs-remote split
- good event names around participant and track lifecycle

## What To Copy From Hang JS

Hang JS suggests a second, equally important lesson:

- the advanced API should still be object-based and declarative

Specific ideas worth copying:

### 1. A curated mid-level `Broadcast` object

Hang's watch-side `Broadcast` is a good shape:

- `enabled`
- `name`
- `reload`
- `status`
- `active`
- `catalog`

all live on one object (`../moq/js/watch/src/broadcast.ts:22`).

That is better than requiring app code to manually stitch together:

- a session
- a named subscription
- catalog loading
- reconnect/reload policy
- state tracking

Rust should have an equivalent advanced API:

```rust
pub struct RemoteBroadcast {
    pub fn name(&self) -> &BroadcastName;
    pub fn status(&self) -> BroadcastStatus;
    pub fn catalog(&self) -> &Catalog;
    pub fn subscribe_video(&self, opts: SubscribeVideoOptions) -> Result<RemoteVideoTrack>;
    pub fn subscribe_audio(&self, opts: SubscribeAudioOptions) -> Result<RemoteAudioTrack>;
}
```

and:

```rust
pub struct LocalBroadcast {
    pub fn set_video(&self, source: impl VideoSource, opts: PublishVideoOptions) -> Result<()>;
    pub fn set_audio(&self, source: impl AudioSource, opts: PublishAudioOptions) -> Result<()>;
    pub fn set_chat(&self, ...) -> Result<()>;
    pub fn catalog(&self) -> &Catalog;
}
```

### 2. Structure subfeatures as components, not boolean bags

Hang publish-side `Broadcast` contains typed subcomponents:

- `audio`
- `video`
- `location`
- `chat`
- `preview`

instead of a flat pile of booleans and loosely-related methods (`../moq/js/publish/src/broadcast.ts:30`).

That maps well to Rust.
Instead of:

- `camera: bool`
- `screen: bool`
- `audio: bool`

prefer nested components or typed options:

```rust
pub struct LocalMedia {
    pub camera: LocalVideoPublication,
    pub microphone: LocalAudioPublication,
    pub screen_share: LocalVideoPublication,
}
```

or:

```rust
pub struct CaptureManager {
    pub camera(&self) -> &CameraCapture;
    pub microphone(&self) -> &MicrophoneCapture;
    pub screen_share(&self) -> &ScreenShareCapture;
}
```

### 3. Separate capability filtering from selection policy

Hang watch-side `video::Source` does a nice job separating:

- catalog extraction
- support filtering
- target-based selection

(`../moq/js/watch/src/video/source.ts:140`)

That is better than baking one fixed `Quality` enum into the subscription surface.

Rust should keep `Quality`, but also add a richer selection object:

```rust
pub struct VideoTarget {
    pub name: Option<String>,
    pub max_pixels: Option<u32>,
    pub max_bitrate: Option<u64>,
}
```

and:

```rust
pub struct SubscribeVideoOptions {
    pub target: Option<VideoTarget>,
    pub decode: DecodeConfig,
}
```

This would be a direct ergonomic improvement over:

- `watch_with(playback_config, quality)`
- `watch_rendition(playback_config, track_name)`

> **[Note]:** `VideoTarget` and `SubscribeVideoOptions` should use the **builder-lite** pattern with `Default` + field methods, not pub fields. This future-proofs against field additions:
>
> ```rust
> #[derive(Default)]
> pub struct VideoTarget {
>     max_pixels: Option<u32>,
>     max_bitrate: Option<u64>,
>     name: Option<String>,
> }
>
> impl VideoTarget {
>     pub fn max_pixels(mut self, pixels: u32) -> Self {
>         self.max_pixels = Some(pixels);
>         self
>     }
>     pub fn max_bitrate(mut self, bitrate: u64) -> Self {
>         self.max_bitrate = Some(bitrate);
>         self
>     }
>     // ... etc
> }
> ```
>
> Usage: `VideoTarget::default().max_pixels(1280 * 720)`. Adding a new field later is backwards-compatible because construction is method-based. This also enables `#[non_exhaustive]` on the struct for extra safety. The current `VideoEncoderConfig` already partially does this (`.bitrate()`, `.framerate()` methods) — just standardize the pattern.
>
> Also consider: `Quality` could implement `Into<VideoTarget>` so the simple path stays simple:
>
> ```rust
> // Simple
> publication.subscribe_video(Quality::High).await?;
> // Advanced
> publication.subscribe_video(VideoTarget::default().max_pixels(1280 * 720)).await?;
> ```
>
> This uses the `impl Into<T>` parameter pattern to accept both.

### 4. Make status/state observable on the object itself

Hang APIs make state part of the object model:

- publish active/inactive
- watch offline/loading/live
- current selected track/config

Rust currently tends to expose:

- one-shot async methods
- mpsc event channels
- low-level handles

This is workable, but it is less ergonomic than object-local state.

In Rust, that suggests:

- event streams for structural changes
- plus queryable current state on the object

for example:

```rust
impl RemoteBroadcast {
    pub fn state(&self) -> RemoteBroadcastState;
    pub fn selected_video(&self) -> Option<SelectedRendition>;
}
```

> **[Note]:** The codebase already has exactly the right primitive for this: `n0_watcher::Watchable<T>`. It provides `.get()` for snapshot reads and `.watch()` for async change notification. The pattern should be:
>
> ```rust
> impl RemoteParticipant {
>     /// Returns the current connection state.
>     pub fn state(&self) -> ParticipantState {
>         self.state_watchable.get()
>     }
>
>     /// Returns a watcher that yields when the participant state changes.
>     pub fn state_changed(&self) -> n0_watcher::Direct<ParticipantState> {
>         self.state_watchable.watch()
>     }
> }
> ```
>
> This dual accessor pattern (snapshot + watcher) is already used for the catalog in `SubscribeBroadcast` (`.catalog()` + `.catalog_watcher()`). Just standardize it across all stateful types.
>
> The watcher integrates with `select!` naturally:
>
> ```rust
> let mut state_watcher = participant.state_changed();
> loop {
>     tokio::select! {
>         _ = state_watcher.changed() => {
>             let state = participant.state();
>             update_ui(state);
>         }
>         event = room_events.next() => { /* ... */ }
>     }
> }
> ```
>
> This is more idiomatic than the LiveKit-style callback/event-listener approach and fits naturally into Rust's ownership model. No closures, no lifetimes on callbacks, no unsubscribe tokens.

### 5. Curated exports matter

Hang's package root exports are intentional and small:

- `@moq/watch` exports a curated set (`../moq/js/watch/src/index.ts:1`)
- `@moq/publish` exports a curated set (`../moq/js/publish/src/index.ts:1`)
- web components are intentionally not in the same default root

Rust should do the same.
`iroh-live` should export a deliberately curated happy-path surface and move escape hatches under a clearly named advanced namespace.

## Revised Architecture Recommendation

After reviewing both LiveKit and Hang JS, I think the right architecture is three bands, not two:

### 1. Product API

For app authors building calls and rooms:

- `Live`
- `Call`
- `Room`
- `Participant`
- `TrackPublication`
- `RemoteTrack` / `LocalTrack`

### 2. Broadcast API

For advanced users who want direct media composition without full room semantics:

- `LocalBroadcast`
- `RemoteBroadcast`
- `BroadcastCatalog`
- `SubscribeVideoOptions`
- `SubscribeAudioOptions`

This is the layer where Hang JS is especially instructive.

### 3. Raw transport/media API

For systems and experimentation:

- `MoqSession`
- raw announce/subscribe/publish
- `BroadcastProducer` / `BroadcastConsumer`
- media pipelines and codecs

This split is a better fit than either:

- exposing only low-level transport, or
- trying to force everything through rooms/calls only

## Architectural Alternatives

There are at least four viable directions for this API. They are not equally good, but they are all real options.

## Alternative A: Keep the current architecture and rename things

### Shape

- keep `Live`, `Room`, `PublishBroadcast`, `SubscribeBroadcast`
- rename methods and types for consistency
- add a few more convenience helpers
- leave transport/session/broadcast concepts mostly intact

### Pros

- lowest implementation cost
- least disruptive internally
- preserves maximum transparency
- easy to land incrementally

### Cons

- does not solve the core product-model problem
- app code still has to assemble participant/publication state itself
- accepting inbound one-to-one still remains unnatural unless deeper changes happen
- examples will still read as "transport orchestration" instead of "call/room API"

### Evaluation

This is not enough.
It would improve aesthetics, but not ergonomics.
I do not recommend this as the main path.

## Alternative B: Broadcast-first public API

### Shape

- make `LocalBroadcast` / `RemoteBroadcast` the primary API
- rooms and calls are thin composition layers on top of broadcasts
- users think in named broadcasts first, participants second

### Pros

- very close to MoQ and Hang
- maps well onto existing internals
- good for advanced/manual use cases
- easier migration from current API

### Cons

- still not the best mental model for RTC rooms and calls
- top-level app code still has to reason about source grouping and publication policy
- participant identity and track/publication lifecycle remain secondary

### Evaluation

This would be a large improvement over the current Rust API and is attractive technically.
But as the top-most public model, it is still too media-transport flavored for the product goals in this repo.

I would use this as the advanced public layer, not the primary layer.

## Alternative C: Room/call-first public API only

### Shape

- expose only `Call`, `Room`, `Participant`, `TrackPublication`
- hide broadcasts almost completely
- advanced users are expected to work through participants/publications too

### Pros

- best beginner ergonomics
- closest to LiveKit
- easiest story for one-to-one and rooms
- strongest abstraction boundary

### Cons

- risks making advanced/manual pipelines awkward
- may force unnatural abstractions on non-room use cases
- likely causes pressure to leak internals back later
- could underserve one of the repo’s explicit goals: manual processing and setup

### Evaluation

This is too opinionated if taken alone.
For this repo, it would probably overcorrect and make the low-level strengths harder to use cleanly.

## Alternative D: Three-layer API: product, broadcast, raw

### Shape

- top layer: `Call`, `Room`, `Participant`, `TrackPublication`
- middle layer: `LocalBroadcast`, `RemoteBroadcast`
- low layer: `MoqSession`, `BroadcastProducer`, `BroadcastConsumer`, media pipelines

### Pros

- best fit for the repo’s actual goals
- supports simple rooms/calls and manual media equally well
- matches what LiveKit does well at the product layer
- matches what Hang JS does well at the media/broadcast layer
- can be introduced incrementally

### Cons

- more design work
- more surface area to maintain
- requires stronger discipline around module boundaries
- needs clear rules so the three layers do not blur together

### Evaluation

This is the best option.
It acknowledges that this repo is not just an RTC product API and not just a transport library.
It needs both.

> **[Note]:** This three-layer approach maps cleanly to Rust’s **crate boundary** discipline. Consider whether the layers should be:
>
> **Option 1: Module-based layers within `iroh-live`**
> ```
> iroh_live::          // product API (Room, Call, Participant)
> iroh_live::broadcast // mid-level (LocalBroadcast, RemoteBroadcast)
> iroh_live::transport // escape hatch (re-exports from iroh-moq)
> ```
>
> **Option 2: Crate-based layers** (current structure, refined)
> ```
> iroh-live   → product API, re-exports curated surface from lower crates
> moq-media   → broadcast/media layer (renamed types)
> iroh-moq    → transport layer
> ```
>
> Option 1 is simpler for users (one dependency), option 2 gives stronger compile-time boundaries. Given that `moq-media` already has no iroh dependency (good!), keeping it as a separate crate is valuable — someone building a non-iroh MoQ stack can use it standalone. So **option 2 is better**, with `iroh-live` doing curated re-exports.
>
> The key Rust idiom here: **the facade crate pattern**. `iroh-live` becomes a thin facade that re-exports the happy-path types from lower crates, adds the product-level types (Room, Call, Participant), and hides everything else behind `pub use iroh_moq as transport` or similar. This is how `tokio` works — it re-exports from `tokio-macros`, `mio`, etc. while presenting one unified API.

## Alternative E: Builder-heavy graph API

### Shape

- represent everything as explicit graph construction:
- `Live::builder()`
- `Room::builder()`
- `PublicationBuilder`
- `SubscriptionBuilder`
- `CaptureManagerBuilder`

### Pros

- extremely flexible
- explicit configuration story
- good fit for Rust’s type-driven style

### Cons

- verbose
- can become ceremony-heavy very quickly
- not a good default for the simple path
- often worse than object methods for interactive/live mutation

### Evaluation

Use builders for construction and coarse configuration, not as the primary everyday API.
The current repo’s use cases involve a lot of runtime mutation. That favors stable objects with methods over a builder-only style.

> **[Note]:** The right Rust pattern here is **builders for construction, methods for mutation**. Specifically:
>
> - **Use builders only where there are many optional config parameters** that are set once at creation time: `Live::builder()`, `RoomOptions::builder()`. The builder should consume itself into the final type.
> - **Use `Default` + field methods** for simple option structs with <5 fields: `PublishCameraOptions::default().codec(VideoCodec::H264)`. No separate builder type needed.
> - **Use plain methods** for runtime mutation: `publication.set_enabled(false)`, `track.set_quality(Quality::High)`.
>
> A good rule of thumb: if the type needs `async` setup (network calls, device access), it probably needs a builder or async constructor. If it’s just configuration data, `Default` + methods is enough.
>
> **Type-state builders** are overkill here. Something like `RoomBuilder<NoEndpoint>` → `RoomBuilder<HasEndpoint>` adds complexity for little gain. The real-world usage is `Live::builder().spawn().await?` — just validate at runtime in `spawn()` and return a clear error.

## API Style Alternatives

Beyond architecture, there are also different style choices within the chosen direction.

## Style 1: Event-stream first

### Shape

- objects expose `events()` and most state changes come through streams
- current state is relatively thin

### Pros

- familiar in async Rust
- straightforward to implement from current actor design
- good for reactive app loops

### Cons

- users need to cache their own state
- every app rebuilds the same derived model
- harder to inspect current state without running event plumbing

### Recommendation

Do not use this alone.
Keep event streams, but pair them with queryable current state on objects.

## Style 2: Snapshot/state first

### Shape

- objects expose current state directly
- events are secondary hints/notifications

### Pros

- easy to inspect
- better for UI integration
- matches Hang JS well

### Cons

- more internal synchronization complexity
- risks stale mental model if events and state diverge

### Recommendation

This is better for the broadcast and participant layers, but it should still be backed by events internally and publicly.

## Style 3: Command-query split

### Shape

- commands mutate via methods
- state/query is read separately
- events exist for subscription

### Pros

- easiest model to reason about
- scales well across top-level and advanced APIs
- maps cleanly to Rust objects

### Recommendation

This should be the default style.
For example:

```rust
room.local_participant().publish_camera(...).await?;
let pubs = room.remote_participant(id).unwrap().publications();
let mut events = room.events();
```

This is better than either purely event-driven or purely builder-driven APIs.

> **[Note]:** This is the right call. A few Rust-specific refinements:
>
> **Commands should be `&self`, not `&mut self`**: Since the types are Arc-based handles, mutation goes through interior mutability (Mutex/RwLock/actor message). This means multiple tasks can hold the same `Room` handle and issue commands concurrently without `&mut` conflicts. The current `Room::publish()` already takes `&self` — good.
>
> **Queries should return owned snapshots or cheap clones, not references**: Returning `&RemoteParticipant` from `room.remote_participant(id)` creates borrow problems in async code. Better:
>
> ```rust
> // Returns a cloneable handle, not a reference
> pub fn remote_participant(&self, id: ParticipantId) -> Option<RemoteParticipant> { ... }
>
> // Returns a snapshot vec, not an iterator over borrowed data
> pub fn remote_participants(&self) -> Vec<RemoteParticipant> { ... }
> ```
>
> This is how `DashMap::get()` returns a guard and how `iroh::Endpoint::remote_info()` returns owned data. It avoids the "hold a lock while awaiting" anti-pattern.
>
> **Events should carry enough data to be self-contained**: Don't make users query the room to understand an event. For example, `TrackPublished` should carry the `RemoteTrackPublication` handle directly, not just a `PublicationId` that requires a follow-up lookup. The current `RoomEvent::BroadcastSubscribed` already does this with `broadcast: SubscribeBroadcast` — good instinct, just needs better types.

## Proposed API Shape

## 1. Top-level modules

I would move toward this public surface:

```rust
pub mod call;
pub mod room;
pub mod media;
pub mod broadcast;
pub mod tickets;
pub mod node;
pub mod lowlevel; // opt-in escape hatch

pub use call::{Call, CallAcceptor, CallEvent, IncomingCall};
pub use broadcast::{LocalBroadcast, RemoteBroadcast, BroadcastStatus};
pub use room::{
    Room, RoomEvent, RoomOptions,
    LocalParticipant, RemoteParticipant, ParticipantId,
    LocalTrackPublication, RemoteTrackPublication,
    TrackKind, TrackSource,
};
pub use tickets::{CallTicket, RoomTicket};
pub use node::LiveNode;

pub struct Live { ... }
```

Important:

- keep `iroh_moq` re-export out of the main happy path
- keep `BroadcastProducer`/`BroadcastConsumer` out of the main happy path
- keep current lower-level types available under an explicit advanced namespace if needed

> **[Note]:** The re-export list is good but should follow the **prelude pattern** for maximum ergonomics. Consider:
>
> ```rust
> // For users who want everything at once
> pub mod prelude {
>     pub use crate::{Live, Room, RoomEvent, Call, CallEvent, IncomingCall};
>     pub use crate::room::{LocalParticipant, RemoteParticipant, ParticipantId};
>     pub use crate::room::{LocalTrackPublication, RemoteTrackPublication, TrackSource};
>     pub use crate::tickets::{CallTicket, RoomTicket};
>     pub use crate::media::{AudioBackend, DecodeConfig, Quality};
> }
> ```
>
> Usage: `use iroh_live::prelude::*;`. This is standard for Rust SDKs (bevy, wgpu, diesel all do this). It lets examples stay clean without 15 lines of imports.
>
> **On `lowlevel` naming**: consider `transport` instead. "Low level" is pejorative and ambiguous. `transport` says what it *is*, and matches the crate name (`iroh-moq`). Alternatives: `raw`, `moq`, `internal`. The iroh ecosystem already uses `iroh::endpoint` not `iroh::lowlevel` — follow that convention.
>
> **On the module structure**: `media` is ambiguous — does it mean codecs, capture devices, or media processing? Since `moq-media` already exists as a crate, `iroh_live::media` should probably just be curated re-exports from that crate. The actual *new* code lives in `call`, `room`, `broadcast`. Consider whether `media` should be called `codec` or `capture` instead, or whether it should be split into `iroh_live::capture` (sources/sinks) and `iroh_live::codec` (encoders/decoders).

## Boundary Rules For The Three Layers

If the three-layer design is chosen, the boundaries need to be explicit.

## Product layer should expose

- calls
- rooms
- participants
- publications
- subscriptions
- tickets
- local publish/unpublish
- incoming acceptance

## Product layer should not expose

- raw MoQ sessions
- raw broadcast producers/consumers
- catalog track names
- track request serving mechanics

## Broadcast layer should expose

- local broadcast composition
- remote broadcast subscription
- catalog access
- rendition selection
- preview
- reload/reconnect policy

## Broadcast layer should not expose by default

- room membership semantics
- participant graphs
- gossip policy

## Raw layer should expose

- sessions
- direct publish/subscribe
- explicit announce/request mechanics
- pipelines/codecs/sources/sinks

These rules matter, otherwise the same confusion returns under new names.

## 2. Direct one-to-one API

### Outbound

```rust
let live = Live::builder().spawn().await?;
let mut call = live.call(peer_ticket).await?;
call.local_participant()
    .publish_camera(camera, PublishCameraOptions::default())
    .await?;
call.local_participant()
    .publish_microphone(mic, PublishAudioOptions::default())
    .await?;
```

### Inbound

```rust
let live = Live::builder().spawn().await?;
let mut incoming = live.incoming_calls();

while let Some(call) = incoming.next().await {
    let remote = call.remote_id();
    if should_accept(remote) {
        let mut call = call.accept().await?;
        // subscribe / publish
    } else {
        call.reject().await?;
    }
}
```

This is the single most important missing API addition.

> **[Note]:** The outbound `call()` method should accept `impl Into<CallTicket>` so users can pass either a ticket or a raw `EndpointAddr`:
>
> ```rust
> // With a ticket (has metadata, bootstrap peers, etc.)
> let call = live.call(ticket).await?;
>
> // With a raw address (direct connection, no metadata)
> let call = live.call(remote_addr).await?;
> ```
>
> This is a standard Rust ergonomics pattern (see `reqwest::get(impl IntoUrl)`).
>
> For the inbound side, the `IncomingCall` should expose **announced broadcasts** before accepting, since in MoQ the announcer sends its catalog early. This lets the callee make an informed decision:
>
> ```rust
> let incoming = incoming.next().await.unwrap();
> println!("from: {}", incoming.remote_id());
> println!("offers: {:?}", incoming.announced_broadcasts()); // ["camera", "screen"]
> let call = incoming.accept().await?;
> ```

### Why `Call`

One-to-one is not a room.
Even if the implementation internally reuses room-ish concepts, the public API should admit the common case directly.

`Call` can still expose:

- `local_participant()`
- `remote_participant()`
- `events()`
- `close()`

That is much easier to reason about than a raw `MoqSession` plus named broadcasts.

## 3. Room API

### Join / create

```rust
let live = Live::builder().spawn().await?;

let room = live.create_room(RoomOptions::default()).await?;
let ticket = room.ticket();

let room = live.join_room(ticket).await?;
```

For nodeful environments:

```rust
let node = LiveNode::builder().spawn().await?;
let room = node.live().join_room(ticket).await?;
```

### Room object model

```rust
pub struct Room {
    pub fn id(&self) -> RoomId;
    pub fn ticket(&self) -> RoomTicket;
    pub fn local_participant(&self) -> &LocalParticipant;
    pub fn remote_participants(&self) -> impl Iterator<Item = &RemoteParticipant>;
    pub fn participant(&self, id: ParticipantId) -> Option<&RemoteParticipant>;
    pub fn events(&self) -> RoomEvents;
    pub async fn close(&self) -> Result<()>;
}
```

`RoomEvent` should look more like:

```rust
pub enum RoomEvent {
    ParticipantJoined(RemoteParticipant),
    ParticipantLeft { participant_id: ParticipantId },
    TrackPublished {
        participant_id: ParticipantId,
        publication: RemoteTrackPublication,
    },
    TrackUnpublished {
        participant_id: ParticipantId,
        publication_id: PublicationId,
    },
    TrackSubscribed {
        participant_id: ParticipantId,
        publication_id: PublicationId,
        track: RemoteTrack,
    },
    TrackUnsubscribed {
        participant_id: ParticipantId,
        publication_id: PublicationId,
    },
}
```

That is much closer to what room users actually need.

## 4. Participant and publication model

### Local participant

```rust
impl LocalParticipant {
    pub async fn publish_camera(
        &self,
        source: impl VideoSource,
        options: PublishCameraOptions,
    ) -> Result<LocalTrackPublication>;

    pub async fn publish_microphone(
        &self,
        source: impl AudioSource,
        options: PublishAudioOptions,
    ) -> Result<LocalTrackPublication>;

    pub async fn publish_screen(
        &self,
        source: impl VideoSource,
        options: PublishScreenOptions,
    ) -> Result<LocalTrackPublication>;

    pub async fn unpublish(&self, publication_id: PublicationId) -> Result<()>;
}
```

### Remote participant

```rust
impl RemoteParticipant {
    pub fn id(&self) -> ParticipantId;
    pub fn publications(&self) -> impl Iterator<Item = &RemoteTrackPublication>;
    pub fn publication(&self, id: PublicationId) -> Option<&RemoteTrackPublication>;
}
```

### Publication

This is the missing middle layer in the current design.

A publication should represent:

- stable identity
- metadata
- source kind (`Camera`, `Microphone`, `ScreenShare`, `Custom`)
- whether it is currently subscribed
- available qualities/renditions

Then the media object can be separate:

- publication metadata exists before subscribing
- subscription attaches actual audio/video pipelines

That matches both LiveKit's shape and common RTC expectations.

> **[Note]:** The publication/subscription split maps naturally to Rust ownership:
>
> ```rust
> // Publication is metadata — cheap Clone, always available
> let pub_handle: RemoteTrackPublication = remote.publication(id).unwrap();
> println!("source: {:?}, renditions: {:?}", pub_handle.source(), pub_handle.renditions());
>
> // Subscription is a resource — owned, produces frames, dropped to unsubscribe
> let subscription: VideoSubscription = pub_handle.subscribe_video(opts).await?;
> // subscription owns the decoder pipeline, frame channel, etc.
>
> // Dropping the subscription unsubscribes and cleans up
> drop(subscription);
> // pub_handle still valid, can resubscribe
> ```
>
> This is a clean ownership model: the publication handle is a lightweight view (Arc-based, Clone), while the subscription is an owned resource with drop semantics. It means "unsubscribe" is just `drop(subscription)` — no explicit `unsubscribe()` needed (though providing one for clarity is fine).
>
> The `VideoSubscription` type should expose frame access the same way `WatchTrack` does today:
>
> ```rust
> impl VideoSubscription {
>     /// Returns the most recent decoded frame, or None if no frame yet.
>     pub fn current_frame(&self) -> Option<DecodedVideoFrame> { ... }
>     /// Waits for the next decoded frame.
>     pub async fn next_frame(&mut self) -> Option<DecodedVideoFrame> { ... }
>     /// The currently active rendition name.
>     pub fn rendition(&self) -> &str { ... }
>     /// Hints the decoder about target display size.
>     pub fn set_viewport(&self, width: u32, height: u32) { ... }
> }
> ```

## 5. Track naming model

Current naming overuses arbitrary strings.

I would split naming into:

- `TrackSource`: semantic source (`Camera`, `Microphone`, `ScreenShare`, `Custom`)
- `PublicationId`: stable internal/public ID
- optional `label`: user-facing name
- `EncodingProfile` / `QualityLayer`: rendition or quality details

The current `"camera"` / `"screen"` / `"video/h264-720p"` style strings should mostly become internal.

Users should not need to manually coordinate string names unless they opt into custom/manual publication.

## 6. Proposed broadcast-level API

This is the layer that is mostly missing today, and where Hang JS provides the clearest inspiration.

### Local broadcast

```rust
let broadcast = live.create_broadcast("alice").await?;

broadcast.video().set_source(CameraCapturer::new()?);
broadcast.video().set_simulcast([
    VideoPreset::P180,
    VideoPreset::P720,
]);

broadcast.audio().set_source(audio.default_input().await?);
broadcast.publish().await?;
```

### Remote broadcast

```rust
let broadcast = live.subscribe_broadcast(ticket).await?;

let video = broadcast.subscribe_video(SubscribeVideoOptions {
    target: Some(VideoTarget {
        max_pixels: Some(1280 * 720),
        max_bitrate: None,
        name: None,
    }),
    decode: DecodeConfig::default(),
}).await?;
```

This is still MoQ-ish and catalog-aware, but vastly more ergonomic than today's raw tuple return values and free-floating helper types.

> **[Note]:** The broadcast layer is the right place for **non-RTC use cases** too. Video distribution, audio studio links, one-to-many streaming — these are all "broadcast"-shaped, not "room/call"-shaped. The broadcast API should not assume bidirectional communication.
>
> A few Rust-specific refinements for the local broadcast API:
>
> **Typestate for publish lifecycle**: A broadcast can be in "configuring" or "live" state. Consider:
>
> ```rust
> // Configuring — can add/remove sources
> let mut broadcast = live.create_broadcast("stream-1");
> broadcast.set_video(camera, VideoRenditions::new(codec, [P720, P1080]));
> broadcast.set_audio(mic, AudioRenditions::new(AudioCodec::Opus, [AudioPreset::Hq]));
>
> // Publish — now live, sources are running
> let live_broadcast: LiveBroadcast = broadcast.publish().await?;
>
> // LiveBroadcast still allows runtime changes (source swap, enable/disable)
> live_broadcast.set_video_enabled(false);
> live_broadcast.set_video_enabled(true); // resumes
> ```
>
> Actually, skip the typestate here. A `LocalBroadcast` that is always "live" and accepts `set_video(Some(...))` / `set_video(None)` at any time is simpler and matches how users actually think. The current `PublishBroadcast::set_video(Option<VideoRenditions>)` is already this shape — just needs better naming.
>
> **Remote broadcast should support `Stream` for frame-level access**:
>
> ```rust
> // For rendering loops (game engines, egui)
> let frame = video.current_frame(); // non-blocking snapshot
>
> // For processing pipelines (recording, transcoding)
> while let Some(frame) = video.next_frame().await {
>     process(frame);
> }
>
> // As a Stream (for composition)
> let frame_stream = video.frames(); // impl Stream<Item = DecodedVideoFrame>
> ```

## 7. Proposed advanced manual API

The simple API above should sit on top of an advanced manual path.

For advanced users:

```rust
let publication = room.local_participant()
    .publish_video_track(
        LocalVideoTrack::from_source(camera)
            .with_encoder(codec::H264Encoder::with_preset(VideoPreset::P720)?)
            .with_simulcast([VideoPreset::P180, VideoPreset::P720]),
        PublishTrackOptions {
            source: TrackSource::Camera,
            label: Some("cam".into()),
            ..Default::default()
        }
    )
    .await?;
```

And for remote:

```rust
let publication = remote.publications()
    .find(|p| p.source() == TrackSource::ScreenShare)
    .unwrap();

let track = publication.subscribe(SubscribeOptions {
    quality: Quality::High,
    decode: DecodeConfig::default(),
}).await?;
```

This is the same power as today, but organized around track publications instead of raw broadcasts.

## More Detailed Evaluation Of Current Rust Types

## `Live`

Assessment: too thin to be the primary API, too exposed to be a clean facade.

Problems:

- `pub moq: Moq` makes it impossible to maintain abstraction discipline
- construction is inconsistent with `LiveNode`
- API returns raw transport and subscription internals

Recommendation:

- keep the name
- make it builder-based
- make it own the happy-path API
- move raw transport access behind `advanced()` or `lowlevel()`

## `Room`

Assessment: useful prototype, not yet the right public abstraction.

Strong parts:

- the current actor model is enough to prove out membership and auto-subscription
- `RoomTicket` is a sound concept

Weak parts:

- event model is too low-level
- participant/publication identity is missing
- `split()` and manual event loop style are too actor-shaped for the default path

Recommendation:

- keep internal actor architecture if useful
- change the public object model completely

## `PublishCaptureController`

Assessment: valuable implementation, weak public API.

Good:

- transport-agnostic
- encapsulates device and source mutation

Bad:

- shaped around internal broadcast layout
- `set_opts()` bundles too much and returns transport artifacts
- booleans do not scale

Recommendation:

- demote to advanced/internal layer, or
- rename to `CaptureManager` and wrap it with participant publication APIs

## `SubscribeBroadcast`

Assessment: closest existing Rust type to a useful mid-level abstraction.

It already has:

- lifecycle
- catalog
- rendition selection
- audio and video helpers

But the naming and surrounding model are weak.

Recommendation:

- evolve this into `RemoteBroadcast`
- keep the core functionality
- rename methods around `subscribe_*`
- add queryable state
- add richer selection options

## `PublishBroadcast`

Assessment: useful internals, wrong public shape.

The type itself is not bad. The problem is that users interact with it via raw producers and controller glue rather than through a cohesive publication object.

Recommendation:

- evolve this into `LocalBroadcast`
- keep `watch_local()` capability but rename it as preview
- stop exposing `producer()` in the default path

> **[Note]:** The `LocalBroadcast` surface should be component-based, not setter-based. Prefer:
>
> - `broadcast.video().publish_camera(...)`
> - `broadcast.video().publish_screen(...)`
> - `broadcast.video().replace_source(...)`
> - `broadcast.video().clear()`
> - `broadcast.audio().publish_microphone(...)`
> - `broadcast.audio().replace_source(...)`
> - `broadcast.audio().clear()`
>
> over top-level `set_video()` / `set_audio()` methods as the primary public shape. The setter form can still exist internally or as a lower-level escape hatch, but the component form matches the examples and makes source replacement semantics much clearer.

## `Moq`

Assessment: useful internal transport manager, wrong shape for most app-facing code.

Good:

- centralizes session and publish bookkeeping
- provides a natural place to host incoming-session support

Bad:

- currently too directly reachable from `Live`
- its responsibilities are transport lifecycle, not end-user semantics

Recommendation:

- keep as a lower layer
- add incoming-session support here
- do not let it define the primary user-facing object model

## `MoqSession`

Assessment: valid advanced type, poor default return type.

Good:

- directness
- obvious mapping to transport operations

Bad:

- mutable subscription API is awkward for a high-level object
- session lifetime is not the same as participant/publication lifetime
- encourages apps to center their architecture on connection state

Recommendation:

- retain in advanced/low-level API
- avoid returning it from primary `Live` APIs except under explicit advanced methods

## `AudioBackend`

Assessment: one of the more successful public APIs in the repo, but it should integrate more naturally into higher layers.

Good:

- clear, focused responsibility
- sensible construction and device operations
- useful as a dependency for higher-level APIs

Bad:

- room/call APIs currently make the user thread it through too manually
- device switching currently leaks into application orchestration too much

Recommendation:

- keep most of this API
- let top-level APIs accept an `AudioBackend` once and reuse it across subscriptions/publications
- reduce the need for app code to rebuild backends manually

## Proposed Implementation Direction For Accepting Incoming Calls

The current `MoqProtocolHandler` accepts sessions internally and hands them to the `Moq` actor, but there is no public "incoming session" stream.

That should change.

## 1. Add a public incoming-connection stream at the `iroh-moq` layer

Something like:

```rust
pub struct IncomingSession {
    pub fn remote_id(&self) -> EndpointId;
    pub async fn accept(self) -> Result<MoqSession>;
    pub async fn reject(self) -> Result<()>;
}

impl Moq {
    pub fn incoming(&self) -> impl Stream<Item = IncomingSession>;
}
```

Implementation idea:

- `MoqProtocolHandler` should no longer auto-finalize everything into hidden session state only
- accepted WebTransport connections should be wrapped in a pending incoming object
- `Moq` should surface these through a channel/stream
- `accept()` performs `MoqSession::session_accept`
- `reject()` closes the underlying connection/session

This does require implementation work. It is not just an API rename.

## 2. Build `CallAcceptor` on top of incoming `MoqSession`s

At the `iroh-live` layer:

```rust
impl Live {
    pub fn incoming_calls(&self) -> CallAcceptor;
}
```

`CallAcceptor` should:

- yield `IncomingCall`
- let the user inspect remote identity
- possibly inspect a small announced intent blob if you add one later
- support `accept()` / `reject()`

## 3. Keep auto-accept as a policy option, not the only behavior

There can still be:

```rust
Live::builder().auto_accept_calls(true)
```

or room internals that auto-accept peer sessions.
But the lower level needs explicit acceptance to exist publicly.

## Proposed Redesign Of Publish State

`PublishOpts` should not be the main long-term public type.

It is fine for UI wiring, but it is too coarse and too capture-specific.

I recommend:

```rust
pub struct CaptureState {
    pub camera: CaptureToggle<CameraCaptureOptions>,
    pub microphone: CaptureToggle<MicrophoneCaptureOptions>,
    pub screen_share: CaptureToggle<ScreenShareOptions>,
}

pub enum CaptureToggle<T> {
    Disabled,
    Enabled(T),
}
```

Why this is better:

- avoids boolean bags
- lets each source have source-specific options
- scales to more sources
- avoids nesting codec/device concerns into one shared `CaptureConfig`

For UI convenience, you can still offer a compatibility helper:

```rust
impl CaptureState {
    pub fn from_legacy(opts: PublishOpts) -> Self { ... }
}
```

## Proposed Naming Changes

## Current to proposed mapping

| Current | Proposed | Why |
|---|---|---|
| `Live` | `Live` | Keep, but make it the real top-level API |
| `LiveNode` | `LiveNode` / `LiveNodeBuilder` | Keep concept, add builder |
| `LiveTicket` | `CallTicket` | "Live" is too generic; this is a direct-call join handle |
| `RoomTicket` | `RoomTicket` | Keep |
| `MoqSession` | `lowlevel::Session` or internal | Too transport-specific for main path |
| `PublishBroadcast` | `LocalPublicationSet` or internal | Users think in publications/tracks, not broadcasts |
| `SubscribeBroadcast` | `RemotePublicationSet` or internal | Same reason |
| `AvRemoteTrack` | `RemoteMediaTracks` or remove | Current name is awkward |
| `WatchTrack` | `RemoteVideoTrack` / `VideoSubscription` | "watch" is UI jargon |
| `AudioTrack` | `RemoteAudioTrack` / `AudioSubscription` | Align naming |
| `PublishCaptureController` | `CaptureManager` / `LocalMediaManager` | More product-shaped |
| `PublishOpts` | `CaptureState` / `LocalMediaState` | More extensible |
| `StreamKind` | `TrackSource` | Use RTC/media language |
| `RoomPublisherSync` | remove | Replace with real participant/publication API |
| `connect_and_subscribe` | `dial_publication` or remove | Too transport-shaped |
| `watch_and_listen` | `subscribe_default_media` | More semantic |

## Naming rules to standardize

- Use `participant` consistently for people/peers in rooms/calls
- Use `publication` for publish-side metadata and subscription handles
- Use `track` for media objects
- Use `source` for semantic origin: camera, microphone, screen share
- Use `subscription` for the act/result of receiving media
- Reserve `broadcast` for low-level or internal APIs
- Reserve `session` for low-level transport objects

## Proposed Event Model

The current event model is too low-level:

- `RemoteAnnounced`
- `RemoteConnected`
- `BroadcastSubscribed`

I recommend two event layers:

### Low-level events

Available in an advanced API:

- session accepted
- session closed
- publication announced
- publication withdrawn

### High-level room/call events

Default API:

- participant joined
- participant left
- track published
- track unpublished
- track subscribed
- track unsubscribed
- active speaker changed later if desired

This split keeps power while making the default model easier.

## Event Model Alternatives

There are a few valid event-model choices here.

## Alternative 1: Single flat event enum per room/call

### Pros

- simple to discover
- easy to log/debug
- matches many Rust async designs

### Cons

- can grow very large
- consumers have to pattern-match many cases

### Evaluation

This is fine for the initial high-level API.

## Alternative 2: Per-object event streams

Examples:

- `room.events()`
- `participant.events()`
- `publication.events()`

### Pros

- more local
- easier to scope observers
- scales better over time

### Cons

- more complex implementation
- more handles/streams for users to manage

### Evaluation

Best long-term shape, but likely too much for the first redesign pass.

## Alternative 3: State watchables plus sparse events

### Pros

- very UI-friendly
- aligns with Hang JS patterns

### Cons

- Rust ecosystem conventions lean more toward streams than signal graphs
- choosing the watch abstraction becomes part of the public API commitment

### Evaluation

Good internally and maybe for selected advanced objects, but I would avoid making the top-level public API depend on a watcher ecosystem unless there is a very strong reason.

> **[Note]:** Actually, given that `n0_watcher` is already an iroh ecosystem dependency (used in `iroh::endpoint::Watcher`), committing to it as a public API primitive is not a large risk. The dual pattern of `.get()` (snapshot) + `.watch()` (change stream) is a genuinely good fit for this domain. The iroh crate itself uses `Watcher` for connection path stats. Exposing `n0_watcher::Direct<T>` in the API is fine — it's a tiny type that wraps a `tokio::sync::watch::Receiver` and implements `Stream`.
>
> Concretely: use watchers for **continuous state** (connection quality, active rendition, participant count) and streams for **discrete events** (participant joined, track published). This is a natural split — you wouldn't poll for "has anyone joined?" but you would poll for "what's the current RTT?".

## Concrete Usage Examples

## 1. Simple one-to-one call

```rust
use iroh_live::{
    Live,
    media::{
        audio_backend::AudioBackend,
        capture::CameraCapturer,
    },
    tickets::CallTicket,
};

let live = Live::builder().spawn().await?;
let audio = AudioBackend::default();

let mut call = live.call(CallTicket::from_str(ticket)?).await?;

call.local_participant()
    .publish_camera(CameraCapturer::new()?, Default::default())
    .await?;

call.local_participant()
    .publish_microphone(audio.default_input().await?, Default::default())
    .await?;

let remote_camera = call.remote_participant()
    .publications()
    .find(|p| p.source() == TrackSource::Camera)
    .unwrap()
    .subscribe_video(Default::default())
    .await?;
```

## 2. Accepting inbound direct calls

```rust
let live = Live::builder().spawn().await?;
let mut incoming = live.incoming_calls();

while let Some(incoming) = incoming.next().await {
    println!("incoming from {}", incoming.remote_id());

    let mut call = incoming.accept().await?;

    call.local_participant()
        .publish_microphone(audio.default_input().await?, Default::default())
        .await?;

    while let Some(event) = call.events().next().await {
        match event {
            CallEvent::TrackPublished { participant_id, publication } => {
                if publication.source() == TrackSource::Camera {
                    let video = publication.subscribe_video(Default::default()).await?;
                    attach_video(video);
                }
            }
            CallEvent::Closed => break,
            _ => {}
        }
    }
}
```

## 3. Simple room API

```rust
let live = Live::builder().spawn().await?;
let room = live.create_room(Default::default()).await?;

room.local_participant()
    .publish_camera(CameraCapturer::new()?, Default::default())
    .await?;

room.local_participant()
    .publish_microphone(audio.default_input().await?, Default::default())
    .await?;

let ticket = room.ticket();
println!("{ticket}");

let mut events = room.events();
while let Some(event) = events.next().await {
    match event {
        RoomEvent::TrackPublished { publication, .. } if publication.source() == TrackSource::Camera => {
            let video = publication.subscribe_video(Default::default()).await?;
            attach_tile(video);
        }
        _ => {}
    }
}
```

## 4. Manual processing room API

```rust
use iroh_live::media::{
    codec::H264Encoder,
    format::{DecodeConfig, VideoPreset},
};

let publication = room.local_participant()
    .publish_video_track(
        LocalVideoTrack::from_source(CameraCapturer::new()?)
            .with_encoder(H264Encoder::with_preset(VideoPreset::P720)?),
        PublishTrackOptions {
            source: TrackSource::Camera,
            ..Default::default()
        }
    )
    .await?;

let remote_pub = room.remote_participants()
    .flat_map(|p| p.publications())
    .find(|p| p.source() == TrackSource::ScreenShare)
    .unwrap();

let screen = remote_pub.subscribe_video(SubscribeVideoOptions {
    decode: DecodeConfig::default(),
    quality: Quality::High,
    ..Default::default()
}).await?;
```

## Decision Criteria

To choose among the alternatives, I would optimize for these criteria in order:

1. Can a new user discover the happy path for a call or room without understanding MoQ internals?
2. Can an advanced user still build a custom capture/encode/subscribe pipeline without fighting the API?
3. Can incoming direct connections be handled explicitly and safely?
4. Can the library evolve without breaking app code every time transport/media internals shift?
5. Can examples become materially shorter and more obvious?

Against those criteria:

- Alternative A fails 1 and 4
- Alternative B improves 2 and 4 but is weaker on 1
- Alternative C improves 1 strongly but weakens 2
- Alternative D is the best overall balance
- Alternative E is useful only as a supporting construction style

> **[Note]:** Add a 6th criterion: **Can the API serve non-RTC use cases without feeling forced?** Video distribution, audio studio links, one-to-many streaming, and data-channel-only sessions are all valid MoQ use cases. The API should not require users to pretend they're in a "room" or on a "call" when they're just streaming video to N viewers. This is where the broadcast layer (Alternative B's strength) must remain accessible and first-class, not buried under room/call abstractions. See the "Beyond RTC" section at the end of this document.

## Specific Structural Changes I Recommend

## 1. Make `Live` a builder-based entry point

Current:

- `Live::new(endpoint)`
- `LiveNode::spawn_from_env()`

Recommended:

```rust
let live = Live::builder()
    .secret_key(secret)
    .gossip(true)
    .spawn()
    .await?;
```

And:

```rust
let node = LiveNode::builder()
    .from_env()
    .spawn()
    .await?;
```

This gives a single obvious construction story.

## 2. Hide `pub moq: Moq`

`Live` should not expose its raw transport field publicly.
Advanced access can be:

```rust
live.lowlevel().moq()
```

or a feature-gated advanced module.

## 3. Add a real broadcast module

I now think this is a required part of the final shape.

Suggested module:

```rust
pub mod broadcast {
    pub struct LocalBroadcast { ... }
    pub struct RemoteBroadcast { ... }
    pub struct VideoTarget { ... }
    pub struct AudioTarget { ... }
    pub struct BroadcastCatalog { ... }
}
```

This gives advanced users a coherent API without forcing them into full room/call semantics or raw MoQ plumbing.

## 4. Split top-level API from escape hatches

I recommend:

```rust
pub mod advanced {
    pub mod moq;
    pub mod media;
}
```

or:

```rust
pub mod lowlevel;
```

The important part is not the exact module name. It is having a clearly non-default place for raw transport/media wiring.

## 5. Stop exposing `BroadcastProducer` in room/call happy-path APIs

Today:

- `Live::publish(name, producer)`
- `RoomHandle::publish(name, producer)`
- `PublishCaptureController::set_opts()` returning `new_screen`

Instead:

- publication should happen through `LocalParticipant`
- advanced publication should use a dedicated `publish_custom_*` API

## 6. Add stable IDs

Add:

- `RoomId`
- `ParticipantId`
- `PublicationId`

Do not make string track names the main stable identity.

> **[Note]:** These should be **newtype wrappers** with derive macros, not raw types:
>
> ```rust
> /// Uniquely identifies a participant in a room or call.
> #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
> pub struct ParticipantId(pub(crate) EndpointId);
>
> /// Uniquely identifies a publication within a session.
> #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
> pub struct PublicationId(pub(crate) u64);
>
> /// Uniquely identifies a room.
> #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
> pub struct RoomId(pub(crate) [u8; 32]); // gossip topic hash
> ```
>
> The `pub(crate)` inner field lets internal code access the raw value while preventing users from constructing fake IDs. Derive `Hash` + `Eq` so they work as map keys. Derive `Copy` for the small ones (ParticipantId, PublicationId) so they're ergonomic to pass around.
>
> `ParticipantId` wrapping `EndpointId` is natural — the iroh endpoint ID *is* the participant identity in a p2p system. `RoomId` could derive from the gossip topic. `PublicationId` is trickier — it needs to be stable across reconnects. Consider using a hash of `(participant_id, source_type, sequence_number)`.
>
> Implement `Display` for all of them (short hex for debugging), and `From<ParticipantId> for EndpointId` for the escape hatch.

## 7. Decouple source semantics from transport grouping

Internally, you may still choose to pack camera+microphone together in one broadcast and screen share in another.
That is fine.

But the public API should describe:

- local camera publication
- local microphone publication
- local screen-share publication

Then map those onto internal broadcast groupings as an implementation detail.

## 8. Unify audio/video subscription naming

Instead of:

- `watch`
- `listen`
- `watch_and_listen`

prefer:

- `subscribe_video`
- `subscribe_audio`
- `subscribe`

That is more obvious and consistent.

## 9. Treat local preview as a local track, not a special broadcast trick

Instead of:

- `PublishBroadcast::watch_local()`

prefer:

- `LocalTrackPublication::preview()`
- `LocalVideoTrack::preview()`

That is what users mean.

> **[Note]:** The current `watch_local()` implementation is clever — it creates a `WatchTrack::from_video_source()` that taps the same source before encoding. The renamed `preview()` should preserve this: it should return the *pre-encode* frames (native resolution, no compression artifacts), not round-trip through encode/decode. This matters for camera viewfinders.
>
> The return type should be the same `VideoSubscription` (or `WatchTrack`) as remote video — same `current_frame()` / `next_frame()` interface. This is good for UI code that doesn't care whether it's rendering local or remote video.

## Additional Current-to-Proposed Mapping

| Current | Better mid-level replacement | Better top-level replacement |
|---|---|---|
| `SubscribeBroadcast` | `RemoteBroadcast` | `RemoteTrackPublication` + `subscribe_*` |
| `PublishBroadcast` | `LocalBroadcast` | `LocalParticipant::publish_*` |
| `watch_rendition` | `subscribe_video(SubscribeVideoOptions { target: ... })` | same |
| `listen_rendition` | `subscribe_audio(SubscribeAudioOptions { target: ... })` | same |
| `watch_local` | `preview()` | `LocalTrackPublication::preview()` |

## Migration Strategy

> **[Note]:** A critical Rust-specific migration concern: **semver compatibility**. Every type rename or method signature change is a breaking change. The migration should be planned with this in mind:
>
> 1. **Phase 1-2 can be additive** — new types wrap old ones, old types get `#[deprecated]` attributes. No breakage.
> 2. **Phase 3-5 are breaking** — they change the primary API. Bundle these into one semver-major release.
> 3. **Use `#[deprecated(since = "0.x", note = "use RemoteBroadcast instead")]`** liberally during transition. Cargo will show deprecation warnings without failing builds.
> 4. **Consider a `compat` feature flag** that keeps old type aliases during the transition period.
>
> For an early-stage crate (pre-1.0), this is less critical — breaking changes are expected. But the habit of additive-first migration is worth building now.

## Phase 1: Add broadcast object layer without breaking internals

- introduce `LocalBroadcast` and `RemoteBroadcast`
- implement them by wrapping `PublishBroadcast` and `SubscribeBroadcast`
- move the current functionality behind better names first

This is the safest first step because it improves ergonomics without requiring the full room/call redesign to land at once.

## Phase 2: Add new top-level API without breaking internals

- introduce new high-level types
- implement them by wrapping `MoqSession`, `LocalBroadcast`, `RemoteBroadcast`, and room actor logic
- keep current APIs public but mark them as lower-level

## Phase 3: Add incoming-call acceptance primitives

- add `Moq::incoming()`
- add `Live::incoming_calls()`
- add `IncomingCall`

This phase needs real implementation work and should be prioritized early.

## Phase 4: Move room internals to participant/publication model

- track per-remote participant
- attach publications to participant state
- emit high-level participant/track events

## Phase 5: Replace controller-shaped publishing API

- introduce `LocalParticipant` publish/unpublish methods
- turn `PublishCaptureController` into an advanced/local implementation detail or rename it to `CaptureManager`

## Phase 6: De-emphasize raw transport types in examples

- examples should use `Room`, `Call`, `Participant`, `Publication`
- advanced examples can still show raw pipelines and low-level transport

## Revised Priority Recommendations

If I had to prioritize the work:

1. add a broadcast object layer (`LocalBroadcast` / `RemoteBroadcast`)
2. add incoming call acceptance API
3. define the participant/publication object model
4. redesign room events around participants and tracks
5. redesign local publish APIs around `LocalParticipant`
6. hide transport-first types from the default surface
7. clean up naming (`CallTicket`, `subscribe_*`, `TrackSource`, etc.)

## Concrete Suggestion

If I were making the call for this repo, I would choose Alternative D and implement it in this order:

## Suggestion: Use a three-layer API, with `RemoteBroadcast` / `LocalBroadcast` as the bridge

### Why this suggestion

- It fits both target audiences:
  - people who want "simple RTC rooms/calls"
  - people who want "manual processing and setup"
- It reuses existing strengths rather than fighting them
- It learns the right lessons from both LiveKit and Hang JS
- It gives a realistic migration path from the current codebase

### Concrete first implementation target

First, add:

- `broadcast::RemoteBroadcast`
- `broadcast::LocalBroadcast`
- `Moq::incoming()`

Then build:

- `Call`
- `IncomingCall`
- `Room` participant/publication wrappers

This order matters.
If you try to jump straight to `Room`/`Call` without a better broadcast object layer, you will either:

- duplicate the existing rough edges under new names, or
- overfit the API around rooms and make manual usage worse

### Concrete naming suggestion

I would use:

- `CallTicket` instead of `LiveTicket`
- `RemoteBroadcast` / `LocalBroadcast`
- `RemoteTrackPublication` / `LocalTrackPublication`
- `subscribe_video` / `subscribe_audio`
- `TrackSource::{Camera, Microphone, ScreenShare, Custom}`

I would avoid:

- `watch_*`
- `listen_*`
- `Sync` suffixes for async-backed objects
- `Opts` bags as the main durable abstraction

### Concrete non-suggestion

I would not try to make the first redesign pass solve:

- every possible watcher/signal abstraction
- rich room metadata semantics
- active speaker / moderation / permissions
- generalized chat/location/user APIs at the top layer

Those can remain broadcast-level or advanced concerns until the core media model is right.

### Short version

Recommended path:

1. formalize broadcast objects
2. add explicit incoming acceptance
3. build participant/publication APIs on top
4. keep raw transport/media as explicit escape hatches

This is the cleanest path to an ergonomic API without throwing away what already works.

## Bottom Line

The repo already has credible media and transport machinery.
What it lacks is a clean product API layer.

The biggest missing piece is not codec support or transport capability.
It is the lack of a first-class object model for:

- incoming calls
- participants
- publications
- subscriptions

That is why the current API feels lower-level than it should, and why application code has to compensate so much.

My recommendation is to keep the current low-level building blocks, add a proper broadcast layer, and build a new primary API around:

- `Live`
- `Call`
- `Room`
- `LocalBroadcast` / `RemoteBroadcast`
- `Participant`
- `TrackPublication`
- `RemoteTrack` / `LocalTrack`

with explicit incoming acceptance support and transport details moved below the default surface.

## Beyond RTC: Non-Room, Non-Call Use Cases

The review above is heavily weighted toward RTC-style rooms and calls. But MoQ is a general-purpose media transport, and iroh-live should not accidentally make non-RTC use cases second-class citizens. This section examines how the proposed API fits use cases that don't involve bidirectional multi-party communication.

### The risk of RTC tunnel vision

If the only top-level concepts are `Room`, `Call`, and `Participant`, then users building:

- a live video distribution system (one publisher, many viewers)
- an audio studio link (low-latency one-way audio monitoring)
- a security camera viewer (many sources, one dashboard)
- a media recording/archival pipeline (subscribe and write to disk)
- a transcoding relay (subscribe, transcode, republish)

would be forced to either:

1. Use `Room`/`Call` and ignore half the API (wasteful, confusing)
2. Drop down to the raw transport layer (losing all ergonomics)

Neither is acceptable. The broadcast layer must be a first-class entry point, not just an implementation detail behind rooms.

### Example: Live video distribution (one-to-many)

A streamer publishes to N viewers. No rooms, no participants, no bidirectional communication.

```rust
use iroh_live::{Live, broadcast::LocalBroadcast};
use moq_media::{
    capture::CameraCapturer,
    codec::VideoCodec,
    format::VideoPreset,
};

// Publisher side
let live = Live::builder().spawn().await?;

let mut broadcast = live.create_broadcast("live-stream");
broadcast.set_video(
    CameraCapturer::new()?,
    VideoCodec::best_available(),
    &[VideoPreset::P720, VideoPreset::P1080],
)?;
broadcast.set_audio(
    audio.default_input().await?,
    AudioCodec::Opus,
    &[AudioPreset::Hq],
)?;

let ticket = broadcast.ticket(); // shareable join handle
println!("viewers connect with: {ticket}");

// The broadcast runs until dropped. No room needed.
// Optionally monitor viewer count:
let mut stats = broadcast.stats();
loop {
    tokio::select! {
        _ = stats.changed() => {
            println!("viewers: {}", broadcast.subscriber_count());
        }
        _ = tokio::signal::ctrl_c() => break,
    }
}
```

```rust
// Viewer side
let live = Live::builder().spawn().await?;

let remote = live.subscribe_broadcast(ticket).await?;
let video = remote.subscribe_video(Quality::High).await?;
let audio = remote.subscribe_audio(&audio_backend).await?;

// Render loop — identical to the room/call case
loop {
    if let Some(frame) = video.current_frame() {
        render(frame);
    }
    tokio::time::sleep(Duration::from_millis(16)).await;
}
```

Key point: no `Room`, no `Participant`, no `Call`. Just `LocalBroadcast` and `RemoteBroadcast`. The broadcast layer is the API.

### Example: Audio studio link (low-latency one-way audio)

A recording studio sends a monitor mix to a remote musician. Ultra-low latency, audio only, one direction.

```rust
// Studio side — publish audio-only broadcast
let live = Live::builder().spawn().await?;

let mut broadcast = live.create_broadcast("monitor-mix");
broadcast.set_audio(
    studio_output_source, // custom AudioSource from DAW
    AudioCodec::Opus,
    &[AudioPreset::Hq],
)?;

let ticket = broadcast.ticket();
// Send ticket to remote musician via side channel
```

```rust
// Musician side — subscribe to audio only
let live = Live::builder().spawn().await?;

let remote = live.subscribe_broadcast(ticket).await?;
let audio = remote.subscribe_audio(&audio_backend).await?;

// Audio plays through speakers automatically via AudioSink
// Monitor until done
audio.stopped().await;
```

This shows why `subscribe_video` and `subscribe_audio` should be independent methods, not bundled into `watch_and_listen`. A studio link has no video.

### Example: Security camera dashboard (many sources, one viewer)

A dashboard subscribes to N camera feeds simultaneously.

```rust
let live = Live::builder().spawn().await?;

let camera_tickets: Vec<BroadcastTicket> = load_camera_config();
let mut feeds = Vec::new();

for ticket in camera_tickets {
    let remote = live.subscribe_broadcast(ticket).await?;
    let video = remote.subscribe_video(
        VideoTarget::default().max_pixels(640 * 480) // thumbnail quality
    ).await?;
    feeds.push((remote, video));
}

// Render all feeds in a grid
loop {
    for (i, (_remote, video)) in feeds.iter().enumerate() {
        if let Some(frame) = video.current_frame() {
            render_tile(i, frame);
        }
    }
    tokio::time::sleep(Duration::from_millis(33)).await;
}
```

This is N independent subscriptions with no room or peer coordination. The `VideoTarget::max_pixels()` hint is critical here — the dashboard doesn't need full-res from every camera.

### Example: Media recording pipeline (subscribe and archive)

A service subscribes to a broadcast and writes frames to disk. No rendering, no UI.

```rust
let live = Live::builder().spawn().await?;
let remote = live.subscribe_broadcast(ticket).await?;

// Subscribe at highest quality for archival
let mut video = remote.subscribe_video(Quality::Highest).await?;

let mut writer = mp4_writer::create("recording.mp4")?;
while let Some(frame) = video.next_frame().await {
    writer.write_frame(&frame)?;
}
writer.finalize()?;
```

This shows why `next_frame()` (async, blocking) must coexist with `current_frame()` (non-blocking, latest). Recording pipelines want every frame in order; render loops want the latest frame and skip stale ones.

### Example: Transcoding relay (subscribe, transcode, republish)

A relay subscribes to a high-quality source and republishes at lower quality.

```rust
let live = Live::builder().spawn().await?;

// Subscribe to source
let source = live.subscribe_broadcast(source_ticket).await?;
let mut video = source.subscribe_video(Quality::Highest).await?;

// Create output broadcast with lower renditions
let mut output = live.create_broadcast("relay-720p");

// Manual pipeline: decode → re-encode → publish
// This is where the raw/advanced API earns its keep
let encoder = VideoCodec::H264.create_encoder(VideoPreset::P720)?;
let mut renditions = VideoRenditions::empty(video.as_source());
renditions.add(VideoCodec::H264, VideoPreset::P720);
output.set_video_renditions(renditions)?;

let ticket = output.ticket();
println!("relay available at: {ticket}");

// Keep relay alive
tokio::signal::ctrl_c().await?;
```

This is the most compelling case for the three-layer design: the relay uses both the broadcast layer (subscription, republishing) and touches the codec layer (manual encoder selection). It would be awkward with only `Room`/`Call` or only raw transport.

### How the proposed API accommodates these use cases

| Use case | Primary API layer | Types used |
|---|---|---|
| Video call | Product (Room/Call) | `Call`, `LocalParticipant`, `RemoteTrackPublication` |
| Multi-party room | Product (Room) | `Room`, `RoomEvent`, `Participant` |
| Live streaming | Broadcast | `LocalBroadcast`, `RemoteBroadcast` |
| Audio studio link | Broadcast | `LocalBroadcast`, `RemoteBroadcast` (audio only) |
| Camera dashboard | Broadcast | N × `RemoteBroadcast` |
| Recording pipeline | Broadcast + Raw | `RemoteBroadcast`, manual frame access |
| Transcoding relay | Broadcast + Raw | `RemoteBroadcast` + `LocalBroadcast` + codec API |

The broadcast layer serves as the universal mid-level API. Every use case passes through it. The product layer (Room/Call) is sugar on top for the interactive cases. The raw layer is the escape hatch for pipelines and processing.

### Design implications

1. **`LocalBroadcast` and `RemoteBroadcast` must be directly constructable from `Live`**, not only reachable through `Room` or `Call`. The `live.create_broadcast()` and `live.subscribe_broadcast()` methods are essential.

2. **`BroadcastTicket` needs to be a standalone type**, separate from `RoomTicket` and `CallTicket`. A broadcast ticket identifies one broadcast, not a room or peer.

3. **The broadcast layer should not require `AudioBackend`**. Audio-only and video-only broadcasts are valid. Don't force users to create an audio context for a video-only subscription.

4. **Subscriber count / connection stats should be available on `LocalBroadcast`**. Streamers need to know how many people are watching. This maps to the number of active sessions subscribing to the broadcast's tracks.

5. **Long-running session lifecycle matters more than call semantics**. A studio link or camera feed might run for hours or days. The API should not assume short-lived sessions:
   - Automatic reconnection policy on `RemoteBroadcast` (configurable)
   - Graceful handling of source restarts (camera power cycle → catalog update → subscriber re-subscribes)
   - Memory-bounded frame buffers (no unbounded growth over hours)

6. **Consider a `Session` type as a mid-level primitive between `Live` and `Broadcast`**. A session represents one QUIC connection to a peer and can carry multiple broadcasts. This matters for the relay and dashboard use cases where you have multiple broadcasts over one connection:

```rust
let session = live.connect(remote_addr).await?;
let feed_a = session.subscribe_broadcast("camera-1").await?;
let feed_b = session.subscribe_broadcast("camera-2").await?;
// Both subscriptions share one QUIC connection
```

This is already how `MoqSession` works internally — just needs a better name and positioning in the broadcast layer.

### Naming refinement

Given these use cases, the naming should be checked against non-RTC contexts:

| Type | Works for RTC? | Works for streaming? | Works for pipelines? |
|---|---|---|---|
| `Room` | yes | no (wrong concept) | no |
| `Call` | yes | no | no |
| `LocalBroadcast` | yes | yes | yes |
| `RemoteBroadcast` | yes | yes | yes |
| `Participant` | yes | awkward ("viewer"?) | no |
| `TrackPublication` | yes | yes | yes |
| `VideoSubscription` | yes | yes | yes |

The broadcast-layer names are universally applicable. The product-layer names are RTC-specific, which is fine — they're meant to be. The key is that users who don't need rooms/calls never have to touch those types.

### Conclusion

The three-layer design (Alternative D) handles non-RTC use cases well, **provided the broadcast layer is treated as a first-class public API**, not just an implementation detail or stepping stone to rooms. The API surface should make it obvious that:

- `Live` → `LocalBroadcast` / `RemoteBroadcast` is a complete, supported path
- `Live` → `Room` / `Call` is a higher-level convenience built on top
- Both paths share the same underlying transport and codec machinery
