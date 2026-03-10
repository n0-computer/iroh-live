# API Sketch

Rust code sketch of the three-layer `iroh-live` API redesign. All method bodies
use `todo!()`. Type signatures are real and intended to compile (modulo the
`todo!()` bodies). The goal is to nail down the public surface before
implementation.

See `0-overview.md` for context on the three layers and design principles.

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
pub struct ParticipantId(pub EndpointId);

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

/// Identifies a published track within a broadcast.
///
/// Combines a [`ParticipantId`] and a track name to form a globally unique
/// reference to a specific audio or video publication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicationId {
    /// Participant that owns the publication.
    pub participant: ParticipantId,
    /// Track name within the participant's broadcast.
    pub name: String,
}

impl fmt::Display for PublicationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.participant, self.name)
    }
}

/// Identifies a room by its gossip topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomId(pub iroh_gossip::TopicId);

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
//!   publications, and tickets for building end-user applications.
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
pub use self::types::{DisconnectReason, ParticipantId, PublicationId, RoomId};

// Participant and publication types from room module.
pub use self::room::{
    LocalParticipant, LocalTrackPublication, RemoteParticipant, RemoteTrackPublication,
};

// Re-export media crate for codec/pipeline access (layer 3).
pub use moq_media as media;
```

## `live.rs` -- Entry point

```rust
//! Entry point for creating calls, rooms, and broadcasts.

use std::sync::Arc;

use iroh::Endpoint;
use n0_error::Result;
use n0_future::Stream;

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
    /// Creates a new [`Live`] instance from an iroh [`Endpoint`].
    pub fn new(endpoint: Endpoint) -> Self {
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

    /// Returns a stream of incoming calls from remote peers.
    ///
    /// Each [`IncomingCall`] must be explicitly accepted or rejected. Dropping
    /// an [`IncomingCall`] without calling [`accept`](IncomingCall::accept)
    /// automatically rejects it.
    pub fn incoming_calls(&self) -> impl Stream<Item = IncomingCall> {
        todo!();
        // Needed for type inference; unreachable.
        futures_lite::stream::empty()
    }

    // -- Rooms --

    /// Joins or creates a multi-party room.
    pub async fn join_room(&self, ticket: impl Into<RoomTicket>) -> Result<Room> {
        todo!()
    }

    // -- Broadcasts (layer 2 convenience) --

    /// Creates a local broadcast that can be published to peers.
    pub fn create_broadcast(&self) -> LocalBroadcast {
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
```

## `call.rs` -- One-to-one calls

```rust
//! One-to-one call primitives with accept/reject semantics.

use std::{fmt, str::FromStr, sync::Arc};

use n0_error::Result;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};

use crate::broadcast::{LocalBroadcast, RemoteBroadcast};
use crate::types::{DisconnectReason, ParticipantId};

/// Portable handle for initiating a call to a specific peer.
///
/// Serializes to a compact string representation suitable for sharing
/// via QR codes, URLs, or clipboard. Contains the remote endpoint address
/// and optional bootstrap information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallTicket {
    /// Remote endpoint address.
    pub remote: iroh::EndpointAddr,
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
    fn from(remote: iroh::EndpointAddr) -> Self {
        Self { remote }
    }
}

/// State of a call connection.
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// Provides access to local and remote media through [`LocalBroadcast`] and
/// [`RemoteBroadcast`]. Dropping the [`Call`] closes the connection and
/// stops all media.
#[derive(Debug, Clone)]
pub struct Call {
    inner: Arc<CallInner>,
}

#[derive(Debug)]
struct CallInner {
    // session, state, local, remote, ...
}

impl Call {
    /// Returns the identity of the remote participant.
    pub fn remote_participant(&self) -> ParticipantId {
        todo!()
    }

    /// Returns the local broadcast for publishing media into the call.
    pub fn local(&self) -> &LocalBroadcast {
        todo!()
    }

    /// Returns the remote broadcast for subscribing to incoming media.
    pub fn remote(&self) -> &RemoteBroadcast {
        todo!()
    }

    /// Returns a watcher for the current call state.
    pub fn state(&self) -> impl Watcher<Value = CallState> {
        todo!();
        n0_watcher::Watchable::new(CallState::Connecting).watch()
    }

