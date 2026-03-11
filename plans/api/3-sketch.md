# API Sketch

Rust code sketch of the three-layer `iroh-live` API redesign. All method bodies
use `todo!()`. Type signatures are real and intended to compile (modulo the
`todo!()` bodies). The goal is to nail down the public surface before
implementation.

See `0-overview.md` for context on the three layers and design principles.

### Review notes (2026-03-10)

Changes from detailed review pass:

- **`accept_call()` instead of `incoming_calls() -> Stream`**: Socket-style `async fn accept_call()` returns one `IncomingCall` at a time. Simpler to implement (single channel or manual future), easier to use, no `Stream` import needed.
- **Same pattern for `Room::recv()`**: Keep `async fn recv()` style for room events instead of `events() -> impl Stream`. Consistent with the `accept_call` pattern and with the current codebase's `Room::recv()`.
- **Removed `VideoTarget` enum**: Premature abstraction with one variant. Removed.
- **Fixed `CallState` / `DisconnectReason` PartialEq conflict**: `dyn Error` doesn't impl `PartialEq`. Removed `PartialEq` from `CallState`, added helper methods instead.
- **Added codec parameter to `publish_video`**: Was missing — users need to choose H264 vs AV1.
- **Renamed `watch_local` → `preview`**: As discussed in review doc.
- **Made ticket/ID fields `pub(crate)`**: Users shouldn't construct IDs or tickets from raw fields. Added accessor methods.
- **Removed `RemoteBroadcast::subscribe_video_rendition`**: Redundant with `subscribe_video_with(opts.rendition("name"))`. Fewer methods = less surface to learn.
- **`Call::remote()` returns `Option`**: Remote broadcast doesn't exist until the peer announces.
- **Added `Live::new()` taking gossip too**: Entry point needs gossip for rooms.
- **`LocalVideoSlot` / `LocalAudioSlot`**: Slot-based API. Supports source replacement (`replaceTrack()` equivalent), mute/enable without teardown.
- **`has_video()` / `has_audio()`** on both `LocalBroadcast` and `RemoteBroadcast`.
- **`SelectedVideo` / `SelectedAudio`** snapshots on `RemoteBroadcast` for UI and debugging.
- **`Live::builder()` + `LiveBuilder`**: Ergonomic construction with defaults. `from_env()` for environment-based config. Matches north star's one-liner setup.
- **`Live::publish_broadcast(name, &broadcast)`**: Returns `BroadcastTicket`. The broadcast-layer way to publish to the network. Publishing is a `Live` concern, not a `LocalBroadcast` concern — `LocalBroadcast` is a pure media wrapper.
- **`Room::participants()` watcher**: Continuous state watcher over participant list (pattern #10). Enables reactive UIs without polling `recv()`.
- **Multi-broadcast participant model**: Each participant publishes N broadcasts (camera, screen, custom). This matches the actual hang/moq model where camera+mic and screen are separate broadcasts with separate catalogs.
- **Removed `LocalTrackPublication`**: Publishing is now done through `LocalBroadcast` slot API. Mute/enable/clear are on the slots. No need for a separate product-layer publication handle on the local side.

### Review notes (2026-03-11)

Changes from second review pass:

- **`BroadcastKind` enum**: Replaces hard-coded `camera()`/`screen()` methods with a type-safe extensible enum: `Camera`, `Screen`, `Named(String)`. All broadcast getter methods take `impl Into<BroadcastKind>`. Enables `call.local(Camera)` and `call.local("custom")` with the same API.
- **`CallTicket` includes `call_id`**: MoQ protocol requires a broadcast name for subscription. `call_id` is the broadcast name the callee will publish under. Generated randomly by default.
- **`hangup()` → `close()`**: More neutral, consistent with `Room::leave()`, better for API that handles both calls and non-calls.
- **`RemoteParticipant::wait_broadcast(kind)`**: Async method to wait for a specific broadcast to appear. Essential for call flows where remote media arrives after connection.
- **Removed `RemoteTrackPublication`**: Broadcasts are the unit of publication in MoQ. The extra indirection added confusion without value. Room events are now `BroadcastPublished`/`BroadcastUnpublished`. Subscribe directly on `RemoteBroadcast`.
- **Removed `PublicationId` and `TrackKind`**: With broadcasts as the publication unit, per-track IDs at the product layer are unnecessary. Track discovery happens through the broadcast's catalog.
- **Removed `RemoteVideoTrack` / `RemoteAudioTrack` wrappers**: `VideoTrack` already provides `current_frame()` and `AudioTrack` already plays automatically. Thin wrappers that add no state are not worth the indirection. Use `moq_media` types directly.
- **Simplified slots**: Removed `publish_camera()`/`publish_screen()`/`publish_microphone()` — these are synonymous with `set_source()`. The broadcast's `BroadcastKind` already carries the semantic meaning (camera vs screen). Slots have one method: `set_source()`.
- **Justified all wrappers**: Each wrapper type documents what state it adds beyond the wrapped type. `RemoteBroadcast` adds: status watchable, selection snapshots, has_video/has_audio. `LocalBroadcast` adds: Arc+Mutex for &self API, slot model, publish(). Types that add nothing are removed.
- **`Call::recv_remote()`**: Async method that yields the next `(BroadcastKind, RemoteBroadcast)` as they are announced. Complements `wait_remote(kind)` for dynamic subscription patterns (handle camera, screen, or custom broadcasts as they arrive).
- **Renamed `WatchTrack` → `VideoTrack`** (in moq-media): `WatchTrack` is an implementation-detail name. `VideoTrack` is the semantic counterpart to `AudioTrack`, consistent and self-describing. This is a moq-media rename that should happen in Phase 1.
- **Screen-share in examples**: Call and room examples now demonstrate multi-broadcast complexity (camera + screen) to show the `BroadcastKind` pattern in practice.
- **Return by value, not reference**: All methods returning `LocalBroadcast`, `RemoteBroadcast`, etc. return owned (Arc-cloned) values, not references. Returning `&T` would require holding `RwLock` guards across the return boundary, which is unsound. Arc-clone is cheap and these types are designed for it.
- **Codec parameter on `set_source`**: Slots take `(source, codec, presets)` — e.g. `set_source(cam, VideoCodec::H264, [P720])`. The codec is a real choice (H264 vs AV1, software vs hardware) that shouldn't be hidden. Maps directly to `VideoRenditions::new()`. For advanced codec control, use `LocalBroadcast::producer()` directly.
- **`LocalBroadcast::publish()` removed**: `LocalBroadcast` is a pure media wrapper with no networking knowledge. Publishing happens through `Live::publish_broadcast(name, &broadcast)` for standalone use, or internally through the room actor / call code. The `producer()` escape hatch is the bridge.

## Wrapping justification

Every wrapper in this sketch must add state or API surface that the inner type does not provide. This section documents the rationale:

| Wrapper | Inner type | What it adds | Justified? |
|---------|-----------|-------------|-----------|
| `LocalBroadcast` | `PublishBroadcast` | Arc+Mutex (inner needs `&mut self`), slot model (`LocalVideoSlot`/`LocalAudioSlot`), `producer()` escape hatch for transport | Yes: inner has `&mut self` API, no slot abstraction |
| `RemoteBroadcast` | `SubscribeBroadcast` | `BroadcastStatus` watchable (Connecting/Live/Ended), selection snapshots (`SelectedVideo`/`SelectedAudio`), `has_video()`/`has_audio()` convenience | Yes: inner has no connection-lifecycle state or selection memory |
| `LocalVideoSlot` / `LocalAudioSlot` | N/A (composed) | Slot abstraction with set/clear/enable/mute. No inner type to wrap — this is new API surface. | Yes: new concept |

Types that were considered for wrapping but rejected:

| Rejected wrapper | Why rejected |
|-----------------|-------------|
| `RemoteVideoTrack` wrapping `VideoTrack` | `VideoTrack` already has `current_frame()`. Wrapper would add zero state. Use `VideoTrack` directly. |
| `RemoteAudioTrack` wrapping `AudioTrack` | `AudioTrack` already plays automatically. Wrapper would add zero state. Use `AudioTrack` directly. |
| `RemoteTrackPublication` | Broadcasts are the publication unit in MoQ. An extra track-level handle between participant and broadcast added confusion. Subscribe directly on `RemoteBroadcast`. |

## `types.rs` -- Shared newtypes and IDs

```rust
//! Shared identifier and option types used across the `iroh_live` API.

use std::{fmt, str::FromStr, sync::Arc};

use iroh::EndpointId;
use serde::{Deserialize, Serialize};

/// Identifies a participant within a room or call.
///
/// Wraps an [`EndpointId`] to provide domain-specific meaning. Two
/// participants with the same underlying endpoint always compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParticipantId(pub(crate) EndpointId);

impl ParticipantId {
    /// Returns the underlying [`EndpointId`].
    pub fn as_endpoint_id(&self) -> &EndpointId {
        &self.0
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.fmt_short())
    }
}

impl From<EndpointId> for ParticipantId {
    fn from(id: EndpointId) -> Self {
        Self(id)
    }
}

/// Identifies the kind of broadcast a participant publishes.
///
/// A participant typically publishes one or two broadcasts: a `Camera`
/// broadcast (with video and/or audio) and optionally a `Screen` broadcast
/// (with screen-capture video and optional system audio). Custom workflows
/// can use `Named` for additional broadcasts.
///
/// Implements `From<&str>` so callers can pass string literals for custom
/// broadcast names without constructing the enum explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BroadcastKind {
    /// Camera video and microphone audio.
    Camera,
    /// Screen-share video and optional system audio.
    Screen,
    /// Custom named broadcast for advanced workflows.
    Named(String),
}

impl BroadcastKind {
    /// Returns the wire name used in the MoQ protocol.
    pub fn as_str(&self) -> &str {
        match self {
            BroadcastKind::Camera => "camera",
            BroadcastKind::Screen => "screen",
            BroadcastKind::Named(name) => name,
        }
    }
}

impl fmt::Display for BroadcastKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for BroadcastKind {
    fn from(s: &str) -> Self {
        match s {
            "camera" => BroadcastKind::Camera,
            "screen" => BroadcastKind::Screen,
            other => BroadcastKind::Named(other.to_string()),
        }
    }
}

impl From<String> for BroadcastKind {
    fn from(s: String) -> Self {
        match s.as_str() {
            "camera" => BroadcastKind::Camera,
            "screen" => BroadcastKind::Screen,
            _ => BroadcastKind::Named(s),
        }
    }
}

/// Identifies a room by its gossip topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomId(pub(crate) iroh_gossip::TopicId);

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", data_encoding::HEXLOWER.encode(self.0.as_bytes()))
    }
}

/// Describes why a call or room connection ended.
#[derive(Debug, Clone)]
pub enum DisconnectReason {
    /// Local side initiated the disconnect.
    LocalClose,
    /// Remote side closed the connection.
    RemoteClose,
    /// Transport-level failure.
    TransportError(Arc<dyn std::error::Error + Send + Sync>),
}
```

## `lib.rs` -- Crate root and re-exports

```rust
//! Real-time media sessions over iroh.
//!
//! This crate provides a three-layer API:
//!
//! - **Product API** (crate root): [`Live`], [`Call`], [`Room`], participants,
//!   and tickets for building end-user applications.
//! - **Broadcast API** ([`broadcast`]): [`LocalBroadcast`], [`RemoteBroadcast`],
//!   and [`BroadcastTicket`] for streaming, studio links, and media pipelines.
//! - **Transport API** ([`transport`]): Direct access to [`MoqSession`] and
//!   raw protocol types.

mod call;
mod live;
mod room;
mod types;

pub mod broadcast;
pub mod transport;

pub use self::call::{Call, CallTicket, IncomingCall};
pub use self::live::Live;
pub use self::room::{Room, RoomEvent, RoomTicket};
pub use self::types::{BroadcastKind, DisconnectReason, ParticipantId, RoomId};

// Participant types from room module.
pub use self::room::{LocalParticipant, RemoteParticipant};

// Re-export media crate for codec/pipeline access (layer 3).
pub use moq_media as media;
```

## `live.rs` -- Entry point

```rust
//! Entry point for creating calls, rooms, and broadcasts.

use std::sync::Arc;

use iroh::Endpoint;
use n0_error::Result;
use crate::broadcast::{LocalBroadcast, RemoteBroadcast, BroadcastTicket};
use crate::call::{Call, CallTicket, IncomingCall};
use crate::room::{Room, RoomTicket};
use crate::transport::MoqSession;

/// Central entry point for all `iroh-live` operations.
///
/// Holds the iroh [`Endpoint`] and manages protocol handlers, connection
/// pooling, and broadcast publishing. All methods take `&self` and use
/// interior mutability. Cloning is cheap (Arc-based).
#[derive(Debug, Clone)]
pub struct Live {
    inner: Arc<LiveInner>,
}

#[derive(Debug)]
struct LiveInner {
    endpoint: Endpoint,
    // moq: iroh_moq::Moq,
    // gossip: iroh_gossip::Gossip,
}

impl Live {
    /// Creates a new [`Live`] instance from an iroh [`Endpoint`] and gossip handle.
    pub fn new(endpoint: Endpoint, gossip: iroh_gossip::Gossip) -> Self {
        todo!()
    }

    /// Returns a builder for configuring and creating a [`Live`] instance.
    ///
    /// The builder provides sensible defaults and environment-based configuration.
    /// Preferred over [`new`](Self::new) for most use cases.
    pub fn builder() -> LiveBuilder {
        todo!()
    }

    /// Returns the underlying iroh [`Endpoint`].
    pub fn endpoint(&self) -> &Endpoint {
        todo!()
    }

    // -- Calls --

    /// Initiates a one-to-one call to a remote peer.
    pub async fn call(&self, ticket: impl Into<CallTicket>) -> Result<Call> {
        todo!()
    }

    /// Waits for the next incoming call from a remote peer.
    ///
    /// Returns `Ok(IncomingCall)` when a new call arrives, or `Err` if the
    /// listener is shut down. Each [`IncomingCall`] must be explicitly accepted
    /// or rejected. Dropping an [`IncomingCall`] without calling
    /// [`accept`](IncomingCall::accept) automatically rejects it.
    pub async fn accept_call(&self) -> Result<IncomingCall> {
        todo!()
    }

    // -- Rooms --

    /// Joins or creates a multi-party room with default options (P2P only).
    pub async fn join_room(&self, ticket: impl Into<RoomTicket>) -> Result<Room> {
        todo!()
    }

    /// Joins or creates a multi-party room with custom options.
    ///
    /// Use [`RoomOpts`] to configure relay support, path preference, and
    /// other room-level settings. See `7-relay.md` for relay integration.
    pub async fn join_room_with_opts(
        &self,
        ticket: impl Into<RoomTicket>,
        opts: crate::relay::RoomOpts,
    ) -> Result<Room> {
        todo!()
    }

    // -- Relay --

    /// Connects to a moq-relay server.
    ///
    /// Returns a [`Relay`](crate::relay::Relay) handle for publishing and
    /// subscribing broadcasts through the relay. See `7-relay.md`.
    pub async fn connect_relay(
        &self,
        remote: impl Into<iroh::EndpointAddr>,
    ) -> Result<crate::relay::Relay> {
        todo!()
    }

    // -- Broadcasts (layer 2 convenience) --

    /// Creates a local broadcast that can be published to peers.
    pub fn create_broadcast(&self) -> LocalBroadcast {
        todo!()
    }

    /// Publishes a local broadcast under the given name.
    ///
    /// The broadcast becomes reachable by remote peers once published.
    /// Returns a [`BroadcastTicket`] that can be shared with subscribers.
    pub fn publish_broadcast(
        &self,
        name: impl ToString,
        broadcast: &LocalBroadcast,
    ) -> Result<BroadcastTicket> {
        todo!()
    }

    /// Subscribes to a remote broadcast by ticket.
    pub async fn subscribe_broadcast(
        &self,
        ticket: impl Into<BroadcastTicket>,
    ) -> Result<RemoteBroadcast> {
        todo!()
    }

    // -- Transport (layer 3 escape hatch) --

    /// Opens a raw [`MoqSession`] to a remote endpoint.
    pub async fn connect_raw(
        &self,
        remote: impl Into<iroh::EndpointAddr>,
    ) -> Result<MoqSession> {
        todo!()
    }

    /// Shuts down all connections and background tasks.
    pub fn shutdown(&self) {
        todo!()
    }
}

/// Builder for configuring a [`Live`] instance.
///
/// Provides defaults for endpoint, gossip, and protocol handler setup.
/// Call [`spawn`](Self::spawn) to create the instance.
#[derive(Debug)]
pub struct LiveBuilder {
    // endpoint_config, gossip_config, ...
}

impl LiveBuilder {
    /// Configures the builder from environment variables (relay URLs, secrets).
    pub fn from_env(self) -> Self {
        todo!()
    }

    /// Creates the [`Live`] instance, starting background tasks.
    pub async fn spawn(self) -> Result<Live> {
        todo!()
    }
}
```

## `call.rs` -- One-to-one calls

```rust
//! One-to-one call primitives with accept/reject semantics.

use std::{fmt, str::FromStr, sync::Arc};

use n0_error::Result;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};

use crate::broadcast::{LocalBroadcast, RemoteBroadcast};
use crate::types::{BroadcastKind, DisconnectReason, ParticipantId};

/// Portable handle for initiating a call to a specific peer.
///
/// Serializes to a compact string representation suitable for sharing
/// via QR codes, URLs, or clipboard. Contains the remote endpoint address
/// and a `call_id` that names the broadcast the callee will publish under
/// (required by the MoQ protocol for subscription).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallTicket {
    /// Remote endpoint address.
    pub(crate) remote: iroh::EndpointAddr,
    /// Broadcast name the callee publishes under. Generated randomly by default.
    pub(crate) call_id: String,
}

impl CallTicket {
    /// Returns the call identifier (broadcast name for this call session).
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Returns the remote endpoint address.
    pub fn remote(&self) -> &iroh::EndpointAddr {
        &self.remote
    }
}

impl fmt::Display for CallTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

impl FromStr for CallTicket {
    type Err = iroh_tickets::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        todo!()
    }
}

impl From<iroh::EndpointAddr> for CallTicket {
    /// Creates a ticket with a randomly generated call_id.
    fn from(remote: iroh::EndpointAddr) -> Self {
        Self {
            remote,
            call_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// State of a call connection.
#[derive(Debug, Clone)]
pub enum CallState {
    /// Connecting to the remote peer.
    Connecting,
    /// Call is active with media flowing.
    Connected,
    /// Call has ended.
    Disconnected(DisconnectReason),
}

/// Active one-to-one call session.
///
/// Provides access to local and remote broadcasts keyed by [`BroadcastKind`].
/// Each side can publish camera, screen, or custom broadcasts independently.
/// Dropping the [`Call`] closes the connection and stops all media.
#[derive(Debug, Clone)]
pub struct Call {
    inner: Arc<CallInner>,
}

#[derive(Debug)]
struct CallInner {
    // session, state, locals: HashMap<BroadcastKind, LocalBroadcast>,
    // remotes: HashMap<BroadcastKind, RemoteBroadcast>, ...
}

impl Call {
    /// Returns the identity of the remote participant.
    pub fn remote_participant(&self) -> ParticipantId {
        todo!()
    }

    /// Returns a local broadcast for the given kind, creating it lazily.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use iroh_live::BroadcastKind::*;
    ///
    /// call.local(Camera).video().set_source(cam, VideoCodec::H264, presets)?;
    /// call.local(Screen).video().set_source(screen, VideoCodec::H264, presets)?;
    /// call.local("custom").video().set_source(src, VideoCodec::H264, presets)?;
    /// ```
    pub fn local(&self, kind: impl Into<BroadcastKind>) -> LocalBroadcast {
        todo!()
    }

    /// Returns the remote broadcast for the given kind, if announced.
    ///
    /// Returns `None` until the remote peer has announced a broadcast of
    /// that kind. For async waiting, use [`wait_remote`](Self::wait_remote).
    pub fn remote(&self, kind: impl Into<BroadcastKind>) -> Option<RemoteBroadcast> {
        todo!()
    }

    /// Returns all remote broadcasts announced so far.
    pub fn remotes(&self) -> std::collections::HashMap<BroadcastKind, RemoteBroadcast> {
        todo!()
    }

    /// Waits until the remote peer announces a broadcast of the given kind.
    ///
    /// Returns `Err` if the call ends before the remote announces.
    pub async fn wait_remote(
        &self,
        kind: impl Into<BroadcastKind>,
    ) -> Result<RemoteBroadcast> {
        todo!()
    }

    /// Waits for the next remote broadcast announcement.
    ///
    /// Returns `Ok((kind, broadcast))` each time the remote peer announces
    /// a new broadcast. Use this in a loop to dynamically subscribe to
    /// camera, screen, or custom broadcasts as they appear. Returns `Err`
    /// when the call ends.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// loop {
    ///     let (kind, broadcast) = call.recv_remote().await?;
    ///     match kind {
    ///         Camera => { /* subscribe to camera */ }
    ///         Screen => { /* subscribe to screen */ }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub async fn recv_remote(&self) -> Result<(BroadcastKind, RemoteBroadcast)> {
        todo!()
    }

    /// Returns a watcher for the current call state.
    pub fn state(&self) -> impl Watcher<Value = CallState> {
        todo!();
        n0_watcher::Watchable::new(CallState::Connecting).watch()
    }

    /// Returns a future that resolves when the call ends.
    pub fn closed(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        todo!();
        async {}
    }

    /// Closes the call gracefully.
    pub fn close(&self) {
        todo!()
    }
}

