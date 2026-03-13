# API Sketch

Rust code sketch of the redesigned API. Method bodies use `todo!()`. The goal
is to nail down the public surface before implementation.

Core principle: moq-media owns all broadcast/media types. iroh-live re-exports
and adds only room/call/participant semantics.

---

## Part 1: moq-media

### Error types

Domain-specific errors instead of blanket `anyhow::Result` on the public API.
Internal code can still use `anyhow` and convert at the boundary.

```rust
// moq-media/src/error.rs

/// Errors from publishing operations.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("no video source configured")]
    NoVideoSource,
    #[error("no audio source configured")]
    NoAudioSource,
    #[error("codec not available: {0}")]
    CodecUnavailable(String),
    #[error("encoder failed: {0}")]
    EncoderFailed(#[source] anyhow::Error),
}

/// Errors from subscription operations.
#[derive(Debug, thiserror::Error)]
pub enum SubscribeError {
    #[error("broadcast not found")]
    NotFound,
    #[error("no catalog available")]
    NoCatalog,
    #[error("rendition not found: {0}")]
    RenditionNotFound(String),
    #[error("decoder failed: {0}")]
    DecoderFailed(#[source] anyhow::Error),
    #[error("broadcast ended")]
    Ended,
}
```

### `Broadcast` — Publishing media

Replaces `PublishBroadcast`. All methods `&self` (interior mutability).
Clone is cheap (Arc-based). Drop stops encoding and removes from transport.

```rust
// moq-media/src/publish.rs

/// A named collection of media tracks published to subscribers.
///
/// Create a broadcast, configure video and audio sources, then pass
/// [`producer()`](Self::producer) to the transport layer for publishing.
///
/// ```no_run
/// let broadcast = Broadcast::new();
/// broadcast.video().set(camera, VideoCodec::H264, [P720, P360])?;
/// broadcast.audio().set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;
/// session.publish("my-stream", broadcast.producer());
/// ```
#[derive(Debug, Clone)]
pub struct Broadcast {
    inner: Arc<BroadcastInner>,
}

struct BroadcastInner {
    video: Mutex<Option<VideoRenditions>>,
    audio: Mutex<Option<AudioRenditions>>,
    catalog: CatalogProducer,
    producer: BroadcastProducer,
    shutdown: CancellationToken,
}

impl Broadcast {
    /// Creates a new empty broadcast with no media sources.
    pub fn new() -> Self { todo!() }

    /// Creates a relay broadcast that forwards packets from a subscription
    /// without decoding or re-encoding.
    ///
    /// Copies the catalog from the subscription and forwards all track
    /// data verbatim. Watches for catalog changes and adapts (new tracks
    /// spawn new forwarding tasks). The relay broadcast can be published
    /// on a different session to act as a media relay/CDN node.
    ///
    /// ```no_run
    /// let sub = Subscription::new("stream", consumer).await?;
    /// let relay = Broadcast::relay(&sub)?;
    /// session.publish("relayed-stream", relay.producer());
    /// ```
    pub fn relay(subscription: &Subscription) -> Result<Self, PublishError> { todo!() }

    /// Returns the video publisher for configuring video sources.
    pub fn video(&self) -> VideoPublisher { todo!() }

    /// Returns the audio publisher for configuring audio sources.
    pub fn audio(&self) -> AudioPublisher { todo!() }

    /// Returns whether video is currently being published.
    pub fn has_video(&self) -> bool { todo!() }

    /// Returns whether audio is currently being published.
    pub fn has_audio(&self) -> bool { todo!() }

    /// Creates a local video preview from the current video source.
    ///
    /// Returns pre-encode frames (native resolution, no compression artifacts).
    /// Same `VideoTrack` interface as remote video — UI code doesn't need to
    /// distinguish local from remote.
    ///
    /// `PlaybackConfig` is an existing type in `rusty_codecs::config` that
    /// controls decoder backend selection and pixel format preferences.
    pub fn preview(&self, config: PlaybackConfig) -> Option<VideoTrack> {
        todo!()
    }

    /// Returns the [`BroadcastProducer`] for transport-level publishing.
    pub fn producer(&self) -> BroadcastProducer { todo!() }
}
```

### `VideoPublisher` / `AudioPublisher` — Media slot sub-handles

Lightweight handles into the broadcast's inner state. No separate allocation.
These replace the `set_video(Some(VideoRenditions))` / `set_video(None)` pattern.

```rust
// moq-media/src/publish.rs

/// Controls video publishing on a [`Broadcast`].
///
/// Supports source replacement (broadcast-layer `replaceTrack()`) without
/// tearing down the logical slot, and enable/disable without removing
/// the slot from the catalog.
#[derive(Debug, Clone)]
pub struct VideoPublisher {
    inner: Arc<BroadcastInner>,
}

impl VideoPublisher {
    /// Sets the video source with a specific codec and resolution presets.
    ///
    /// Replaces any existing video source. Starts encoder pipelines for
    /// each preset. The catalog is updated to advertise the new renditions.
    #[cfg(any_video_codec)]
    pub fn set(
        &self,
        source: impl VideoSource,
        codec: VideoCodec,
        presets: impl IntoIterator<Item = VideoPreset>,
    ) -> Result<(), PublishError> { todo!() }

    /// Sets the video source using a generic encoder factory.
    pub fn set_with<E: VideoEncoderFactory>(
        &self,
        source: impl VideoSource,
        presets: impl IntoIterator<Item = VideoPreset>,
    ) -> Result<(), PublishError> { todo!() }