    /// Hangs up the call gracefully.
    pub fn hangup(&self) {
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
//! Multi-party room with participant and publication tracking.

use std::{collections::HashMap, fmt, str::FromStr, sync::Arc};

use iroh::EndpointId;
use n0_error::Result;
use n0_future::Stream;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};

use crate::broadcast::{LocalBroadcast, RemoteBroadcast, SubscribeVideoOptions, SubscribeAudioOptions, VideoTarget};
use crate::types::{DisconnectReason, ParticipantId, PublicationId, RoomId};

/// Portable handle for joining or creating a room.
///
/// Contains the gossip topic ID and optional bootstrap peers. Serializes
/// to a compact string for sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTicket {
    /// Gossip topic that identifies the room.
    pub room_id: RoomId,
    /// Bootstrap peers to connect through.
    pub bootstrap: Vec<EndpointId>,
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
/// Delivered through the stream returned by [`Room::events`]. Each variant
/// represents a state transition that the application should handle.
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
    /// A remote participant published a new track.
    TrackPublished {
        /// Identity of the publishing participant.
        participant: ParticipantId,
        /// The new publication.
        publication: RemoteTrackPublication,
    },
    /// A remote participant unpublished a track.
    TrackUnpublished {
        /// Identity of the publishing participant.
        participant: ParticipantId,
        /// Identifier of the removed publication.
        publication_id: PublicationId,
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

    /// Returns a stream of [`RoomEvent`]s.
    ///
    /// Events are buffered; slow consumers will eventually see backpressure.
    /// The stream ends when the room is closed.
    pub fn events(&self) -> impl Stream<Item = RoomEvent> {
        todo!();
        futures_lite::stream::empty()
    }

    /// Returns a snapshot of all currently connected remote participants.
    pub fn remote_participants(&self) -> HashMap<ParticipantId, RemoteParticipant> {
        todo!()
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
/// Controls what media the local peer publishes into the room. Publishing
/// is done by attaching a [`LocalBroadcast`] or by using the convenience
/// methods that create publications directly.
#[derive(Debug, Clone)]
pub struct LocalParticipant {
    inner: Arc<LocalParticipantInner>,
}

#[derive(Debug)]
struct LocalParticipantInner {
    // id, broadcast, publications, ...
}

impl LocalParticipant {
    /// Returns the local participant's identity.
    pub fn id(&self) -> ParticipantId {
        todo!()
    }

    /// Returns the underlying [`LocalBroadcast`] for advanced publishing control.
    pub fn broadcast(&self) -> &LocalBroadcast {
        todo!()
    }

    /// Publishes a video track and returns a handle to the publication.
    ///
    /// The `source` can be any type implementing [`moq_media::traits::VideoSource`].
    /// Multiple renditions are encoded automatically based on the provided presets.
    pub fn publish_video(
        &self,
        source: impl moq_media::traits::VideoSource,
        presets: impl IntoIterator<Item = moq_media::format::VideoPreset>,
    ) -> Result<LocalTrackPublication> {
        todo!()
    }

    /// Publishes an audio track and returns a handle to the publication.
    ///
    /// The `source` can be any type implementing [`moq_media::traits::AudioSource`].
    pub fn publish_audio(
        &self,
        source: impl moq_media::traits::AudioSource,
        presets: impl IntoIterator<Item = moq_media::format::AudioPreset>,
    ) -> Result<LocalTrackPublication> {
        todo!()
    }

    /// Returns all active local publications.
    pub fn publications(&self) -> Vec<LocalTrackPublication> {
        todo!()
    }
}

/// Handle to a locally published track.
///
/// Dropping the publication unpublishes the track from the room.
#[derive(Debug, Clone)]
pub struct LocalTrackPublication {
    inner: Arc<LocalTrackPublicationInner>,
}

#[derive(Debug)]
struct LocalTrackPublicationInner {
    // id, kind, ...
}

/// Kind of media carried by a publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// Video track.
    Video,
    /// Audio track.
    Audio,
}

impl LocalTrackPublication {
    /// Returns the publication identifier.
    pub fn id(&self) -> &PublicationId {
        todo!()
    }

    /// Returns the kind of media (video or audio).
    pub fn kind(&self) -> TrackKind {
        todo!()
    }

    /// Mutes the publication (stops sending media without unpublishing).
    pub fn set_muted(&self, muted: bool) {
        todo!()
    }

    /// Returns whether the publication is currently muted.
    pub fn is_muted(&self) -> bool {
        todo!()
    }

    /// Unpublishes and stops the track.
    pub fn unpublish(self) {
        todo!()
    }
}

/// Remote participant in a room.
///
/// Provides access to the participant's identity, publications, and their
/// underlying [`RemoteBroadcast`]. Cloning is cheap (Arc-based).
#[derive(Debug, Clone)]
pub struct RemoteParticipant {
    inner: Arc<RemoteParticipantInner>,
}

#[derive(Debug)]
struct RemoteParticipantInner {
    // id, broadcast, publications, ...
}

impl RemoteParticipant {
    /// Returns the participant's identity.
    pub fn id(&self) -> ParticipantId {
        todo!()
    }

    /// Returns the underlying [`RemoteBroadcast`] for advanced subscription control.
    pub fn broadcast(&self) -> &RemoteBroadcast {
        todo!()
    }

    /// Returns all publications from this participant.
    pub fn publications(&self) -> Vec<RemoteTrackPublication> {
        todo!()
    }

    /// Returns a specific publication by name, if available.
    pub fn publication(&self, name: &str) -> Option<RemoteTrackPublication> {
        todo!()
    }
}

/// Handle to a track published by a remote participant.
///
/// Provides both a simple subscription path (call [`subscribe_video`](Self::subscribe_video)
/// or [`subscribe_audio`](Self::subscribe_audio)) and an advanced path through
/// the underlying [`RemoteBroadcast`].
#[derive(Debug, Clone)]
pub struct RemoteTrackPublication {
    inner: Arc<RemoteTrackPublicationInner>,
}

#[derive(Debug)]
struct RemoteTrackPublicationInner {
    // id, kind, participant, broadcast, ...
}

impl RemoteTrackPublication {
    /// Returns the publication identifier.
    pub fn id(&self) -> &PublicationId {
        todo!()
    }

    /// Returns the kind of media (video or audio).
    pub fn kind(&self) -> TrackKind {
        todo!()
    }

    /// Returns the identity of the publishing participant.
    pub fn participant(&self) -> ParticipantId {
        todo!()
    }

    // -- Simple subscription path --

    /// Subscribes to this video publication with default options.
    ///
    /// Uses [`moq_media::codec::DynamicVideoDecoder`] for automatic codec selection.
    /// Returns a [`moq_media::subscribe::WatchTrack`] that yields decoded frames.
    /// For explicit decoder selection, use [`RemoteBroadcast::subscribe_video`]
    /// on the underlying broadcast.
    pub fn subscribe_video(&self) -> Result<moq_media::subscribe::WatchTrack> {
        self.subscribe_video_with(Default::default())
    }

    /// Subscribes to this video publication with custom options.
    ///
    /// Uses [`moq_media::codec::DynamicVideoDecoder`] internally. For explicit
    /// decoder type selection, use the broadcast-layer API via [`RemoteParticipant::broadcast`].
    pub fn subscribe_video_with(
        &self,
        options: SubscribeVideoOptions,
    ) -> Result<moq_media::subscribe::WatchTrack> {
        todo!()
    }

    /// Subscribes to this audio publication with default options.
    ///
    /// Returns an [`moq_media::subscribe::AudioTrack`] that plays through the
    /// provided audio backend.
    pub async fn subscribe_audio(
        &self,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
    ) -> Result<moq_media::subscribe::AudioTrack> {
        self.subscribe_audio_with(Default::default(), audio_backend).await
    }

    /// Subscribes to this audio publication with custom options.
    pub async fn subscribe_audio_with(
        &self,
        options: SubscribeAudioOptions,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
    ) -> Result<moq_media::subscribe::AudioTrack> {
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
    pub remote: iroh::EndpointAddr,
    /// Name of the broadcast to subscribe to.
    pub broadcast_name: String,
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

/// Where to deliver decoded video frames.
///
/// Used in [`SubscribeVideoOptions`] to control the output destination.
#[derive(Debug, Clone)]
pub enum VideoTarget {
    /// Delivers frames through a [`moq_media::subscribe::WatchTrack`] receiver.
    Frames,
    // Future: Texture(wgpu::Device), DirectDisplay, etc.
}

impl Default for VideoTarget {
    fn default() -> Self {
        Self::Frames
    }
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
    /// Where to deliver decoded frames.
    pub target: VideoTarget,
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
/// Supports setting video and audio sources, managing renditions, and
/// obtaining a local preview. Cloning is cheap (Arc-based).
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

    /// Sets the video source and rendition presets for encoding.
    ///
    /// Replaces any previously configured video. Pass `None` to remove video.
    pub fn set_video(
        &self,
        renditions: Option<moq_media::publish::VideoRenditions>,
    ) -> Result<()> {
        todo!()
    }

    /// Sets the audio source and rendition presets for encoding.
    ///
    /// Replaces any previously configured audio. Pass `None` to remove audio.
    pub fn set_audio(
        &self,
        renditions: Option<moq_media::publish::AudioRenditions>,
    ) -> Result<()> {
        todo!()
    }

    /// Creates a local video preview from the current video source.
    ///
    /// Returns `None` if no video source is configured.
    pub fn watch_local(
        &self,
        decode_config: moq_media::format::DecodeConfig,
    ) -> Option<moq_media::subscribe::WatchTrack> {
        todo!()
    }

    /// Returns the raw [`moq_lite::BroadcastProducer`] for transport-level access.
    pub fn producer(&self) -> moq_lite::BroadcastProducer {
        todo!()
    }
}

/// Subscribed remote broadcast for consuming media.
///
/// Wraps a [`moq_media::subscribe::SubscribeBroadcast`] with catalog access,
/// rendition selection, and simple subscription methods. Cloning is cheap
/// (Arc-based).
#[derive(Debug, Clone)]
pub struct RemoteBroadcast {
    inner: Arc<RemoteBroadcastInner>,
}

#[derive(Debug)]
struct RemoteBroadcastInner {
    // No Mutex needed: SubscribeBroadcast methods all take &self.
    // subscribe: SubscribeBroadcast,
    // status: Watchable<BroadcastStatus>,
}

impl RemoteBroadcast {
    /// Returns a watcher for the broadcast status.
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

    // -- Simple subscription path --

    /// Subscribes to the best available video track with default options.
    ///
    /// Selects the highest-quality rendition from the catalog and starts
    /// decoding. Returns a [`moq_media::subscribe::WatchTrack`] that yields
    /// decoded frames. For rendition control, use [`subscribe_video_with`](Self::subscribe_video_with).
    pub fn subscribe_video<D: moq_media::traits::VideoDecoder>(
        &self,
    ) -> Result<moq_media::subscribe::WatchTrack> {
        self.subscribe_video_with::<D>(Default::default())
    }

    /// Subscribes to a video track with custom options.
    pub fn subscribe_video_with<D: moq_media::traits::VideoDecoder>(
        &self,
        options: SubscribeVideoOptions,
    ) -> Result<moq_media::subscribe::WatchTrack> {
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
    pub async fn subscribe_audio_with<D: moq_media::traits::AudioDecoder>(
        &self,
        options: SubscribeAudioOptions,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
    ) -> Result<moq_media::subscribe::AudioTrack> {
        todo!()
    }

    // -- Advanced subscription path --

    /// Subscribes to a specific video rendition by track name.
    ///
    /// Bypasses quality-based selection for full control over which rendition
    /// is decoded. Useful for picture-in-picture, thumbnails, or adaptive
    /// bitrate logic.
    pub fn subscribe_video_rendition<D: moq_media::traits::VideoDecoder>(
        &self,
        track_name: &str,
        decode_config: &moq_media::format::DecodeConfig,
    ) -> Result<moq_media::subscribe::WatchTrack> {
        todo!()
    }

    /// Subscribes to a specific audio rendition by track name.
    pub async fn subscribe_audio_rendition<D: moq_media::traits::AudioDecoder>(
        &self,
        track_name: &str,
        audio_backend: &dyn moq_media::traits::AudioStreamFactory,
    ) -> Result<moq_media::subscribe::AudioTrack> {
        todo!()
    }

    /// Lists available video rendition names, sorted by resolution (ascending).
    pub fn video_renditions(&self) -> Vec<String> {
        todo!()
    }

    /// Lists available audio rendition names.
    pub fn audio_renditions(&self) -> Vec<String> {
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