/// Incoming call that has not yet been accepted.
///
/// Must be accepted or rejected. Dropping without calling [`accept`](Self::accept)
/// automatically rejects the call (sends a rejection to the remote peer).
#[derive(Debug)]
pub struct IncomingCall {
    inner: Option<IncomingCallInner>,
}

#[derive(Debug)]
struct IncomingCallInner {
    // session, remote_id, ...
}

impl IncomingCall {
    /// Returns the identity of the calling participant.
    pub fn remote_participant(&self) -> ParticipantId {
        todo!()
    }

    /// Accepts the incoming call and returns an active [`Call`].
    pub async fn accept(mut self) -> Result<Call> {
        let _inner = self.inner.take().expect("accept called after consume");
        todo!()
    }

    /// Rejects the incoming call with an optional reason.
    pub fn reject(mut self, _reason: Option<&str>) {
        let _inner = self.inner.take().expect("reject called after consume");
        todo!()
    }
}

impl Drop for IncomingCall {
    /// Automatically rejects the call if dropped without accepting.
    fn drop(&mut self) {
        if let Some(_inner) = self.inner.take() {
            // Send rejection to remote peer.
        }
    }
}
```

## `room.rs` -- Multi-party rooms

```rust
//! Multi-party room with participant and broadcast tracking.