    /// Replaces the video source while keeping the same codec and presets.
    ///
    /// Encoder pipelines restart with the new source. Subscribers see a
    /// seamless transition (new keyframe, same rendition names).
    pub fn replace(&self, source: impl VideoSource) -> Result<(), PublishError> { todo!() }

    /// Removes video from the broadcast entirely.
    pub fn clear(&self) { todo!() }

    /// Pauses or resumes video encoding without removing the slot.
    ///
    /// When disabled, the catalog still advertises renditions but the
    /// encoder stops producing frames. Subscribers see a frozen frame
    /// or gap depending on their player.
    pub fn set_enabled(&self, enabled: bool) { todo!() }

    /// Returns whether video encoding is currently enabled.
    pub fn is_enabled(&self) -> bool { todo!() }

    /// Returns the currently configured rendition names.
    pub fn renditions(&self) -> Vec<String> { todo!() }
}

/// Controls audio publishing on a [`Broadcast`].
#[derive(Debug, Clone)]
pub struct AudioPublisher {
    inner: Arc<BroadcastInner>,
}

impl AudioPublisher {
    /// Sets the audio source with a specific codec and quality presets.
    #[cfg(any_audio_codec)]
    pub fn set(
        &self,
        source: impl AudioSource,
        codec: AudioCodec,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Result<(), PublishError> { todo!() }

    /// Sets the audio source using a generic encoder factory.
    pub fn set_with<E: AudioEncoderFactory>(
        &self,
        source: impl AudioSource,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Result<(), PublishError> { todo!() }

    /// Replaces the audio source while keeping the same codec and presets.
    pub fn replace(&self, source: impl AudioSource) -> Result<(), PublishError> { todo!() }

    /// Removes audio from the broadcast entirely.
    pub fn clear(&self) { todo!() }

    /// Mutes or unmutes audio without removing the slot.
    pub fn set_muted(&self, muted: bool) { todo!() }

    /// Returns whether audio is currently muted.
    pub fn is_muted(&self) -> bool { todo!() }

    /// Returns the currently configured rendition names.
    pub fn renditions(&self) -> Vec<String> { todo!() }
}
```

### `VideoTarget` — Declarative quality selection

Inspired by Hang JS's `video.target = { width, height }`. Express intent,
the system picks the best rendition. `Quality` is a convenience that converts
into a `VideoTarget`.

```rust
// moq-media/src/subscribe.rs

/// Declarative target for video quality selection.
///
/// The subscription system picks the best available rendition matching the
/// target constraints. If multiple renditions match, the highest quality
/// within the constraints is chosen.
///
/// ```no_run
/// // By pixel budget (great for dashboards with many tiles)
/// let target = VideoTarget::default().max_pixels(640 * 480);
///
/// // By bitrate budget (great for bandwidth-limited links)
/// let target = VideoTarget::default().max_bitrate_kbps(500);
///
/// // Combine constraints
/// let target = VideoTarget::default()
///     .max_pixels(1280 * 720)
///     .max_bitrate_kbps(2000);
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct VideoTarget {
    /// Maximum pixel count (width × height). `None` = no limit.
    pub max_pixels: Option<u32>,
    /// Maximum bitrate in kbps. `None` = no limit.
    pub max_bitrate_kbps: Option<u32>,
    /// Explicit rendition name, bypassing automatic selection.
    pub rendition: Option<String>,
}

impl VideoTarget {
    pub fn max_pixels(mut self, pixels: u32) -> Self {
        self.max_pixels = Some(pixels); self
    }
    pub fn max_bitrate_kbps(mut self, kbps: u32) -> Self {
        self.max_bitrate_kbps = Some(kbps); self
    }
    pub fn rendition(mut self, name: impl Into<String>) -> Self {
        self.rendition = Some(name.into()); self
    }
}

/// Common quality presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Lowest available rendition.
    Low,
    /// Middle rendition (or lowest if only two).
    Medium,
    /// Highest available rendition.
    High,
}

impl From<Quality> for VideoTarget {
    fn from(q: Quality) -> Self {
        match q {
            Quality::Low => VideoTarget::default().max_pixels(320 * 240),
            Quality::Medium => VideoTarget::default().max_pixels(640 * 480),
            Quality::High => VideoTarget::default(), // no limit
        }
    }
}
```

### `Subscription` — Subscribing to remote media

Replaces `SubscribeBroadcast`. All methods `&self`.

```rust
// moq-media/src/subscribe.rs

/// Status of a broadcast subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastStatus {
    /// Waiting for the initial catalog.
    Connecting,
    /// Broadcast is active, media available.
    Live,
    /// Broadcast ended or connection lost.
    Ended,
}

/// Options for subscribing to video.
///
/// `PlaybackConfig` is an existing type in `rusty_codecs::config` controlling
/// decoder backend selection (VAAPI, software, etc.) and pixel format preferences.
/// It already exists and is well-tested — we reuse it here rather than defining
/// a new type.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct VideoOptions {
    /// Playback configuration (decoder backend, pixel format).
    /// Defaults to automatic decoder selection via `DynamicVideoDecoder`.
    pub playback: Option<PlaybackConfig>,
    /// Quality target for automatic rendition selection.
    pub target: Option<VideoTarget>,
    /// Initial viewport dimensions for decode scaling.
    pub viewport: Option<(u32, u32)>,
}