use std::{collections::HashMap, fmt, str::FromStr, sync::Arc};

use iroh::EndpointId;
use n0_error::Result;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};

use crate::broadcast::{LocalBroadcast, RemoteBroadcast};
use crate::types::{BroadcastKind, DisconnectReason, ParticipantId, RoomId};

/// Portable handle for joining or creating a room.
///
/// Contains the gossip topic ID and optional bootstrap peers. Serializes
/// to a compact string for sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTicket {
    /// Gossip topic that identifies the room.
    pub(crate) room_id: RoomId,
    /// Bootstrap peers to connect through.
    pub(crate) bootstrap: Vec<EndpointId>,
}

impl RoomTicket {
    /// Creates a ticket for an existing room with bootstrap peers.
    pub fn new(room_id: RoomId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
        Self {
            room_id,
            bootstrap: bootstrap.into_iter().collect(),
        }
    }

    /// Generates a ticket for a new room with a random topic.
    pub fn generate() -> Self {
        todo!()
    }
}

impl fmt::Display for RoomTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

impl FromStr for RoomTicket {
    type Err = iroh_tickets::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        todo!()
    }
}

/// Discrete events emitted by a [`Room`].
///
/// Delivered through [`Room::recv`]. Each variant represents a state
/// transition that the application should handle. Broadcasts are the unit
/// of publication — track-level changes within a broadcast are observable
/// via the broadcast's catalog watcher.
#[derive(Debug)]
pub enum RoomEvent {
    /// A new participant joined the room.
    ParticipantJoined(RemoteParticipant),
    /// A participant left the room.
    ParticipantLeft {
        /// Identity of the participant that left.
        participant: ParticipantId,
        /// Reason for departure.
        reason: DisconnectReason,
    },
    /// A remote participant published a new broadcast.
    ///
    /// The broadcast is immediately available for subscription. Check
    /// `broadcast.has_video()` / `broadcast.has_audio()` to decide what
    /// to subscribe to. For track-level updates within a broadcast (e.g.
    /// video added later), watch `broadcast.catalog()`.
    BroadcastPublished {
        /// Identity of the publishing participant.
        participant: ParticipantId,
        /// The broadcast kind (Camera, Screen, or Named).
        kind: BroadcastKind,
        /// The broadcast, ready for subscription.
        broadcast: RemoteBroadcast,
    },
    /// A remote participant removed a broadcast.
    BroadcastUnpublished {
        /// Identity of the publishing participant.
        participant: ParticipantId,
        /// The kind of the removed broadcast.
        kind: BroadcastKind,
    },
}

/// Active multi-party room session.
///
/// Provides the local participant handle for publishing, a stream of room
/// events, and access to remote participants. All methods take `&self`.
/// Cloning is cheap (Arc-based). Dropping the last clone leaves the room.
#[derive(Debug, Clone)]
pub struct Room {
    inner: Arc<RoomInner>,
}

#[derive(Debug)]
struct RoomInner {
    // room_id, local, participants, event_stream, ...
}

impl Room {
    /// Returns the room identifier.
    pub fn id(&self) -> RoomId {
        todo!()
    }

    /// Returns a shareable ticket for this room, including the local peer as bootstrap.
    pub fn ticket(&self) -> RoomTicket {
        todo!()
    }

    /// Returns the local participant handle for publishing media.
    pub fn local_participant(&self) -> &LocalParticipant {
        todo!()
    }

    /// Waits for the next room event.
    ///
    /// Returns `Ok(RoomEvent)` when an event occurs, or `Err` when the
    /// room has been closed. Events are buffered; slow consumers will
    /// eventually see backpressure.
    pub async fn recv(&self) -> Result<RoomEvent> {
        todo!()
    }

    /// Returns a snapshot of all currently connected remote participants.
    pub fn remote_participants(&self) -> HashMap<ParticipantId, RemoteParticipant> {
        todo!()
    }