impl VideoOptions {
    pub fn playback(mut self, config: PlaybackConfig) -> Self {
        self.playback = Some(config); self
    }
    pub fn target(mut self, target: impl Into<VideoTarget>) -> Self {
        self.target = Some(target.into()); self
    }
    pub fn quality(mut self, quality: Quality) -> Self {
        self.target = Some(quality.into()); self
    }
    pub fn viewport(mut self, width: u32, height: u32) -> Self {
        self.viewport = Some((width, height)); self
    }
}

/// Options for subscribing to audio.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AudioOptions {
    /// Quality preference. Defaults to highest.
    pub quality: Option<Quality>,
    /// Specific rendition name, bypassing quality selection.
    pub rendition: Option<String>,
}

impl AudioOptions {
    pub fn quality(mut self, quality: Quality) -> Self {
        self.quality = Some(quality); self
    }
    pub fn rendition(mut self, name: impl Into<String>) -> Self {
        self.rendition = Some(name.into()); self
    }
}

/// A subscription to a remote broadcast.
///
/// Provides catalog inspection and media subscription. Created by
/// connecting to a named broadcast on a transport session.
///
/// ```no_run
/// let sub = Subscription::new("their-stream", consumer).await?;
/// let video = sub.video()?;
/// let audio = sub.audio(audio_backend).await?;
/// ```
#[derive(Debug, Clone)]
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

struct SubscriptionInner {
    // name: String,
    // consumer: BroadcastConsumer,
    // catalog: Watchable<CatalogSnapshot>,
    // status: Watchable<BroadcastStatus>,
    // shutdown: CancellationToken,
}

impl Subscription {
    /// Connects to a named broadcast and fetches the initial catalog.
    pub async fn new(
        name: impl Into<String>,
        broadcast: BroadcastConsumer,
    ) -> Result<Self, SubscribeError> { todo!() }

    /// Returns the broadcast name.
    pub fn name(&self) -> &str { todo!() }

    // -- Status (dual-accessor: snapshot + watcher) --

    /// Returns the current broadcast status.
    pub fn status(&self) -> BroadcastStatus { todo!() }

    /// Returns a watcher for broadcast status changes.
    pub fn status_watcher(&self) -> impl Watcher<Value = BroadcastStatus> { todo!() }

    // -- Catalog (dual-accessor: snapshot + watcher) --

    /// Returns the current catalog snapshot.
    pub fn catalog(&self) -> CatalogSnapshot { todo!() }

    /// Returns a watcher for catalog changes.
    pub fn catalog_watcher(&self) -> impl Watcher<Value = CatalogSnapshot> { todo!() }

    /// Returns whether the catalog advertises any video renditions.
    pub fn has_video(&self) -> bool { todo!() }

    /// Returns whether the catalog advertises any audio renditions.
    pub fn has_audio(&self) -> bool { todo!() }

    /// Lists available video rendition names, sorted by resolution ascending.
    pub fn video_renditions(&self) -> Vec<String> { todo!() }

    /// Lists available audio rendition names.
    pub fn audio_renditions(&self) -> Vec<String> { todo!() }

    // -- Subscribe to media --

    /// Subscribes to the best available video with default options.
    ///
    /// Uses [`DynamicVideoDecoder`] for automatic codec selection.
    /// Picks the highest-quality rendition from the catalog.
    pub fn video(&self) -> Result<VideoTrack, SubscribeError> {
        self.video_with(Default::default())
    }

    /// Subscribes to video with custom options.
    pub fn video_with(&self, options: VideoOptions) -> Result<VideoTrack, SubscribeError> {
        todo!()
    }