    /// Returns a watcher over the remote participant list.
    ///
    /// Updates whenever participants join or leave. Useful for reactive UIs
    /// that re-render on participant changes without polling events.
    pub fn participants(&self) -> impl Watcher<Value = HashMap<ParticipantId, RemoteParticipant>> {
        todo!();
        n0_watcher::Watchable::new(HashMap::new()).watch()
    }

    /// Returns a specific remote participant by ID, if connected.
    pub fn remote_participant(&self, id: &ParticipantId) -> Option<RemoteParticipant> {
        todo!()
    }

    /// Leaves the room gracefully.
    pub fn leave(&self) {
        todo!()
    }
}

/// Local participant within a room.
///
/// Controls what media the local peer publishes into the room. Broadcasts
/// are created lazily on first access and keyed by [`BroadcastKind`].
#[derive(Debug, Clone)]
pub struct LocalParticipant {
    inner: Arc<LocalParticipantInner>,
}

#[derive(Debug)]
struct LocalParticipantInner {
    // id, broadcasts: RwLock<HashMap<BroadcastKind, LocalBroadcast>>, ...
}

impl LocalParticipant {
    /// Returns the local participant's identity.
    pub fn id(&self) -> ParticipantId {
        todo!()
    }

    /// Returns a broadcast for the given kind, creating it lazily.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use iroh_live::BroadcastKind::*;
    ///
    /// // Publish camera video and microphone audio:
    /// room.local_participant().broadcast(Camera).video().set_source(cam, codec, presets)?;
    /// room.local_participant().broadcast(Camera).audio().set_source(mic, codec, presets)?;
    ///
    /// // Publish screen-share:
    /// room.local_participant().broadcast(Screen).video().set_source(screen, codec, presets)?;
    /// ```
    pub fn broadcast(&self, kind: impl Into<BroadcastKind>) -> LocalBroadcast {
        todo!()
    }

    /// Returns all active local broadcasts.
    pub fn broadcasts(&self) -> HashMap<BroadcastKind, LocalBroadcast> {
        todo!()
    }
}

/// Remote participant in a room.
///
/// Provides access to the participant's identity and their broadcasts.
/// Cloning is cheap (Arc-based).
#[derive(Debug, Clone)]
pub struct RemoteParticipant {
    inner: Arc<RemoteParticipantInner>,
}

#[derive(Debug)]
struct RemoteParticipantInner {
    // id, broadcasts: RwLock<HashMap<BroadcastKind, RemoteBroadcast>>, ...
}

impl RemoteParticipant {
    /// Returns the participant's identity.
    pub fn id(&self) -> ParticipantId {
        todo!()
    }

    /// Returns all broadcasts from this participant.
    ///
    /// A participant typically publishes a `Camera` broadcast (with video+audio)
    /// and optionally a `Screen` broadcast. Custom workflows may publish
    /// additional named broadcasts.
    pub fn broadcasts(&self) -> HashMap<BroadcastKind, RemoteBroadcast> {
        todo!()
    }

    /// Returns a specific broadcast by kind.
    pub fn broadcast(&self, kind: impl Into<BroadcastKind>) -> Option<RemoteBroadcast> {
        todo!()
    }

    /// Waits until the participant publishes a broadcast of the given kind.
    ///
    /// Returns immediately if the broadcast already exists. Returns `Err`
    /// if the participant leaves before publishing.
    pub async fn wait_broadcast(
        &self,
        kind: impl Into<BroadcastKind>,
    ) -> Result<RemoteBroadcast> {
        todo!()
    }
}
```

## `broadcast.rs` -- Broadcast layer (Layer 2)

```rust
//! Broadcast-level API for direct media publishing and subscription.
//!
//! This module provides the mid-level broadcast API for use cases that don't
//! need room or call semantics: live streaming, studio links, camera
//! dashboards, recording pipelines, and transcoding relays.

use std::{fmt, str::FromStr, sync::Arc, time::Duration};

use hang::catalog::Catalog;
use n0_error::Result;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};

use crate::types::ParticipantId;

/// Portable handle for subscribing to a remote broadcast.
///
/// Contains the remote endpoint address and broadcast name. Serializes to
/// a compact string for sharing via QR codes or URLs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastTicket {
    /// Remote endpoint address.
    pub(crate) remote: iroh::EndpointAddr,
    /// Name of the broadcast to subscribe to.
    pub(crate) broadcast_name: String,
}

impl fmt::Display for BroadcastTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

impl FromStr for BroadcastTicket {
    type Err = iroh_tickets::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        todo!()
    }
}

/// Status of a broadcast connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastStatus {
    /// Waiting for the initial catalog from the remote.
    Connecting,
    /// Broadcast is active and media is available.
    Live,
    /// Broadcast has ended or the connection was lost.
    Ended,
}

/// Options for subscribing to a video track.
#[derive(Debug, Clone, Default)]
pub struct SubscribeVideoOptions {
    /// Preferred quality level. Defaults to highest available.
    pub quality: Option<moq_media::format::Quality>,
    /// Specific rendition name to subscribe to, bypassing quality selection.
    pub rendition: Option<String>,
    /// Initial viewport dimensions for decode scaling.
    pub viewport: Option<(u32, u32)>,
    /// Decode configuration overrides.
    pub decode_config: Option<moq_media::format::DecodeConfig>,
}

impl SubscribeVideoOptions {
    /// Sets the preferred quality level.
    pub fn quality(mut self, quality: moq_media::format::Quality) -> Self {
        self.quality = Some(quality);
        self
    }

    /// Sets a specific rendition name.
    pub fn rendition(mut self, name: impl Into<String>) -> Self {
        self.rendition = Some(name.into());
        self
    }

    /// Sets the initial viewport dimensions.
    pub fn viewport(mut self, width: u32, height: u32) -> Self {
        self.viewport = Some((width, height));
        self
    }

    /// Sets the decode configuration.
    pub fn decode_config(mut self, config: moq_media::format::DecodeConfig) -> Self {
        self.decode_config = Some(config);
        self
    }
}

/// Options for subscribing to an audio track.
#[derive(Debug, Clone, Default)]
pub struct SubscribeAudioOptions {
    /// Preferred quality level. Defaults to highest available.
    pub quality: Option<moq_media::format::Quality>,
    /// Specific rendition name to subscribe to, bypassing quality selection.
    pub rendition: Option<String>,
}

impl SubscribeAudioOptions {
    /// Sets the preferred quality level.
    pub fn quality(mut self, quality: moq_media::format::Quality) -> Self {
        self.quality = Some(quality);
        self
    }

    /// Sets a specific rendition name.
    pub fn rendition(mut self, name: impl Into<String>) -> Self {
        self.rendition = Some(name.into());
        self
    }
}

/// Local broadcast for publishing media to subscribers.
///
/// Wraps a [`moq_media::publish::PublishBroadcast`] with a higher-level API.
///
/// **Why this wrapper exists** (vs using `PublishBroadcast` directly):
/// - `PublishBroadcast::set_video/set_audio` take `&mut self`; this wrapper
///   provides `Arc + Mutex` so all methods take `&self` and handles are cloneable.
/// - Adds the slot abstraction (`LocalVideoSlot`/`LocalAudioSlot`) with
///   set/clear/enable/mute semantics that `PublishBroadcast` doesn't have.
/// - Exposes `producer()` for transport integration (`Live::publish_broadcast()`,
///   room actor, call code).
#[derive(Debug, Clone)]
pub struct LocalBroadcast {
    inner: Arc<LocalBroadcastInner>,
}