    /// Subscribes to the best available audio with default options.
    pub async fn audio(
        &self,
        backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack, SubscribeError> {
        self.audio_with(Default::default(), backend).await
    }

    /// Subscribes to audio with custom options.
    pub async fn audio_with(
        &self,
        options: AudioOptions,
        backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack, SubscribeError> { todo!() }

    // -- Advanced: explicit decoder type --

    /// Subscribes to a video rendition with an explicit decoder type.
    ///
    /// For most uses, prefer [`video()`](Self::video) which uses automatic
    /// decoder selection.
    pub fn video_rendition<D: VideoDecoder>(
        &self,
        name: &str,
        config: PlaybackConfig,
    ) -> Result<VideoTrack, SubscribeError> { todo!() }

    /// Subscribes to an audio rendition with an explicit decoder type.
    pub async fn audio_rendition<D: AudioDecoder>(
        &self,
        name: &str,
        backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack, SubscribeError> { todo!() }

    /// Subscribes to both video and audio with defaults.
    ///
    /// Convenience for the common case of wanting all media.
    pub async fn media(
        &self,
        backend: &dyn AudioStreamFactory,
    ) -> Result<MediaTracks, SubscribeError> { todo!() }

    /// Returns a future that resolves when the broadcast ends.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        todo!()
    }

    /// Shuts down the subscription.
    pub fn shutdown(&self) { todo!() }
}

/// Video and audio tracks from a single subscription.
#[derive(Debug)]
pub struct MediaTracks {
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
}
```

### `VideoTrack` — Decoded video from a subscription

Replaces `WatchTrack`. Also implements `VideoSource` to enable transcode relay.
Provides three access patterns: snapshot (`current_frame`), async pull
(`next_frame`), and composable stream (`frames`).

```rust
// moq-media/src/subscribe.rs

/// Decoded video frames from a subscription or local preview.
///
/// Implements [`VideoSource`] so it can be fed directly into a
/// [`Broadcast`] for transcoding relay.
///
/// Three access patterns:
/// - `current_frame()` — latest frame, non-blocking (render loops)
/// - `next_frame()` — next frame in order, async (recording pipelines)
/// - `frames()` — `impl Stream` for composition with StreamExt
///
/// ```no_run
/// // Transcode relay: decode at one quality, re-encode at another
/// let sub = Subscription::new("source", consumer).await?;
/// let video = sub.video()?;
///
/// let relay = Broadcast::new();
/// relay.video().set_with::<H264Encoder>(video, [P360])?;
/// ```
#[derive(Debug)]
pub struct VideoTrack {
    rx: mpsc::Receiver<VideoFrame>,
    inner: VideoTrackInner,
}

enum VideoTrackInner {
    Pipeline(VideoDecoderHandle),
    Source { /* capture source, scaler thread */ },
    Empty { rendition: String },
}

impl VideoTrack {
    /// Returns the rendition name.
    pub fn rendition(&self) -> &str { todo!() }

    /// Returns the decoder name (or "capture" for raw sources).
    pub fn decoder_name(&self) -> &str { todo!() }

    /// Updates the target viewport dimensions for the decoder/scaler.
    pub fn set_viewport(&self, width: u32, height: u32) { todo!() }

    /// Returns the latest frame, draining any buffered older frames.
    ///
    /// Non-blocking. Returns `None` if no frame has been decoded yet.
    /// Best for render loops that always want the freshest frame.
    pub fn current_frame(&mut self) -> Option<VideoFrame> { todo!() }

    /// Waits for the next decoded frame in order.
    ///
    /// Returns `None` when the track ends. Best for recording pipelines
    /// that need every frame.
    pub async fn next_frame(&mut self) -> Option<VideoFrame> { todo!() }

    /// Returns a stream of decoded frames for composition with StreamExt.
    ///
    /// ```no_run
    /// use futures::StreamExt;
    /// let mut frames = video.frames();
    /// while let Some(frame) = frames.next().await {
    ///     process(frame);
    /// }
    /// ```
    pub fn frames(&mut self) -> impl Stream<Item = VideoFrame> + '_ { todo!() }
}

/// `VideoTrack` can be used as a `VideoSource` for transcode relay.
impl VideoSource for VideoTrack {
    fn name(&self) -> &str { self.rendition() }

    fn format(&self) -> VideoFormat { todo!() }

    fn start(&mut self) -> Result<()> { Ok(()) } // already running

    fn stop(&mut self) -> Result<()> { Ok(()) } // handled by Drop

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.current_frame())
    }
}
```

### `AudioTrack` — Decoded audio from a subscription

Unchanged except minor rename of internal constructor.

```rust
// moq-media/src/subscribe.rs

/// Audio playback from a subscription.
///
/// Audio is decoded and routed to an audio sink (speaker) automatically.
/// Use [`handle()`](Self::handle) for playback control (pause, resume,
/// peak metering).
#[derive(Debug)]
pub struct AudioTrack {
    pipeline: AudioDecoderPipeline,
    rendition: String,
}

impl AudioTrack {
    /// Returns the rendition name.
    pub fn rendition(&self) -> &str { &self.rendition }

    /// Returns the audio control handle (pause, resume, peak metering).
    pub fn handle(&self) -> &dyn AudioSinkHandle { todo!() }

    /// Returns a future that resolves when playback stops.
    pub fn stopped(&self) -> impl Future<Output = ()> + '_ { todo!() }
}
```

### `CatalogSnapshot` — Catalog with rendition helpers

Replaces `CatalogWrapper`. Better name, same functionality plus
`VideoTarget`-based selection.

```rust
// moq-media/src/subscribe.rs

/// Snapshot of a broadcast's catalog with rendition selection helpers.
///
/// Wraps `hang::catalog::Catalog` with convenience methods for selecting
/// renditions by quality, target, or name.
#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    inner: Arc<Catalog>,
    sequence: u64,
}

impl CatalogSnapshot {
    /// Returns video rendition names sorted by width ascending.
    pub fn video_renditions(&self) -> impl Iterator<Item = &str> { todo!() }

    /// Returns audio rendition names.
    pub fn audio_renditions(&self) -> impl Iterator<Item = &str> { todo!() }

    /// Selects a video rendition matching the quality preference.
    pub fn select_video(&self, quality: Quality) -> Result<String, SubscribeError> { todo!() }

    /// Selects a video rendition matching the target constraints.
    ///
    /// Returns the highest-quality rendition within the target's
    /// `max_pixels` and `max_bitrate_kbps` limits.
    pub fn select_video_target(&self, target: &VideoTarget) -> Result<String, SubscribeError> {
        todo!()
    }

    /// Selects an audio rendition matching the quality preference.
    pub fn select_audio(&self, quality: Quality) -> Result<String, SubscribeError> { todo!() }

    /// Returns the underlying [`Catalog`].
    pub fn as_catalog(&self) -> &Catalog { &self.inner }
}

impl Deref for CatalogSnapshot {
    type Target = Catalog;
    fn deref(&self) -> &Catalog { &self.inner }
}
```

### moq-media re-exports (`lib.rs`)

```rust
// moq-media/src/lib.rs

pub mod publish;
pub mod subscribe;
pub mod error;
pub mod pipeline;
pub mod transport;
pub mod audio_backend;
pub mod capture;

// Primary types at crate root for ergonomic imports.
pub use publish::Broadcast;
pub use subscribe::{AudioTrack, Subscription, VideoTrack, VideoTarget, Quality};
pub use error::{PublishError, SubscribeError};