#[derive(Debug)]
struct LocalBroadcastInner {
    // Mutex needed: PublishBroadcast::set_video/set_audio take &mut self.
    // publish: Mutex<PublishBroadcast>,
}

impl LocalBroadcast {
    /// Creates a new empty broadcast with no media sources.
    pub fn new() -> Self {
        todo!()
    }

    /// Returns the local video slot for this broadcast.
    ///
    /// The returned handle is cheap to clone and provides video source
    /// configuration, enable/disable, and clear.
    pub fn video(&self) -> LocalVideoSlot {
        todo!()
    }

    /// Returns the local audio slot for this broadcast.
    ///
    /// The returned handle is cheap to clone and provides audio source
    /// configuration, mute/unmute, and clear.
    pub fn audio(&self) -> LocalAudioSlot {
        todo!()
    }

    /// Creates a local video preview from the current video source.
    ///
    /// Returns `None` if no video source is configured.
    pub fn preview(
        &self,
        decode_config: moq_media::format::DecodeConfig,
    ) -> Option<moq_media::subscribe::VideoTrack> {
        todo!()
    }

    /// Returns whether the broadcast currently has a video source configured.
    pub fn has_video(&self) -> bool {
        todo!()
    }

    /// Returns whether the broadcast currently has an audio source configured.
    pub fn has_audio(&self) -> bool {
        todo!()
    }

    /// Returns the raw [`moq_lite::BroadcastProducer`] for transport-level access.
    ///
    /// Used by [`Live::publish_broadcast`], room actors, and call code to
    /// register this broadcast on the network. `LocalBroadcast` itself is
    /// a pure media wrapper with no networking knowledge.
    pub fn producer(&self) -> moq_lite::BroadcastProducer {
        todo!()
    }
}

/// Video-side configuration and control for a [`LocalBroadcast`].
///
/// A single method — [`set_source`](Self::set_source) — handles all source
/// types (camera, screen, file, test pattern). The broadcast's
/// [`BroadcastKind`] carries the semantic meaning, not the slot.
///
/// Replacement semantics: calling `set_source` again swaps the source
/// without tearing down the logical video slot (equivalent to WebRTC's
/// `replaceTrack()`). Call `clear()` to remove video entirely.
#[derive(Debug, Clone)]
pub struct LocalVideoSlot {
    // inner: Arc<LocalBroadcastInner>,
}

impl LocalVideoSlot {
    /// Sets or replaces the video source with the given codec and presets.
    ///
    /// Creates renditions from the source, codec, and presets using
    /// [`moq_media::publish::VideoRenditions::new`]. If a video source is
    /// already configured, it is replaced without tearing down the logical
    /// slot. This is the broadcast-layer equivalent of WebRTC's
    /// `replaceTrack()`.
    ///
    /// For advanced codec control (specific encoder types, custom configs),
    /// use [`LocalBroadcast::producer`] and [`moq_media::publish::PublishBroadcast`]
    /// directly.
    pub fn set_source(
        &self,
        source: impl moq_media::traits::VideoSource,
        codec: moq_media::format::VideoCodec,
        presets: impl IntoIterator<Item = moq_media::format::VideoPreset>,
    ) -> Result<()> {
        todo!()
    }

    /// Removes video from the broadcast entirely.
    pub fn clear(&self) -> Result<()> {
        todo!()
    }

    /// Enables or disables the video slot without removing it.
    ///
    /// When disabled, no frames are encoded or sent, but the slot remains
    /// in the catalog. Re-enabling resumes without a full source setup.
    pub fn set_enabled(&self, enabled: bool) {
        todo!()
    }

    /// Returns whether the video slot is currently enabled.
    pub fn is_enabled(&self) -> bool {
        todo!()
    }
}

/// Audio-side configuration and control for a [`LocalBroadcast`].
///
/// A single method — [`set_source`](Self::set_source) — handles all audio
/// source types (microphone, system audio, file). Call `clear()` to remove
/// audio entirely.
#[derive(Debug, Clone)]
pub struct LocalAudioSlot {
    // inner: Arc<LocalBroadcastInner>,
}

impl LocalAudioSlot {
    /// Sets or replaces the audio source with the given codec and presets.
    ///
    /// Creates renditions from the source, codec, and presets. If an audio
    /// source is already configured, it is replaced without tearing down
    /// the logical slot.
    pub fn set_source(
        &self,
        source: impl moq_media::traits::AudioSource,
        codec: moq_media::format::AudioCodec,
        presets: impl IntoIterator<Item = moq_media::format::AudioPreset>,
    ) -> Result<()> {
        todo!()
    }

    /// Removes audio from the broadcast entirely.
    pub fn clear(&self) -> Result<()> {
        todo!()
    }

    /// Mutes or unmutes the audio slot without removing it.
    ///
    /// When muted, silence is sent (or encoding pauses, depending on codec
    /// DTX support). The slot remains in the catalog. Unmuting resumes
    /// without full source setup.
    pub fn set_muted(&self, muted: bool) {
        todo!()
    }

    /// Returns whether the audio slot is currently muted.
    pub fn is_muted(&self) -> bool {
        todo!()
    }
}

/// Subscribed remote broadcast for consuming media.
///
/// Wraps a [`moq_media::subscribe::SubscribeBroadcast`] with additional state.
///
/// **Why this wrapper exists** (vs using `SubscribeBroadcast` directly):
/// - Adds `BroadcastStatus` watchable (Connecting → Live → Ended lifecycle)
///   that `SubscribeBroadcast` doesn't track.
/// - Adds selection snapshots (`SelectedVideo`/`SelectedAudio`) remembering
///   the last successful subscription for UI display and debugging.
/// - Adds `has_video()`/`has_audio()` convenience derived from catalog.
///
/// All `SubscribeBroadcast` methods are available via [`as_inner()`](Self::as_inner).
#[derive(Debug, Clone)]
pub struct RemoteBroadcast {
    inner: Arc<RemoteBroadcastInner>,
}

#[derive(Debug)]
struct RemoteBroadcastInner {
    // No Mutex needed: SubscribeBroadcast methods all take &self.
    // subscribe: SubscribeBroadcast,
    // status: Watchable<BroadcastStatus>,
    // selected_video: Watchable<Option<SelectedVideo>>,
    // selected_audio: Watchable<Option<SelectedAudio>>,
}

impl RemoteBroadcast {
    /// Returns a watcher for the broadcast status.
    ///
    /// Not available on `SubscribeBroadcast` — this wrapper tracks the
    /// Connecting → Live → Ended lifecycle.
    pub fn status(&self) -> impl Watcher<Value = BroadcastStatus> {
        todo!();
        n0_watcher::Watchable::new(BroadcastStatus::Connecting).watch()
    }

    /// Returns a watcher for the current catalog.
    pub fn catalog(&self) -> impl Watcher<Value = Arc<Catalog>> {
        todo!();
        n0_watcher::Watchable::new(Arc::new(Catalog::default())).watch()
    }

    /// Returns a snapshot of the current catalog.
    pub fn catalog_snapshot(&self) -> Arc<Catalog> {
        todo!()
    }

    /// Returns the broadcast name.
    pub fn name(&self) -> &str {
        todo!()
    }

    /// Returns whether the broadcast currently advertises any video renditions.
    pub fn has_video(&self) -> bool {
        todo!()
    }

    /// Returns whether the broadcast currently advertises any audio renditions.
    pub fn has_audio(&self) -> bool {
        todo!()
    }

    // -- Subscription --

    /// Subscribes to the best available video track with default options.
    ///
    /// Selects the highest-quality rendition from the catalog and starts
    /// decoding. Returns a [`moq_media::subscribe::VideoTrack`] that yields
    /// decoded frames via `current_frame()`. For rendition control, use
    /// [`subscribe_video_with`](Self::subscribe_video_with).
    pub fn subscribe_video<D: moq_media::traits::VideoDecoder>(
        &self,
    ) -> Result<moq_media::subscribe::VideoTrack> {
        self.subscribe_video_with::<D>(Default::default())
    }

    /// Subscribes to a video track with custom options.
    ///
    /// Updates `selected_video()` on success — not available on
    /// `SubscribeBroadcast`.
    pub fn subscribe_video_with<D: moq_media::traits::VideoDecoder>(
        &self,
        options: SubscribeVideoOptions,
    ) -> Result<moq_media::subscribe::VideoTrack> {
        todo!()
    }

    /// Subscribes to the best available audio track with default options.
    pub async fn subscribe_audio<D: moq_media::traits::AudioDecoder>(
        &self,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
    ) -> Result<moq_media::subscribe::AudioTrack> {
        self.subscribe_audio_with::<D>(Default::default(), audio_backend).await
    }

    /// Subscribes to an audio track with custom options.
    ///
    /// Updates `selected_audio()` on success — not available on
    /// `SubscribeBroadcast`.
    pub async fn subscribe_audio_with<D: moq_media::traits::AudioDecoder>(
        &self,
        options: SubscribeAudioOptions,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
    ) -> Result<moq_media::subscribe::AudioTrack> {
        todo!()
    }

    // -- Catalog inspection --

    /// Lists available video rendition names, sorted by resolution (ascending).
    pub fn video_renditions(&self) -> Vec<String> {
        todo!()
    }

    /// Lists available audio rendition names.
    pub fn audio_renditions(&self) -> Vec<String> {
        todo!()
    }

    /// Returns the currently selected video rendition snapshot, if any.
    ///
    /// Updated after each successful `subscribe_video_with` call. Not
    /// available on `SubscribeBroadcast`.
    pub fn selected_video(&self) -> Option<SelectedVideo> {
        todo!()
    }

    /// Returns the currently selected audio rendition snapshot, if any.
    ///
    /// Updated after each successful `subscribe_audio_with` call. Not
    /// available on `SubscribeBroadcast`.
    pub fn selected_audio(&self) -> Option<SelectedAudio> {
        todo!()
    }

    /// Returns a future that resolves when the broadcast ends.
    pub fn closed(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        todo!();
        async {}
    }

    /// Returns the underlying [`moq_media::subscribe::SubscribeBroadcast`] for
    /// full transport-level access.
    pub fn as_inner(&self) -> &moq_media::subscribe::SubscribeBroadcast {
        todo!()
    }
}

/// Snapshot of the currently selected video rendition and options.
#[derive(Debug, Clone)]
pub struct SelectedVideo {
    pub rendition: String,
    pub options: SubscribeVideoOptions,
}

/// Snapshot of the currently selected audio rendition and options.
#[derive(Debug, Clone)]
pub struct SelectedAudio {
    pub rendition: String,
    pub options: SubscribeAudioOptions,
}
```

## `transport.rs` -- Raw transport re-exports (Layer 3)

```rust
//! Raw transport and protocol types for advanced usage.
//!
//! Re-exports [`iroh_moq`] session and protocol types for users who need
//! direct control over MoQ connections, custom publish/subscribe patterns,
//! or integration with non-standard media pipelines.

pub use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};

/// Re-exports from [`moq_lite`] for raw broadcast access.
pub mod raw {
    pub use moq_lite::{BroadcastConsumer, BroadcastProducer, Track, TrackProducer};
}
```