// Re-exports from rusty-codecs.
pub use rusty_codecs::{codec, config, format, traits};
pub use rusty_codecs::render; // #[cfg(feature = "wgpu")]
pub use audio_backend::{AudioBackend, AudioBackendOpts, AudioDevice};
```

---

## Part 2: iroh-live

### `lib.rs` — Crate root

```rust
//! Real-time media sessions over iroh.
//!
//! For streaming, relays, and pipelines without room/call semantics,
//! use [`moq_media`] directly.

mod call;
mod live;
mod room;
mod types;

pub use self::call::{Call, CallError, CallState, CallTicket, IncomingCall};
pub use self::live::Live;
pub use self::room::{
    LocalParticipant, RemoteParticipant,
    Room, RoomError, RoomEvent, RoomTicket, TrackKind,
};
pub use self::types::{DisconnectReason, ParticipantId, RoomId};

// Re-export the media crate for broadcast/media access.
pub use moq_media as media;

// Convenience re-exports of the most-used moq-media types.
pub use moq_media::{Broadcast, Subscription, VideoTrack, AudioTrack, VideoTarget, Quality};

// Transport escape hatch.
pub mod transport {
    pub use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};
}

/// Ergonomic imports for common use.
///
/// ```
/// use iroh_live::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{Live, Room, RoomEvent, Call, IncomingCall};
    pub use crate::room::{LocalParticipant, RemoteParticipant};
    pub use crate::types::ParticipantId;
    pub use crate::{CallTicket, RoomTicket};
    pub use crate::{Broadcast, Subscription, VideoTrack, AudioTrack, VideoTarget, Quality};
    pub use moq_media::AudioBackend;
}
```

### `Live` — Entry point

Builder-based construction. All methods `&self`.

```rust
// iroh-live/src/live.rs

/// Builder for [`Live`].
///
/// ```no_run
/// let live = Live::builder()
///     .secret_key(key)
///     .relay_url(url)
///     .spawn()
///     .await?;
/// ```
pub struct LiveBuilder { /* ... */ }

impl LiveBuilder {
    pub fn secret_key(mut self, key: SecretKey) -> Self { todo!() }
    pub fn relay_url(mut self, url: Url) -> Self { todo!() }
    pub fn gossip(mut self, enabled: bool) -> Self { todo!() }
    pub async fn spawn(self) -> Result<Live> { todo!() }
}

/// Entry point for iroh-live operations.
///
/// Manages the iroh endpoint, MoQ transport, and gossip. All methods
/// take `&self`. Cloning is cheap.
///
/// For broadcast-only use cases, use [`moq_media::Broadcast`] and
/// [`moq_media::Subscription`] directly without this type.
#[derive(Debug, Clone)]
pub struct Live {
    inner: Arc<LiveInner>,
}

impl Live {
    /// Returns a builder for constructing a `Live` instance.
    pub fn builder() -> LiveBuilder { todo!() }

    /// Creates a new instance from existing components.
    pub fn new(endpoint: Endpoint, gossip: Gossip) -> Self { todo!() }

    /// Returns the underlying iroh endpoint.
    pub fn endpoint(&self) -> &Endpoint { todo!() }

    /// Returns the MoQ transport handle for advanced operations.
    pub fn transport(&self) -> &iroh_moq::Moq { todo!() }

    // -- Calls --

    /// Initiates a one-to-one call.
    ///
    /// Accepts `CallTicket` or `EndpointAddr` via `Into<CallTicket>`.
    pub async fn call(&self, ticket: impl Into<CallTicket>) -> Result<Call, CallError> {
        todo!()
    }

    /// Returns a stream of incoming calls.
    ///
    /// Each [`IncomingCall`] must be accepted or rejected. Dropping an
    /// `IncomingCall` without accepting auto-rejects it.
    ///
    /// ```no_run
    /// use futures::StreamExt;
    /// let mut incoming = live.incoming_calls();
    /// while let Some(call) = incoming.next().await {
    ///     let call = call.accept().await?;
    /// }
    /// ```
    pub fn incoming_calls(&self) -> impl Stream<Item = IncomingCall> + '_ { todo!() }

    // -- Rooms --

    /// Joins or creates a multi-party room with default options.
    pub async fn join_room(&self, ticket: impl Into<RoomTicket>) -> Result<Room, RoomError> {
        self.join_room_with(ticket, Default::default()).await
    }

    /// Joins or creates a multi-party room with custom options.
    ///
    /// Use `RoomOptions` to configure relay transport, auto-subscribe policy, etc.
    pub async fn join_room_with(
        &self,
        ticket: impl Into<RoomTicket>,
        options: RoomOptions,
    ) -> Result<Room, RoomError> {
        todo!()
    }

    // -- Direct broadcast convenience --

    /// Publishes a broadcast on the transport.
    pub async fn publish(
        &self,
        name: impl ToString,
        producer: moq_lite::BroadcastProducer,
    ) -> Result<()> { todo!() }

    /// Subscribes to a remote broadcast by address and name.
    pub async fn subscribe(
        &self,
        remote: impl Into<EndpointAddr>,
        name: &str,
    ) -> Result<Subscription, SubscribeError> { todo!() }

    pub fn shutdown(&self) { todo!() }
}
```

### `Call` — One-to-one sessions

```rust
// iroh-live/src/call.rs

/// Portable handle for initiating a call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallTicket {
    pub(crate) remote: EndpointAddr,
}

impl From<EndpointAddr> for CallTicket { ... }
impl fmt::Display for CallTicket { ... }
impl FromStr for CallTicket { ... }

/// Call lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallState {
    Connecting,
    Connected,
    Disconnected(DisconnectReason),
}

/// Errors from call operations.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("connection failed: {0}")]
    ConnectionFailed(#[source] anyhow::Error),
    #[error("call rejected by remote")]
    Rejected,
    #[error("call ended: {0}")]
    Ended(DisconnectReason),
}

/// Active one-to-one call.
///
/// Media is published and subscribed through the standard moq-media types:
/// [`Broadcast`] for outgoing, [`Subscription`] for incoming.
#[derive(Debug, Clone)]
pub struct Call { inner: Arc<CallInner> }

impl Call {
    pub fn remote_id(&self) -> ParticipantId { todo!() }

    /// Local media — configure with `local().video().set(...)` etc.
    pub fn local(&self) -> &Broadcast { todo!() }

    /// Remote media — subscribe with `remote().video()` etc.
    /// `None` until the remote peer announces a broadcast.
    pub fn remote(&self) -> Option<&Subscription> { todo!() }

    /// Waits until the remote peer's subscription is available.
    ///
    /// Resolves immediately if already available. Prefer this over
    /// polling `remote()` in a loop.
    ///
    /// ```no_run
    /// let call = live.call(remote_addr).await?;
    /// let remote = call.remote_ready().await?;
    /// let video = remote.video()?;
    /// ```
    pub async fn remote_ready(&self) -> Result<&Subscription, CallError> { todo!() }

    /// Returns the current call state.
    pub fn state(&self) -> CallState { todo!() }

    /// Returns a watcher for call state changes.
    pub fn state_watcher(&self) -> impl Watcher<Value = CallState> { todo!() }

    pub fn hangup(&self) { todo!() }
}

/// Incoming call. Must be accepted or rejected. Drop auto-rejects.
#[derive(Debug)]
pub struct IncomingCall { inner: Option<IncomingCallInner> }

impl IncomingCall {
    /// Returns the remote peer's identity.
    pub fn remote_id(&self) -> ParticipantId { todo!() }

    /// Accepts the call and returns the active [`Call`] handle.
    pub async fn accept(mut self) -> Result<Call, CallError> { todo!() }

    /// Rejects the call.
    pub fn reject(mut self) { todo!() }
}

impl Drop for IncomingCall {
    fn drop(&mut self) {
        if let Some(_inner) = self.inner.take() {
            // auto-reject
        }
    }
}
```

### `Room` — Multi-party

Events via `impl Stream` for StreamExt composability. Room queries return
owned handles (cheap Arc clone), not borrowed references.

```rust
// iroh-live/src/room.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTicket {
    pub(crate) room_id: RoomId,
    pub(crate) bootstrap: Vec<EndpointId>,
}

impl RoomTicket {
    pub fn new(room_id: RoomId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self { ... }
    pub fn generate() -> Self { todo!() }
}

/// Configuration for connecting to a moq-relay server.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RelayConfig {
    /// Relay server URL (HTTPS/WebTransport).
    pub url: Url,
    /// JWT authentication token. `None` for anonymous/public access.
    pub token: Option<String>,
}

impl RelayConfig {
    pub fn new(url: impl Into<Url>) -> Self { todo!() }
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into()); self
    }
}

/// Options for joining a room.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RoomOptions {
    /// Optional relay server for media transport. When set, media is
    /// routed through the relay instead of direct P2P. Gossip is still
    /// used for membership discovery.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind { Video, Audio }

/// Events from a room. Carry full handles, not just IDs.
///
/// Use with `StreamExt` for idiomatic async processing:
/// ```no_run
/// use futures::StreamExt;
/// let mut events = room.events();
/// while let Some(event) = events.next().await {
///     match event {
///         RoomEvent::ParticipantJoined(p) => { /* p is a full handle */ }
///         _ => {}
///     }
/// }
/// ```
#[derive(Debug)]
pub enum RoomEvent {
    /// A remote participant connected. The handle contains their
    /// [`Subscription`] — call `p.subscription().video()` to start
    /// receiving video.
    ParticipantJoined(RemoteParticipant),
    /// A remote participant disconnected.
    ParticipantLeft { participant: ParticipantId, reason: DisconnectReason },
    /// A remote participant published a new track.
    TrackPublished { participant: ParticipantId, kind: TrackKind, rendition: String },
    /// A remote participant unpublished a track.
    TrackUnpublished { participant: ParticipantId, kind: TrackKind, rendition: String },
}

/// Errors from room operations.
#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("join failed: {0}")]
    JoinFailed(#[source] anyhow::Error),
    #[error("gossip failure: {0}")]
    GossipFailed(#[source] anyhow::Error),
    #[error("room closed")]
    Closed,
}

#[derive(Debug, Clone)]
pub struct Room { inner: Arc<RoomInner> }

impl Room {
    pub fn id(&self) -> RoomId { todo!() }
    pub fn ticket(&self) -> RoomTicket { todo!() }

    /// Local participant — use `.broadcast()` to configure media.
    pub fn local(&self) -> &LocalParticipant { todo!() }

    /// Returns a stream of room events.
    ///
    /// Prefer this over polling. Use `StreamExt::next()` in an async loop.
    pub fn events(&self) -> impl Stream<Item = RoomEvent> + '_ { todo!() }

    /// Returns a snapshot of all currently connected remote participants.
    ///
    /// Returns owned handles (cheap Arc clones), safe to hold across awaits.
    pub fn remote_participants(&self) -> Vec<RemoteParticipant> { todo!() }

    /// Returns a specific remote participant by ID.
    pub fn remote_participant(&self, id: &ParticipantId) -> Option<RemoteParticipant> { todo!() }

    /// Returns a watcher for the remote participant list.
    ///
    /// Fires when participants join or leave. For per-participant track
    /// changes, use [`events()`](Self::events).
    pub fn remote_participants_watcher(&self) -> impl Watcher<Value = Vec<RemoteParticipant>> {
        todo!()
    }

    pub fn leave(&self) { todo!() }
}
```

### Participants — Thin identity wrappers

These are the only wrappers in iroh-live, and they exist because they add
a genuinely new concept: participant identity tied to a broadcast/subscription.

```rust
// iroh-live/src/room.rs

/// Local participant in a room.
///
/// Wraps a [`Broadcast`] with participant identity. All media operations
/// go through the broadcast directly.
///
/// ```no_run
/// let room = live.join_room(ticket).await?;
/// room.local().broadcast().video().set(camera, codec, presets)?;
/// room.local().broadcast().audio().set(mic, codec, presets)?;
/// ```
#[derive(Debug, Clone)]
pub struct LocalParticipant { inner: Arc<LocalParticipantInner> }

struct LocalParticipantInner {
    id: ParticipantId,
    broadcast: Broadcast,
}

impl LocalParticipant {
    pub fn id(&self) -> ParticipantId { todo!() }
    pub fn broadcast(&self) -> &Broadcast { &self.inner.broadcast }
}

/// Remote participant in a room.
///
/// Wraps a [`Subscription`] with participant identity. All media
/// operations go through the subscription directly.
///
/// ```no_run
/// if let RoomEvent::ParticipantJoined(participant) = events.next().await.unwrap() {
///     let video = participant.subscription().video()?;
///     let audio = participant.subscription().audio(backend).await?;
/// }
/// ```
#[derive(Debug, Clone)]
pub struct RemoteParticipant { inner: Arc<RemoteParticipantInner> }

struct RemoteParticipantInner {
    id: ParticipantId,
    subscription: Subscription,
}

impl RemoteParticipant {
    pub fn id(&self) -> ParticipantId { todo!() }
    pub fn subscription(&self) -> &Subscription { &self.inner.subscription }
}
```

### Types (`types.rs`)

```rust
// iroh-live/src/types.rs

/// Uniquely identifies a participant in a room or call.
///
/// Wraps an iroh `EndpointId`. In a p2p system the endpoint ID
/// *is* the participant identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParticipantId(pub(crate) EndpointId);

impl ParticipantId {
    pub fn as_endpoint_id(&self) -> &EndpointId { &self.0 }
}
impl fmt::Display for ParticipantId { ... }
impl From<EndpointId> for ParticipantId { ... }
impl From<ParticipantId> for EndpointId { ... }

/// Uniquely identifies a room.
///
/// Derived from the gossip topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomId(pub(crate) iroh_gossip::TopicId);

impl fmt::Display for RoomId { ... }

#[derive(Debug, Clone)]
pub enum DisconnectReason {
    LocalClose,
    RemoteClose,
    TransportError(Arc<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for DisconnectReason { ... }
```

---

## Part 3: Relay Patterns

### Zero-transcode relay

Forward packets without decoding. Useful for CDN-style fan-out or geographic
relaying. Catalog is copied, track data forwarded verbatim.

```rust
// Relay a remote stream to local subscribers
let sub = live.subscribe(remote_addr, "concert-stream").await?;
let relay = Broadcast::relay(&sub)?;
live.publish("concert-relay", relay.producer()).await?;
```

Implementation: `Broadcast::relay()` iterates the subscription's catalog,
creates matching `TrackProducer`s, and spawns async tasks that read from the
subscription's `TrackConsumer`s and write to the producers using the existing
`forward_packets()` helper in `pipeline.rs`. Watches for catalog changes and
spawns new forwarding tasks when renditions are added.

### Transcoding relay

Decode, optionally transform, re-encode at different quality/codec.
`VideoTrack` implements `VideoSource`, so it plugs directly into `Broadcast`.

```rust
// Transcode: receive 1080p H.264, re-encode as 360p AV1
let sub = live.subscribe(remote_addr, "source-stream").await?;
let video = sub.video()?;

let relay = Broadcast::new();
relay.video().set_with::<Av1Encoder>(video, [VideoPreset::P360])?;
live.publish("transcoded-stream", relay.producer()).await?;
```

### Room relay

A room participant can relay another participant's media to a non-room
subscriber (e.g., streaming a room call to a wider audience).

```rust
let room = live.join_room(ticket).await?;

// When a participant joins, relay their media
use futures::StreamExt;
let mut events = room.events();
while let Some(event) = events.next().await {
    if let RoomEvent::ParticipantJoined(p) = event {
        let relay = Broadcast::relay(p.subscription())?;
        live.publish(&format!("room-relay-{}", p.id()), relay.producer()).await?;
    }
}
```

---

## Part 4: Key Differences from Original Sketch

| Aspect | Original | Revised |
|---|---|---|
| Publish type | `LocalBroadcast` wrapper in iroh-live | `Broadcast` in moq-media |
| Subscribe type | `RemoteBroadcast` wrapper in iroh-live | `Subscription` in moq-media |
| Video output | `WatchTrack` | `VideoTrack` (also implements `VideoSource`) |
| Subscribe methods | `watch()` / `listen()` | `video()` / `audio()` |
| Quality selection | `Quality` enum only | `VideoTarget { max_pixels, max_bitrate }` + `Quality: Into<VideoTarget>` |
| Options types | In iroh-live `broadcast` module | `VideoOptions` / `AudioOptions` in moq-media, `#[non_exhaustive]` |
| Slot sub-handles | `LocalVideoSlot` / `LocalAudioSlot` in iroh-live | `VideoPublisher` / `AudioPublisher` in moq-media |
| `Call::local()` | `&LocalBroadcast` (wrapper) | `&Broadcast` (moq-media type) |
| `Call::remote()` | `&RemoteBroadcast` (wrapper) | `Option<&Subscription>` (moq-media type) |
| Participant media | Subscription methods on `RemoteTrackPublication` | `participant.subscription().video()` |
| Relay | Not addressed | `Broadcast::relay()` + `VideoTrack: VideoSource` |
| Event model | `recv()` → `Result<RoomEvent>` | `events()` → `impl Stream<Item = RoomEvent>` |
| Incoming calls | `accept_call()` single | `incoming_calls()` → `impl Stream<Item = IncomingCall>` |
| Entry point | `Live::new()` | `Live::builder().spawn().await?` (+ `Live::new()` for advanced) |
| State access | Mixed | Dual-accessor: snapshot + watcher everywhere |
| Errors | `anyhow::Result` everywhere | Domain errors: `PublishError`, `SubscribeError`, `CallError`, `RoomError` |
| Catalog type | `CatalogWrapper` | `CatalogSnapshot` with `VideoTarget`-based selection |
| AV combined | `AvRemoteTrack` | `MediaTracks { video, audio }` |
| Layer 2 module | `iroh_live::broadcast` with duplicate types | No separate module — moq-media IS the broadcast layer |
| Query returns | References | Owned handles (`Vec<RemoteParticipant>`) safe in async |
| Video access | `current_frame` + `next_frame` | + `frames()` → `impl Stream<Item = VideoFrame>` |
| Import ergonomics | Many `use` lines | `iroh_live::prelude::*` |

## Part 5: Patterns & Conventions

### Dual-accessor convention

All types with observable state expose both:
- **Snapshot method**: returns current value, non-blocking
- **Watcher method**: returns `impl Watcher<Value = T>` for change notification

| Type | Snapshot | Watcher / Async |
|---|---|---|
| `Subscription` | `status()` | `status_watcher()` |
| `Subscription` | `catalog()` | `catalog_watcher()` |
| `Call` | `state()` | `state_watcher()` |
| `Call` | `remote()` | `remote_ready().await` |
| `Room` | `remote_participants()` | `remote_participants_watcher()` |

### Builder-lite convention

Configuration structs with <5 fields use `Default` + method chaining, not a
separate builder type. All marked `#[non_exhaustive]` so fields can be added
without breaking changes.

```rust
VideoOptions::default().quality(Quality::High).viewport(1280, 720)
VideoTarget::default().max_pixels(640 * 480)
AudioOptions::default().quality(Quality::High)
```

### Thread safety

All public types `Send + Sync`. Verified with:
```rust
fn assert_send_sync<T: Send + Sync>() {}
assert_send_sync::<Broadcast>();
assert_send_sync::<Subscription>();
assert_send_sync::<VideoTrack>();
assert_send_sync::<Call>();
assert_send_sync::<Room>();
```

### Resource model

Important invariants that must be documented on the types:

**Decoder threads**: Each `sub.video()` call spawns a new OS-level decoder
thread. Calling `sub.video()` twice creates two independent decoders. For a
dashboard with 16 cameras, that's 16 decoder threads. This is by design
(decoders are stateful and not shareable), but users should know:

```rust
// ONE decoder — correct for rendering
let video = sub.video()?;

// TWO decoders — probably a bug, wastes resources
let video1 = sub.video()?;
let video2 = sub.video()?;
```

**AudioBackend lifetime**: The `AudioBackend` must outlive all `AudioTrack`s
created from it. Pass it by reference to `sub.audio(&backend)`. The backend
owns the audio device; tracks route decoded audio to it.

**VideoTrack mutability**: `current_frame(&mut self)` and `next_frame(&mut self)`
require exclusive access because they drain the internal channel. To share
video across tasks, use one task that reads frames and distributes them, or
clone the `Subscription` and create separate `VideoTrack`s (separate decoders).

**Broadcast lifecycle**: A `Broadcast` is inert until its `producer()` is
passed to a transport session via `session.publish()`. The broadcast manages
encoding; the session manages network delivery. Dropping the `Broadcast`
stops encoding and removes tracks from the catalog. Dropping the session
closes the network connection. Both are independent — you can reconnect a
broadcast to a new session by calling `producer()` again.

**Subscription lifecycle**: `Subscription::new()` is async because it waits
for the initial catalog. Once created, `sub.video()` and `sub.audio()` are
synchronous — they use the cached catalog to select a rendition and start
decoding immediately. If the catalog changes (new renditions), the subscription
updates internally; existing tracks continue unchanged.

### Existing types referenced but not defined here

These types already exist in `rusty-codecs` and are reused, not redefined:

- `PlaybackConfig` (`rusty_codecs::config`) — decoder backend selection, pixel format
- `VideoSource` (`rusty_codecs::traits`) — trait for video frame sources (cameras, etc.)
- `AudioSource` / `AudioStreamFactory` — trait for audio sources and backends
- `VideoCodec` / `AudioCodec` — codec selection enums
- `VideoPreset` / `AudioPreset` — resolution/quality presets for encoding
- `VideoFrame` — decoded video frame (may contain GPU texture or CPU buffer)
- `VideoFormat` — resolution, pixel format, framerate metadata
