//! Comprehensive API sketch for the iroh-live ecosystem.
//!
//! Structured as in-file modules mirroring the real crate layout.
//! All types are stubs (`todo!()`). Read as pseudo-Rust design doc.
//!
//! Modules:
//! - `moq_lite` — broadcast transport primitives (used directly from crate)
//! - [`iroh_moq`] — MoQ session and transport management
//! - [`moq_media`] — media publish/subscribe, codecs, tracks
//! - [`iroh_live`] — rooms, calls, participants, high-level entry point

#![allow(unused, dead_code, unreachable_code, unreachable_pub)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use n0_watcher::{Direct, Watcher};

type Result<T, E = anyhow::Error> = std::result::Result<T, E>;

// ── Placeholder external types ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EndpointId([u8; 32]);
impl EndpointId {
    fn fmt_short(&self) -> &str {
        "abcd1234"
    }
}
#[derive(Debug, Clone)]
struct EndpointAddr {
    id: EndpointId,
}
#[derive(Debug, Clone)]
struct Endpoint;
impl Endpoint {
    fn builder() -> EndpointBuilder {
        EndpointBuilder
    }
    async fn bind() -> Result<Self> {
        Self::builder().bind().await
    }
    fn id(&self) -> EndpointId {
        todo!()
    }
}
struct EndpointBuilder;
impl EndpointBuilder {
    fn secret_key(self, _key: SecretKey) -> Self {
        self
    }
    async fn bind(self) -> Result<Endpoint> {
        todo!()
    }
}
#[derive(Debug, Clone)]
struct SecretKey;
impl SecretKey {
    fn generate() -> Self {
        SecretKey
    }
}
#[derive(Debug, Clone)]
struct Gossip;
impl Gossip {
    fn builder() -> GossipBuilder {
        GossipBuilder
    }
}
impl ProtocolHandler for Gossip {}
/// Placeholder for the `iroh_gossip` crate's module-level constants.
mod iroh_gossip {
    pub const ALPN: &[u8] = b"iroh-gossip/0";
}
struct GossipBuilder;
impl GossipBuilder {
    fn spawn(self, _endpoint: Endpoint) -> Gossip {
        todo!()
    }
}
struct Url;

/// Placeholder for `iroh::protocol::Router`.
struct Router;
impl Router {
    fn builder(endpoint: Endpoint) -> RouterBuilder {
        RouterBuilder { endpoint }
    }
    fn endpoint(&self) -> &Endpoint {
        todo!()
    }
    async fn shutdown(&self) -> Result<()> {
        todo!()
    }
}
struct RouterBuilder {
    endpoint: Endpoint,
}
impl RouterBuilder {
    fn accept(self, _alpn: &[u8], _handler: impl ProtocolHandler) -> Self {
        self
    }
    fn spawn(self) -> Router {
        todo!()
    }
}
/// Placeholder for `iroh::protocol::ProtocolHandler`.
trait ProtocolHandler {}

// moq_lite types used directly from the crate — no stubs needed.
use moq_lite::{BroadcastConsumer, BroadcastProducer, OriginConsumer, OriginProducer};

// ═══════════════════════════════════════════════════════════════════════════════
// iroh_moq — MoQ transport layer
// ═══════════════════════════════════════════════════════════════════════════════

/// MoQ session and transport management.
///
/// Wraps moq-lite protocol handling with iroh's QUIC transport. Manages
/// sessions, broadcast fan-out, and the ALPN protocol handler.
mod iroh_moq {
    use super::*;

    /// ALPN protocol identifier for MoQ-over-iroh.
    pub const ALPN: &[u8] = b"iroh-moq/0";

    /// A single MoQ session with one remote peer.
    ///
    /// Each session has two halves:
    /// - **publish** (`OriginProducer`): broadcasts we announce TO the peer
    /// - **subscribe** (`OriginConsumer`): broadcasts the peer announces to US
    ///
    /// The same type works for both direct P2P peers and relay servers.
    /// A relay is just another MoQ endpoint — the protocol is symmetric.
    #[derive(Clone)]
    pub struct MoqSession;

    impl MoqSession {
        /// Announces a broadcast to this peer.
        ///
        /// Takes a [`BroadcastConsumer`] — one reader of the producer's data.
        /// The peer can then subscribe to tracks within this broadcast.
        pub fn publish(&self, _name: &str, _consumer: BroadcastConsumer) {
            todo!()
        }

        /// Waits for the peer to announce a broadcast with this name.
        ///
        /// Returns a consumer to read its track data. Blocks until the peer
        /// announces, or returns an error if the session closes first.
        pub async fn subscribe(
            &self,
            _name: &str,
        ) -> Result<BroadcastConsumer, super::moq_media::SubscribeError> {
            todo!()
        }

        /// Returns the remote peer's endpoint ID, if this is an iroh session.
        ///
        /// Returns `None` for relay URL connections (no iroh identity).
        pub fn remote_id(&self) -> Option<EndpointId> {
            todo!()
        }

        /// Waits until the session is closed.
        pub async fn closed(&self) -> Result<()> {
            todo!()
        }

        /// Closes the session with the given error code and reason.
        pub fn close(&self, _code: u32, _reason: &[u8]) {
            todo!()
        }
    }

    /// MoQ transport layer — manages sessions, broadcast fan-out, ALPN handler.
    ///
    /// Internally runs an actor that:
    /// - Stores a `HashMap<String, BroadcastProducer>` of published broadcasts
    /// - When a new session connects, clones consumers from each producer and
    ///   publishes them to the session (automatic fan-out)
    /// - Handles incoming sessions via the iroh protocol handler
    #[derive(Debug)]
    pub struct Moq;

    impl Moq {
        /// Creates a new MoQ transport bound to the given endpoint.
        pub fn new(_endpoint: Endpoint) -> Self {
            todo!()
        }

        /// Registers a broadcast for fan-out to all sessions.
        ///
        /// The actor stores the [`BroadcastProducer`] and clones a consumer
        /// for each current and future session. This is how "publish once,
        /// serve to any peer" works.
        pub fn publish(&self, _name: &str, _producer: BroadcastProducer) {
            todo!()
        }

        /// Connects to a remote iroh peer and performs the MoQ handshake.
        pub async fn connect(&self, _remote: EndpointAddr) -> Result<MoqSession> {
            todo!()
        }

        /// Connects to a MoQ endpoint by URL (relay server).
        ///
        /// Uses WebTransport over HTTPS. The URL may include auth as a query
        /// parameter (e.g. `?jwt=TOKEN`).
        ///
        /// The relay inverts publish/subscribe: when we publish a broadcast,
        /// the relay subscribes to us (pulls our data). When a remote viewer
        /// connects to the relay and subscribes, the relay fans out.
        // review(claude): connect_url needs either moq-native dep or porting
        // WebTransport URL connect to web-transport-iroh. Start with moq-native.
        pub async fn connect_url(&self, _url: &str) -> Result<MoqSession> {
            todo!()
        }

        /// Stream of incoming MoQ sessions from remote peers.
        pub fn incoming_sessions(&self) -> IncomingSessionStream {
            todo!()
        }

        /// Returns the protocol handler for mounting on an iroh Router.
        pub fn protocol_handler(&self) -> MoqProtocolHandler {
            MoqProtocolHandler
        }

        /// Shuts down the transport, closing all sessions.
        pub fn shutdown(&self) {
            todo!()
        }
    }

    /// Protocol handler that routes incoming iroh connections to the Moq actor.
    pub struct MoqProtocolHandler;
    impl ProtocolHandler for MoqProtocolHandler {}

    /// Stream of incoming MoQ sessions.
    pub struct IncomingSessionStream;
    impl IncomingSessionStream {
        pub async fn next(&mut self) -> Option<IncomingSession> {
            todo!()
        }
    }

    /// An incoming MoQ session, not yet fully accepted into the actor.
    ///
    /// Carries the remote peer's identity. Can be inspected before deciding
    /// whether to accept or reject.
    pub struct IncomingSession {
        _session: MoqSession,
    }
    impl IncomingSession {
        /// Returns the remote peer's endpoint ID.
        pub fn remote_id(&self) -> EndpointId {
            todo!()
        }
        /// Accepts the session, making it available for publish/subscribe.
        pub fn accept(self) -> MoqSession {
            self._session
        }
        /// Rejects the session, closing the connection.
        pub fn reject(self) {
            todo!()
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// moq_media — media publish/subscribe layer
// ═══════════════════════════════════════════════════════════════════════════════

/// Media capture, encoding, decoding, and broadcast management.
///
/// This is the primary API for all media operations. Usable standalone
/// without [`iroh_live`] for non-RTC use cases (streaming, relays, pipelines).
///
/// Key types:
/// - [`LocalBroadcast`] — publish media (encode + announce)
/// - [`RemoteBroadcast`] — subscribe to remote media (decode + render)
/// - [`VideoTrack`] / [`AudioTrack`] — decoded media from a remote broadcast
/// - [`VideoTarget`] — declarative quality/rendition selection
mod moq_media {
    use super::*;

    // ── Traits ──────────────────────────────────────────────────────────────

    /// Source of raw video frames for encoding.
    pub trait VideoSource: Send + 'static {}

    /// Source of raw audio samples for encoding.
    pub trait AudioSource: Send + 'static {}

    /// Factory for platform audio streams (playback sinks).
    pub trait AudioStreamFactory {}

    // ── Codec / format types ────────────────────────────────────────────────

    /// Video frame (decoded pixel data).
    pub struct VideoFrame;

    /// Video codec selection.
    #[derive(Debug, Clone, Copy)]
    pub enum VideoCodec {
        H264,
        Av1,
    }

    /// Audio codec selection.
    #[derive(Debug, Clone, Copy)]
    pub enum AudioCodec {
        Opus,
    }

    /// Video encoding preset (resolution + bitrate profile).
    #[derive(Debug, Clone, Copy)]
    pub enum VideoPreset {
        P1080,
        P720,
        P360,
        P180,
    }

    /// Audio encoding preset (quality profile).
    #[derive(Debug, Clone, Copy)]
    pub enum AudioPreset {
        Hq,
        Lq,
    }

    // ── Capture sources ─────────────────────────────────────────────────────

    /// Camera capture device.
    pub struct CameraCapture;
    impl CameraCapture {
        /// Opens camera by device index.
        pub fn open(_idx: usize) -> Result<Self> {
            todo!()
        }
    }
    impl VideoSource for CameraCapture {}

    /// Screen capture source.
    pub struct ScreenCapture;
    impl ScreenCapture {
        /// Opens screen capture (prompts user for permission).
        pub async fn open() -> Result<Self> {
            todo!()
        }
    }
    impl VideoSource for ScreenCapture {}

    /// Platform audio backend for input/output device management.
    pub struct AudioBackend;
    impl AudioBackend {
        pub fn new() -> Self {
            AudioBackend
        }
        /// Returns the default microphone input source.
        pub async fn default_input(&self) -> Result<MicSource> {
            todo!()
        }
    }
    impl AudioStreamFactory for AudioBackend {}

    /// Microphone input source.
    pub struct MicSource;
    impl AudioSource for MicSource {}

    /// Playback configuration for decoder output.
    #[derive(Debug, Clone, Default)]
    pub struct PlaybackConfig;

    // ── Error types ─────────────────────────────────────────────────────────

    /// Errors from publishing operations.
    #[derive(Debug)]
    pub enum PublishError {
        NoVideoSource,
        NoAudioSource,
        CodecUnavailable(String),
        EncoderFailed(anyhow::Error),
    }

    /// Errors from subscription operations.
    #[derive(Debug)]
    pub enum SubscribeError {
        NotFound,
        NoCatalog,
        RenditionNotFound(String),
        DecoderFailed(anyhow::Error),
        Ended,
    }

    // ── Quality / target selection ──────────────────────────────────────────

    /// Declarative video quality constraint.
    ///
    /// Used to select the best rendition matching the constraint, without
    /// naming specific renditions. The subscription picks the highest quality
    /// rendition that fits within the target.
    #[derive(Debug, Clone, Default)]
    pub struct VideoTarget {
        pub max_pixels: Option<u32>,
        pub max_bitrate_kbps: Option<u32>,
        pub rendition: Option<String>,
    }

    impl VideoTarget {
        pub fn max_pixels(mut self, pixels: u32) -> Self {
            self.max_pixels = Some(pixels);
            self
        }
        pub fn max_bitrate_kbps(mut self, kbps: u32) -> Self {
            self.max_bitrate_kbps = Some(kbps);
            self
        }
        /// Pins to a specific rendition by name, bypassing automatic selection.
        pub fn rendition(mut self, name: impl Into<String>) -> Self {
            self.rendition = Some(name.into());
            self
        }
    }

    /// Convenience quality presets.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Quality {
        Low,
        Medium,
        High,
    }

    impl From<Quality> for VideoTarget {
        fn from(q: Quality) -> Self {
            match q {
                Quality::Low => VideoTarget::default().max_pixels(320 * 240),
                Quality::Medium => VideoTarget::default().max_pixels(640 * 480),
                Quality::High => VideoTarget::default(), // no constraint = highest
            }
        }
    }

    /// Options for video subscription.
    #[derive(Debug, Clone, Default)]
    pub struct VideoOptions {
        pub playback: Option<PlaybackConfig>,
        pub target: Option<VideoTarget>,
        pub viewport: Option<(u32, u32)>,
    }

    impl VideoOptions {
        pub fn target(mut self, target: impl Into<VideoTarget>) -> Self {
            self.target = Some(target.into());
            self
        }
        pub fn quality(mut self, quality: Quality) -> Self {
            self.target = Some(quality.into());
            self
        }
        pub fn viewport(mut self, w: u32, h: u32) -> Self {
            self.viewport = Some((w, h));
            self
        }
        pub fn playback(mut self, config: PlaybackConfig) -> Self {
            self.playback = Some(config);
            self
        }
    }

    /// Options for audio subscription.
    #[derive(Debug, Clone, Default)]
    pub struct AudioOptions {
        pub rendition: Option<String>,
    }

    impl AudioOptions {
        /// Pins to a specific audio rendition by name.
        pub fn rendition(mut self, name: impl Into<String>) -> Self {
            self.rendition = Some(name.into());
            self
        }
    }

    // ── Broadcast status ────────────────────────────────────────────────────

    /// Lifecycle state of a local or remote broadcast.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BroadcastStatus {
        /// Waiting for catalog / initial data.
        Connecting,
        /// Actively receiving / sending media.
        Live,
        /// Producer closed the broadcast.
        Ended,
    }

    // ── CatalogSnapshot ─────────────────────────────────────────────────────

    /// Snapshot of a broadcast's catalog (available renditions and their config).
    ///
    /// Obtained via `remote.catalog().get()`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CatalogSnapshot {
        _inner: (),
    }

    impl CatalogSnapshot {
        /// Returns available video rendition names, sorted by quality (highest first).
        pub fn video_renditions(&self) -> Vec<String> {
            todo!()
        }
        /// Returns available audio rendition names.
        pub fn audio_renditions(&self) -> Vec<String> {
            todo!()
        }
        /// Selects the best video rendition matching the target constraints.
        pub fn select_video(&self, _target: &VideoTarget) -> Option<String> {
            todo!()
        }
        /// Selects the best audio rendition matching the quality level.
        pub fn select_audio(&self, _quality: Quality) -> Option<String> {
            todo!()
        }
        pub fn has_video(&self) -> bool {
            todo!()
        }
        pub fn has_audio(&self) -> bool {
            todo!()
        }
    }

    // ── LocalBroadcast (publishing) ─────────────────────────────────────────

    /// A named collection of media tracks published to subscribers.
    ///
    /// All methods take `&self` (interior mutability, Arc-based). Clone is cheap.
    /// Dropping the last clone stops encoding and removes from transport.
    ///
    /// # Publishing flow
    /// 1. `LocalLocalBroadcast::new()` — create empty broadcast
    /// 2. `broadcast.video().set(source, codec, presets)` — configure video
    /// 3. `broadcast.audio().set(source, codec, presets)` — configure audio
    /// 4. `live.publish("name", &broadcast)` — announce to peers
    #[derive(Debug, Clone)]
    pub struct LocalBroadcast {
        _inner: Arc<()>,
    }

    impl LocalBroadcast {
        /// Creates a new empty broadcast. No encoding until sources are set.
        pub fn new() -> Self {
            Self {
                _inner: Arc::new(()),
            }
        }

        /// Creates a zero-transcode relay from a remote broadcast.
        ///
        /// Copies the catalog and forwards all track data verbatim — no
        /// decoding or re-encoding. Ideal for CDN fan-out and geographic relay.
        ///
        /// Watches for catalog changes and adapts forwarding tasks.
        pub fn relay(_remote: &RemoteBroadcast) -> Result<Self, PublishError> {
            todo!()
        }

        /// Returns the video publishing handle.
        pub fn video(&self) -> VideoPublisher {
            VideoPublisher
        }
        /// Returns the audio publishing handle.
        pub fn audio(&self) -> AudioPublisher {
            AudioPublisher
        }

        pub fn has_video(&self) -> bool {
            todo!()
        }
        pub fn has_audio(&self) -> bool {
            todo!()
        }

        /// Returns a local preview video track (decode our own output).
        pub fn preview(&self, _config: PlaybackConfig) -> Option<VideoTrack> {
            todo!()
        }

        /// Returns the underlying moq-lite producer for direct use.
        pub fn producer(&self) -> BroadcastProducer {
            todo!()
        }

        /// Creates a consumer that reads from this broadcast's producer.
        ///
        /// Shorthand for `broadcast.producer().consume()`. Each consumer is an
        /// independent reader — use this for per-session fan-out.
        pub fn consume(&self) -> BroadcastConsumer {
            self.producer().consume()
        }
    }

    /// Handle for configuring the video track(s) of a [`LocalBroadcast`].
    pub struct VideoPublisher;
    impl VideoPublisher {
        /// Sets the video source, codec, and encoding presets (simulcast renditions).
        ///
        /// Multiple presets = multiple renditions (simulcast). Subscribers
        /// select renditions via [`VideoTarget`].
        pub fn set(
            &self,
            _src: impl VideoSource,
            _codec: VideoCodec,
            _presets: impl IntoIterator<Item = VideoPreset>,
        ) -> Result<(), PublishError> {
            todo!()
        }

        /// Replaces the video source without changing codec or presets.
        ///
        /// Triggers a new keyframe. Subscribers see a seamless transition.
        pub fn replace(&self, _src: impl VideoSource) -> Result<(), PublishError> {
            todo!()
        }

        /// Removes video from the broadcast.
        pub fn clear(&self) {
            todo!()
        }

        /// Pauses/resumes video encoding. Catalog still advertises the track.
        pub fn set_enabled(&self, _enabled: bool) {
            todo!()
        }
        pub fn is_enabled(&self) -> bool {
            todo!()
        }

        /// Returns the current rendition names.
        pub fn renditions(&self) -> Vec<String> {
            todo!()
        }
    }

    /// Handle for configuring the audio track(s) of a [`LocalBroadcast`].
    pub struct AudioPublisher;
    impl AudioPublisher {
        /// Sets the audio source, codec, and encoding presets.
        pub fn set(
            &self,
            _src: impl AudioSource,
            _codec: AudioCodec,
            _presets: impl IntoIterator<Item = AudioPreset>,
        ) -> Result<(), PublishError> {
            todo!()
        }

        /// Replaces the audio source without changing codec or presets.
        pub fn replace(&self, _src: impl AudioSource) -> Result<(), PublishError> {
            todo!()
        }

        /// Removes audio from the broadcast.
        pub fn clear(&self) {
            todo!()
        }

        /// Mutes/unmutes audio. Encoding continues but produces silence.
        pub fn set_muted(&self, _muted: bool) {
            todo!()
        }
        pub fn is_muted(&self) -> bool {
            todo!()
        }
    }

    // ── RemoteBroadcast (receiving) ─────────────────────────────────────────

    /// A remote broadcast we are subscribed to.
    ///
    /// Created from a [`BroadcastConsumer`] obtained via
    /// `session.subscribe(name)`. Construction is async because it waits for
    /// the catalog (metadata about available tracks/renditions).
    ///
    /// Stores a clone of the [`MoqSession`] it was created from. Access it
    /// via [`session()`](Self::session) for stats, close, etc.
    ///
    /// Once created, `video()` and `audio()` are sync — they use the cached
    /// catalog to spawn decoder threads immediately.
    #[derive(Debug, Clone)]
    pub struct RemoteBroadcast {
        _inner: Arc<()>,
    }

    impl RemoteBroadcast {
        /// Subscribes to a named broadcast on the given session.
        ///
        /// Calls `session.subscribe(name)` internally, then waits for the
        /// initial catalog before returning.
        pub async fn new(
            _session: super::iroh_moq::MoqSession,
            _name: impl Into<String>,
        ) -> Result<Self, SubscribeError> {
            // internally: let consumer = session.subscribe(name).await?;
            todo!()
        }

        /// Returns the underlying MoQ session.
        ///
        /// Useful for connection stats, remote identity, or closing the session.
        pub fn session(&self) -> &super::iroh_moq::MoqSession {
            todo!()
        }

        /// Returns the broadcast name.
        pub fn name(&self) -> &str {
            todo!()
        }

        // -- Status (watcher pattern: use .get() for snapshot, .updated().await to react) --

        /// Returns a watcher for the broadcast lifecycle status.
        pub fn status(&self) -> Direct<BroadcastStatus> {
            todo!()
        }

        /// Returns a watcher for the catalog (renditions added/removed).
        pub fn catalog(&self) -> Direct<CatalogSnapshot> {
            todo!()
        }

        /// Convenience: checks if the catalog has video renditions.
        pub fn has_video(&self) -> bool {
            todo!()
        }
        /// Convenience: checks if the catalog has audio renditions.
        pub fn has_audio(&self) -> bool {
            todo!()
        }
        /// Returns available video rendition names.
        pub fn video_renditions(&self) -> Vec<String> {
            todo!()
        }
        /// Returns available audio rendition names.
        pub fn audio_renditions(&self) -> Vec<String> {
            todo!()
        }

        // -- Video --

        /// Subscribes to video with automatic rendition selection.
        ///
        /// Spawns a decoder thread. Returns immediately (sync) — uses cached catalog.
        pub fn video(&self) -> Result<VideoTrack, SubscribeError> {
            self.video_with(Default::default())
        }

        /// Subscribes to video with explicit options (quality target, viewport).
        pub fn video_with(&self, _opts: VideoOptions) -> Result<VideoTrack, SubscribeError> {
            todo!()
        }

        // -- Audio --

        /// Subscribes to audio with default settings.
        ///
        /// Spawns a decoder thread and connects to the audio backend for playback.
        pub async fn audio(
            &self,
            _backend: &dyn AudioStreamFactory,
        ) -> Result<AudioTrack, SubscribeError> {
            self.audio_with(Default::default(), _backend).await
        }

        /// Subscribes to audio with explicit options.
        pub async fn audio_with(
            &self,
            _opts: AudioOptions,
            _backend: &dyn AudioStreamFactory,
        ) -> Result<AudioTrack, SubscribeError> {
            todo!()
        }

        // -- Convenience --

        /// Subscribes to both video and audio in one call.
        pub async fn media(
            &self,
            _backend: &dyn AudioStreamFactory,
        ) -> Result<MediaTracks, SubscribeError> {
            todo!()
        }

        /// Waits until the broadcast ends.
        pub async fn closed(&self) {
            todo!()
        }

        /// Actively shuts down this remote broadcast (unsubscribes from all tracks).
        pub fn shutdown(&self) {
            todo!()
        }
    }

    /// Combined video + audio tracks from a single remote broadcast.
    pub struct MediaTracks {
        pub video: Option<VideoTrack>,
        pub audio: Option<AudioTrack>,
    }

    // ── VideoTrack ──────────────────────────────────────────────────────────

    /// Decoded video from a remote broadcast. Each instance owns a decoder thread.
    ///
    /// Three access patterns:
    /// - `current_frame()` — latest frame snapshot (may skip frames, ideal for UI)
    /// - `next_frame()` — async pull, waits for next frame
    /// - `frames()` — `Stream` of all frames in order (for recording/processing)
    pub struct VideoTrack {
        _inner: (),
    }

    impl VideoTrack {
        /// Returns the rendition name this track is decoding.
        pub fn rendition(&self) -> &str {
            todo!()
        }

        /// Returns the latest decoded frame, or `None` if no frame yet.
        ///
        /// Non-blocking. May skip frames — always returns the most recent.
        /// Ideal for render loops.
        pub fn current_frame(&mut self) -> Option<VideoFrame> {
            todo!()
        }

        /// Waits for the next decoded frame.
        pub async fn next_frame(&mut self) -> Option<VideoFrame> {
            todo!()
        }

        /// Returns a stream of decoded frames in order.
        ///
        /// Unlike `current_frame()`, delivers every frame sequentially.
        /// Use for recording or frame-accurate processing.
        pub fn frames(&mut self) -> VideoFrameStream {
            todo!()
        }

        /// Updates the viewport hint for rendition auto-switching.
        pub fn set_viewport(&self, _w: u32, _h: u32) {
            todo!()
        }
    }

    /// `VideoTrack` implements `VideoSource` for transcode relay.
    ///
    /// Plug a decoded track directly into a [`LocalBroadcast`] as if it were a
    /// camera — the broadcast re-encodes at the configured presets.
    impl VideoSource for VideoTrack {}

    /// Stream of decoded video frames (placeholder for `impl Stream<Item = VideoFrame>`).
    pub struct VideoFrameStream;
    impl VideoFrameStream {
        pub async fn next(&mut self) -> Option<VideoFrame> {
            todo!()
        }
    }

    // ── AudioTrack ──────────────────────────────────────────────────────────

    /// Decoded audio from a remote broadcast. Plays through the audio backend automatically.
    pub struct AudioTrack {
        _inner: (),
    }

    impl AudioTrack {
        /// Returns the rendition name this track is decoding.
        pub fn rendition(&self) -> &str {
            todo!()
        }
        /// Waits until the audio stream ends.
        pub async fn stopped(&self) {
            todo!()
        }
        /// Sets playback volume (0.0 = mute, 1.0 = full).
        pub fn set_volume(&self, _volume: f32) {
            todo!()
        }
    }

    // ── Display / Error impls ───────────────────────────────────────────────

    impl std::fmt::Display for PublishError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for PublishError {}

    impl std::fmt::Display for SubscribeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for SubscribeError {}
}

// ═══════════════════════════════════════════════════════════════════════════════
// iroh_live — rooms, calls, participants
// ═══════════════════════════════════════════════════════════════════════════════

/// High-level API for live audio/video sessions and rooms.
///
/// Re-exports [`moq_media`] types and adds signaling, identity, and room
/// semantics. Only wraps where genuinely new concepts are needed (rooms,
/// calls, participants).
///
/// Entry point: [`Live::builder()`].
mod iroh_live {
    use super::iroh_moq::*;
    use super::*;

    use super::moq_media::*;

    /// ALPN protocol identifier (re-exported from `iroh_moq`).
    pub const ALPN: &[u8] = super::iroh_moq::ALPN;

    // ── IDs ─────────────────────────────────────────────────────────────────

    /// Unique identifier for a participant, derived from their iroh endpoint ID.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ParticipantId(EndpointId);

    impl std::fmt::Display for ParticipantId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0.fmt_short())
        }
    }

    /// Unique identifier for a room, derived from the gossip topic.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct RoomId([u8; 32]);

    impl std::fmt::Display for RoomId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", &self.0[..4])
        }
    }

    // ── Error types ─────────────────────────────────────────────────────────

    /// Disconnect reason for calls and room participants.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DisconnectReason {
        LocalClose,
        RemoteClose,
        TransportError,
    }

    impl std::fmt::Display for DisconnectReason {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::LocalClose => write!(f, "local close"),
                Self::RemoteClose => write!(f, "remote close"),
                Self::TransportError => write!(f, "transport error"),
            }
        }
    }

    /// Errors from call operations.
    #[derive(Debug)]
    pub enum CallError {
        ConnectionFailed(anyhow::Error),
        Rejected,
        Ended(DisconnectReason),
    }

    impl std::fmt::Display for CallError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for CallError {}

    /// Errors from room operations.
    #[derive(Debug)]
    pub enum RoomError {
        JoinFailed(anyhow::Error),
        GossipFailed(anyhow::Error),
        Closed,
    }

    impl std::fmt::Display for RoomError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for RoomError {}

    // ── Live (entry point) ──────────────────────────────────────────────────

    /// Entry point for iroh-live. Manages the iroh endpoint, MoQ transport,
    /// and optionally gossip for room membership.
    ///
    /// Two construction patterns:
    ///
    /// ```rust,ignore
    /// // 1. Simple: creates router internally
    /// let live = Live::builder(endpoint)
    ///     .enable_gossip()
    ///     .spawn_with_router();
    ///
    /// // 2. Compose: bring your own router
    /// let live = Live::builder(endpoint.clone())
    ///     .enable_gossip()
    ///     .spawn();
    /// let mut router = Router::builder(endpoint);
    /// router = live.register_protocols(router);
    /// let router = router.accept(my_alpn, my_handler).spawn();
    /// ```
    #[derive(Clone)]
    pub struct Live {
        endpoint: Endpoint,
        moq: Arc<Moq>,
        gossip: Option<Arc<Gossip>>,
        _router: Option<Arc<Router>>,
    }

    impl std::fmt::Debug for Live {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Live")
                .field("endpoint", &self.endpoint)
                .finish_non_exhaustive()
        }
    }

    /// Builder for [`Live`]. Always starts from an endpoint.
    pub struct LiveBuilder {
        endpoint: Endpoint,
        gossip: Option<Arc<Gossip>>,
    }

    impl LiveBuilder {
        /// Enables gossip (required for rooms). Creates a Gossip instance
        /// internally and auto-mounts it via [`Live::register_protocols`].
        pub fn enable_gossip(mut self) -> Self {
            self.gossip = Some(Arc::new(Gossip::builder().spawn(self.endpoint.clone())));
            self
        }

        /// Sets an externally-managed gossip instance (for rooms).
        ///
        /// Use this instead of `enable_gossip()` when you manage Gossip
        /// yourself. You are responsible for mounting it on the Router
        /// (it will NOT be auto-mounted by `register_protocols`).
        pub fn gossip(mut self, gossip: Gossip) -> Self {
            self.gossip = Some(Arc::new(gossip));
            self
        }

        /// Creates the MoQ transport and optional gossip. Does NOT create a Router.
        ///
        /// Use [`Live::register_protocols`] to mount on your own Router:
        /// ```rust,ignore
        /// let live = Live::builder(endpoint.clone()).spawn();
        /// let mut router = Router::builder(endpoint);
        /// router = live.register_protocols(router);
        /// let router = router.accept(my_alpn, my_handler).spawn();
        /// ```
        pub fn spawn(self) -> Live {
            let moq = Moq::new(self.endpoint.clone());
            Live {
                endpoint: self.endpoint,
                moq: Arc::new(moq),
                gossip: self.gossip,
                _router: None,
            }
        }

        /// Creates the MoQ transport, optional gossip, AND a Router.
        ///
        /// The Router is stored on Live and kept alive for its lifetime.
        /// All Live protocols are auto-mounted.
        pub fn spawn_with_router(self) -> Live {
            let endpoint = self.endpoint.clone();
            let mut live = self.spawn();
            let router = live.register_protocols(Router::builder(endpoint));
            live._router = Some(Arc::new(router.spawn()));
            live
        }
    }

    impl Live {
        /// Returns a builder. Always starts from an existing endpoint.
        pub fn builder(endpoint: Endpoint) -> LiveBuilder {
            LiveBuilder {
                endpoint,
                gossip: None,
            }
        }

        /// Mounts Live's protocol handlers on a RouterBuilder.
        ///
        /// Registers the MoQ ALPN handler and, if gossip was enabled via
        /// `enable_gossip()`, the gossip ALPN handler too. Returns the
        /// RouterBuilder for further chaining.
        pub fn register_protocols(&self, router: RouterBuilder) -> RouterBuilder {
            let router = router.accept(iroh_moq::ALPN, self.moq.protocol_handler());
            // if let Some(ref gossip) = self.gossip {
            //     router = router.accept(iroh_gossip::ALPN, gossip.clone());
            // }
            router
        }

        /// Returns the MoQ protocol handler for manual Router mounting.
        ///
        /// Use this when you need fine-grained control over which protocols
        /// are mounted (e.g. with `set_gossip()` where you mount gossip yourself).
        pub fn protocol_handler(&self) -> MoqProtocolHandler {
            self.moq.protocol_handler()
        }

        /// Returns the iroh endpoint.
        pub fn endpoint(&self) -> &Endpoint {
            &self.endpoint
        }

        /// Returns the MoQ transport handle for direct/advanced operations.
        pub fn transport(&self) -> &Moq {
            &self.moq
        }

        // -- Direct broadcast --

        /// Announces a broadcast so any peer (current + future) can subscribe.
        ///
        /// Delegates to `self.transport().publish()` internally. The broadcast's
        /// producer is stored in the MoQ actor; consumers are cloned per session.
        pub fn publish(&self, name: &str, broadcast: &LocalBroadcast) {
            self.moq.publish(name, broadcast.producer());
        }

        /// Connects to a remote peer and subscribes to a named broadcast.
        ///
        /// Convenience for `transport().connect()` + `session.subscribe()` +
        /// `RemoteBroadcast::new()`.
        pub async fn subscribe(
            &self,
            _remote: EndpointAddr,
            _name: &str,
        ) -> Result<RemoteBroadcast, SubscribeError> {
            todo!()
        }

        // -- Relay convenience --

        /// Connects to a moq-relay server.
        pub async fn connect_relay(&self, _config: RelayConfig) -> Result<MoqSession> {
            todo!()
        }

        /// Publishes a broadcast to a relay for fan-out.
        ///
        /// Convenience for `connect_relay()` + `session.publish()`.
        /// Returns the session (caller must keep it alive).
        pub async fn publish_to_relay(
            &self,
            _config: RelayConfig,
            _name: &str,
            _broadcast: &LocalBroadcast,
        ) -> Result<MoqSession> {
            todo!()
        }

        /// Subscribes to a broadcast from a relay.
        ///
        /// The session is stored on the returned [`RemoteBroadcast`] — access
        /// it via `.session()` if needed.
        pub async fn subscribe_from_relay(
            &self,
            _config: RelayConfig,
            _name: &str,
        ) -> Result<RemoteBroadcast> {
            todo!()
        }

        // -- Rooms --

        /// Joins a room with default options.
        pub async fn join_room(&self, ticket: impl Into<RoomTicket>) -> Result<Room, RoomError> {
            self.join_room_with(ticket, Default::default()).await
        }

        /// Joins a room with explicit options (relay, auto-subscribe, etc).
        pub async fn join_room_with(
            &self,
            _ticket: impl Into<RoomTicket>,
            _opts: RoomOptions,
        ) -> Result<Room, RoomError> {
            todo!()
        }

        /// Shuts down the Live instance, closing all sessions and rooms.
        pub fn shutdown(&self) {
            todo!()
        }
    }

    // ── Relay config ────────────────────────────────────────────────────────

    /// Configuration for connecting to a moq-relay server.
    #[derive(Debug, Clone)]
    pub struct RelayConfig {
        pub url: String,
        pub token: Option<String>,
    }

    impl RelayConfig {
        pub fn new(url: impl Into<String>) -> Self {
            Self {
                url: url.into(),
                token: None,
            }
        }
        /// Sets the JWT authentication token.
        pub fn token(mut self, token: impl Into<String>) -> Self {
            self.token = Some(token.into());
            self
        }
    }

    // ── Call ─────────────────────────────────────────────────────────────────

    /// Standalone 1:1 call helper. Pure sugar over MoQ primitives.
    ///
    /// What it does internally:
    /// 1. Connects to remote peer (or accepts incoming session)
    /// 2. Creates + publishes a [`LocalBroadcast`] named "call"
    /// 3. Subscribes to the remote's broadcast → [`RemoteBroadcast`]
    /// 4. Wraps both in a handle with state tracking
    ///
    /// Everything Call does can be done directly with
    /// [`Live::transport()`] + [`LocalBroadcast`] + [`RemoteBroadcast`].
    pub struct Call {
        session: MoqSession,
        local: LocalBroadcast,
        remote: Option<RemoteBroadcast>,
    }

    impl Call {
        /// Dials a remote peer. Connects, publishes local, subscribes to remote.
        pub async fn dial(live: &Live, remote: EndpointAddr) -> Result<Self, CallError> {
            let session = live
                .transport()
                .connect(remote)
                .await
                .map_err(CallError::ConnectionFailed)?;
            let local = LocalBroadcast::new();
            session.publish("call", local.consume());

            let remote = RemoteBroadcast::new(session.clone(), "call")
                .await
                .map_err(|_| CallError::Rejected)?;

            Ok(Self {
                session,
                local,
                remote: Some(remote),
            })
        }

        /// Accepts an incoming session as a call.
        pub async fn accept(session: MoqSession) -> Result<Self, CallError> {
            let local = LocalBroadcast::new();
            session.publish("call", local.consume());

            let remote = RemoteBroadcast::new(session.clone(), "call")
                .await
                .map_err(|_| CallError::Rejected)?;

            Ok(Self {
                session,
                local,
                remote: Some(remote),
            })
        }

        /// Returns the local broadcast (configure video/audio here).
        pub fn local(&self) -> &LocalBroadcast {
            &self.local
        }

        /// Returns the remote broadcast (subscribe to video/audio here).
        pub fn remote(&self) -> Option<&RemoteBroadcast> {
            self.remote.as_ref()
        }

        /// Waits until the call ends.
        pub async fn closed(&self) -> DisconnectReason {
            let _ = self.session.closed().await;
            DisconnectReason::RemoteClose
        }
    }

    // ── Room ────────────────────────────────────────────────────────────────

    /// Shareable ticket for joining a room.
    #[derive(Debug, Clone)]
    pub struct RoomTicket {
        pub topic_id: [u8; 32],
        pub bootstrap: Vec<EndpointId>,
    }

    impl RoomTicket {
        /// Generates a new room ticket with a random topic.
        pub fn generate() -> Self {
            todo!()
        }
    }

    /// Options for joining a room.
    #[derive(Debug, Clone, Default)]
    pub struct RoomOptions {
        /// Optional relay for media transport (SFU mode).
        pub relay: Option<RelayConfig>,
        /// Auto-subscribe to all participants' media on join.
        pub auto_subscribe: bool,
    }

    impl RoomOptions {
        pub fn relay(mut self, config: RelayConfig) -> Self {
            self.relay = Some(config);
            self
        }
        pub fn auto_subscribe(mut self, enabled: bool) -> Self {
            self.auto_subscribe = enabled;
            self
        }
    }

    /// Media track kind (used in room events).
    #[derive(Debug, Clone, Copy)]
    pub enum TrackKind {
        Video,
        Audio,
    }

    /// Discrete room events, delivered as a stream.
    ///
    /// Events carry full handles where appropriate — `ParticipantJoined`
    /// includes the [`RemoteParticipant`] so you can immediately call
    /// `p.broadcast(TrackName::Camera)` without a follow-up lookup.
    pub enum RoomEvent {
        ParticipantJoined(RemoteParticipant),
        ParticipantLeft {
            participant: ParticipantId,
            reason: DisconnectReason,
        },
        /// A participant published a new track (e.g. started screen sharing).
        TrackPublished {
            participant: ParticipantId,
            track: TrackName,
            kind: TrackKind,
            rendition: String,
        },
        /// A participant unpublished a track.
        TrackUnpublished {
            participant: ParticipantId,
            track: TrackName,
            kind: TrackKind,
            rendition: String,
        },
    }

    /// Multi-party room with participant model and event stream.
    #[derive(Debug, Clone)]
    pub struct Room {
        _inner: Arc<()>,
    }

    impl Room {
        /// Returns the room's unique ID.
        pub fn id(&self) -> RoomId {
            todo!()
        }
        /// Returns a shareable ticket for this room.
        pub fn ticket(&self) -> RoomTicket {
            todo!()
        }
        /// Returns the local participant (our broadcast handle).
        pub fn local(&self) -> &LocalParticipant {
            todo!()
        }
        /// Returns a watcher for the remote participants list.
        ///
        /// Call `.get()` for a snapshot, `.updated().await` to react to changes.
        pub fn remote_participants(&self) -> Direct<Vec<RemoteParticipant>> {
            todo!()
        }
        /// Returns a stream of discrete room events.
        pub fn events(&self) -> RoomEventStream {
            todo!()
        }
        /// Leaves the room gracefully.
        pub fn leave(&self) {
            todo!()
        }
    }

    /// Stream of [`RoomEvent`]s.
    pub struct RoomEventStream;
    impl RoomEventStream {
        pub async fn next(&mut self) -> Option<RoomEvent> {
            todo!()
        }
    }

    /// Well-known track names within a room.
    ///
    /// The room prepends its own namespace, so `TrackName::Camera` becomes
    /// e.g. `"room-abc/camera"` on the wire.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub enum TrackName {
        /// Primary camera + mic broadcast.
        Camera,
        /// Screen share broadcast.
        Screen,
        /// Custom named broadcast.
        Other(String),
    }

    impl std::fmt::Display for TrackName {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Camera => write!(f, "camera"),
                Self::Screen => write!(f, "screen"),
                Self::Other(s) => write!(f, "{s}"),
            }
        }
    }

    /// The local participant in a room.
    #[derive(Debug, Clone)]
    pub struct LocalParticipant {
        _inner: Arc<()>,
    }
    impl LocalParticipant {
        pub fn id(&self) -> ParticipantId {
            todo!()
        }
        /// Returns the broadcast handle for the given track name.
        ///
        /// Creates the broadcast lazily on first access. The room handles
        /// namespace prefixing and announcing to other participants.
        pub fn broadcast(&self, _name: TrackName) -> &LocalBroadcast {
            todo!()
        }
    }

    /// A remote participant in a room.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RemoteParticipant {
        _inner: Arc<()>,
    }
    impl RemoteParticipant {
        pub fn id(&self) -> ParticipantId {
            todo!()
        }
        /// Returns the remote broadcast for the given track name.
        ///
        /// Returns `None` if the participant hasn't published that track.
        pub fn broadcast(&self, _name: TrackName) -> Option<&RemoteBroadcast> {
            todo!()
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// EXAMPLES — implemented against the stub API above
// ═══════════════════════════════════════════════════════════════════════════════

/// One-to-many streaming — publish from zero, subscribe from anywhere.
mod ex_one_to_many {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn publish() -> Result<()> {
        let endpoint = Endpoint::builder()
            .secret_key(SecretKey::generate())
            .bind()
            .await?;
        let live = Live::builder(endpoint).spawn_with_router();

        let camera = CameraCapture::open(0)?;
        let audio_backend = AudioBackend::new();
        let mic = audio_backend.default_input().await?;

        let broadcast = LocalBroadcast::new();
        broadcast.video().set(
            camera,
            VideoCodec::H264,
            [VideoPreset::P720, VideoPreset::P360],
        )?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        live.publish("my-stream", &broadcast);
        println!("listening on {:?}", live.endpoint());

        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }

    async fn subscribe(publisher_addr: EndpointAddr) -> Result<()> {
        let endpoint = Endpoint::bind().await?;
        let live = Live::builder(endpoint).spawn_with_router();
        let sub = live.subscribe(publisher_addr, "my-stream").await?;

        let mut video = sub.video()?;
        let audio_backend = AudioBackend::new();
        let _audio = sub.audio(&audio_backend).await?;

        loop {
            if let Some(_frame) = video.current_frame() {
                // render(frame)
            }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }
}

/// One-to-one call WITHOUT the Call helper — raw primitives.
mod ex_call_raw {
    use super::iroh_live::*;
    use super::iroh_moq::*;
    use super::moq_media::*;
    use super::*;

    async fn caller(live: Live, remote: EndpointAddr) -> Result<()> {
        let session = live.transport().connect(remote).await?;

        let camera = CameraCapture::open(0)?;
        let audio_backend = AudioBackend::new();
        let mic = audio_backend.default_input().await?;

        let broadcast = LocalBroadcast::new();
        broadcast
            .video()
            .set(camera, VideoCodec::H264, [VideoPreset::P720])?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;
        session.publish("call", broadcast.consume());

        let sub = RemoteBroadcast::new(session.clone(), "call").await?;
        let mut video = sub.video()?;
        let _audio = sub.audio(&audio_backend).await?;

        loop {
            if let Some(_frame) = video.current_frame() { /* render */ }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }

    async fn receiver(live: Live) -> Result<()> {
        let mut incoming = live.transport().incoming_sessions();
        while let Some(incoming_session) = incoming.next().await {
            let session = incoming_session.accept();

            let camera = CameraCapture::open(0)?;
            let broadcast = LocalBroadcast::new();
            broadcast
                .video()
                .set(camera, VideoCodec::H264, [VideoPreset::P720])?;
            session.publish("call", broadcast.consume());

            let sub = RemoteBroadcast::new(session.clone(), "call").await?;
            let mut _video = sub.video()?;
        }
        Ok(())
    }
}

/// Same call using the Call convenience helper.
mod ex_call_with_helper {
    use super::iroh_live::*;
    use super::iroh_moq::*;
    use super::moq_media::*;
    use super::*;

    async fn caller(live: Live, remote: EndpointAddr) -> Result<()> {
        let call = Call::dial(&live, remote).await?;

        let camera = CameraCapture::open(0)?;
        call.local()
            .video()
            .set(camera, VideoCodec::H264, [VideoPreset::P720])?;

        let audio_backend = AudioBackend::new();
        let mic = audio_backend.default_input().await?;
        call.local()
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        let remote_sub = call.remote().expect("dial waits for remote");
        let mut video = remote_sub.video()?;
        let _audio = remote_sub.audio(&audio_backend).await?;

        loop {
            if let Some(_frame) = video.current_frame() { /* render */ }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }

    async fn receiver(live: Live) -> Result<()> {
        let mut incoming = live.transport().incoming_sessions();
        while let Some(incoming_session) = incoming.next().await {
            let call = Call::accept(incoming_session.accept()).await?;

            let camera = CameraCapture::open(0)?;
            call.local()
                .video()
                .set(camera, VideoCodec::H264, [VideoPreset::P720])?;

            let remote_sub = call.remote().expect("accept waits for remote");
            let mut _video = remote_sub.video()?;
        }
        Ok(())
    }
}

/// Call with screen share — caller shares screen as a second broadcast.
mod ex_call_with_screenshare {
    use super::iroh_live::*;
    use super::iroh_moq::*;
    use super::moq_media::*;
    use super::*;

    async fn caller_with_screenshare(live: Live, remote: EndpointAddr) -> Result<()> {
        let session = live.transport().connect(remote).await?;
        let audio_backend = AudioBackend::new();

        // Camera + mic broadcast
        let camera_broadcast = LocalBroadcast::new();
        let camera = CameraCapture::open(0)?;
        let mic = audio_backend.default_input().await?;
        camera_broadcast
            .video()
            .set(camera, VideoCodec::H264, [VideoPreset::P720])?;
        camera_broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;
        session.publish("camera", camera_broadcast.consume());

        // Screen share as a separate broadcast — independent lifecycle
        let screen_broadcast = LocalBroadcast::new();
        let screen = ScreenCapture::open().await?;
        screen_broadcast
            .video()
            .set(screen, VideoCodec::H264, [VideoPreset::P1080])?;
        session.publish("screen", screen_broadcast.consume());

        // Subscribe to remote's camera + optional screen
        let cam_sub = RemoteBroadcast::new(session.clone(), "camera").await?;
        let mut cam_video = cam_sub.video()?;
        let _cam_audio = cam_sub.audio(&audio_backend).await?;

        // Screen share is optional — remote may or may not share
        if let Ok(screen_sub) = RemoteBroadcast::new(session.clone(), "screen").await {
            let mut _screen_video = screen_sub.video()?;
            // render screen share in a separate panel
        }

        // Stop screen share later (drop stops encoding + un-announces)
        drop(screen_broadcast);

        session.closed().await?;
        Ok(())
    }
}

/// Room with screen share — participants can share both camera and screen.
mod ex_room_with_screenshare {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn room_with_screenshare(live: Live, ticket: RoomTicket) -> Result<()> {
        let room = live.join_room(ticket).await?;
        let audio_backend = AudioBackend::new();

        // Camera + mic
        let camera = CameraCapture::open(0)?;
        room.local().broadcast(TrackName::Camera).video().set(
            camera,
            VideoCodec::H264,
            [VideoPreset::P720],
        )?;
        let mic = audio_backend.default_input().await?;
        room.local().broadcast(TrackName::Camera).audio().set(
            mic,
            AudioCodec::Opus,
            [AudioPreset::Hq],
        )?;

        // Screen share — just another TrackName
        let screen = ScreenCapture::open().await?;
        room.local().broadcast(TrackName::Screen).video().set(
            screen,
            VideoCodec::H264,
            [VideoPreset::P1080],
        )?;

        let mut events = room.events();
        while let Some(event) = events.next().await {
            match event {
                RoomEvent::ParticipantJoined(p) => {
                    // Subscribe to their camera
                    if let Some(cam) = p.broadcast(TrackName::Camera) {
                        let mut _video = cam.video()?;
                        let _audio = cam.audio(&audio_backend).await?;
                    }
                    // Subscribe to their screen share (if they have one)
                    if let Some(screen) = p.broadcast(TrackName::Screen) {
                        let mut _video = screen.video()?;
                    }
                }
                RoomEvent::ParticipantLeft {
                    participant,
                    reason,
                } => {
                    println!("{participant} left ({reason})");
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Multi-party room with event-driven participant handling.
mod ex_room {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn room_participant(live: Live, ticket: RoomTicket) -> Result<()> {
        let room = live.join_room(ticket).await?;
        let audio_backend = AudioBackend::new();

        let camera = CameraCapture::open(0)?;
        room.local().broadcast(TrackName::Camera).video().set(
            camera,
            VideoCodec::H264,
            [VideoPreset::P720],
        )?;
        let mic = audio_backend.default_input().await?;
        room.local().broadcast(TrackName::Camera).audio().set(
            mic,
            AudioCodec::Opus,
            [AudioPreset::Hq],
        )?;

        println!("Room ID: {}, share ticket: {:?}", room.id(), room.ticket());

        let mut events = room.events();
        while let Some(event) = events.next().await {
            match event {
                RoomEvent::ParticipantJoined(p) => {
                    println!("{} joined", p.id());
                    if let Some(cam) = p.broadcast(TrackName::Camera) {
                        let mut _video = cam.video()?;
                        let _audio = cam.audio(&audio_backend).await?;
                    }
                }
                RoomEvent::ParticipantLeft {
                    participant,
                    reason,
                } => {
                    println!("{participant} left ({reason})");
                }
                RoomEvent::TrackPublished {
                    participant,
                    track,
                    kind,
                    rendition,
                } => {
                    println!("{participant} published {track} {kind:?} rendition {rendition}");
                }
                RoomEvent::TrackUnpublished {
                    participant,
                    track,
                    kind,
                    rendition,
                } => {
                    println!("{participant} unpublished {track} {kind:?} {rendition}");
                }
            }
        }
        Ok(())
    }
}

/// Zero-transcode relay — forward packets without decoding.
mod ex_relay {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn relay(live: Live, source_addr: EndpointAddr) -> Result<()> {
        let sub = live.subscribe(source_addr, "concert-stream").await?;
        let relay = LocalBroadcast::relay(&sub)?;
        live.publish("concert-relay", &relay);
        sub.closed().await;
        Ok(())
    }
}

/// Transcoding relay — decode then re-encode at different quality.
mod ex_transcode {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn transcode(live: Live, source_addr: EndpointAddr) -> Result<()> {
        let sub = live.subscribe(source_addr, "hd-stream").await?;
        let video = sub.video()?;
        let relay = LocalBroadcast::new();
        relay
            .video()
            .set(video, VideoCodec::H264, [VideoPreset::P360])?;
        live.publish("sd-stream", &relay);
        sub.closed().await;
        Ok(())
    }
}

/// Camera dashboard with quality constraints.
mod ex_dashboard {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn dashboard(live: Live, cameras: Vec<(EndpointAddr, &str)>) -> Result<()> {
        let mut tracks: Vec<(String, VideoTrack)> = Vec::new();

        for (addr, name) in cameras {
            let sub = live.subscribe(addr, name).await?;
            let video = sub.video_with(
                VideoOptions::default().target(VideoTarget::default().max_pixels(320 * 180)),
            )?;
            tracks.push((name.to_string(), video));
        }

        loop {
            for (name, track) in &mut tracks {
                if let Some(_frame) = track.current_frame() {
                    let _ = name; // render_tile(name, frame)
                }
            }
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    }
}

/// Camera switch mid-stream — seamless source replacement.
mod ex_camera_switch {
    use super::moq_media::*;
    use super::*;

    async fn camera_switch() -> Result<()> {
        let broadcast = LocalBroadcast::new();

        let front = CameraCapture::open(0)?;
        broadcast
            .video()
            .set(front, VideoCodec::H264, [VideoPreset::P720])?;

        let audio_backend = AudioBackend::new();
        let mic = audio_backend.default_input().await?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        let rear = CameraCapture::open(1)?;
        broadcast.video().replace(rear)?;

        broadcast.video().set_enabled(false);
        broadcast.video().set_enabled(true);
        broadcast.audio().set_muted(true);
        broadcast.audio().set_muted(false);

        Ok(())
    }
}

/// Room relay — forward all participants to external audience.
mod ex_room_relay {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn room_to_stream(live: Live, ticket: RoomTicket) -> Result<()> {
        let room = live.join_room(ticket).await?;
        let mut relays: HashMap<ParticipantId, LocalBroadcast> = HashMap::new();

        let mut events = room.events();
        while let Some(event) = events.next().await {
            match event {
                RoomEvent::ParticipantJoined(p) => {
                    if let Some(cam) = p.broadcast(TrackName::Camera) {
                        let relay = LocalBroadcast::relay(cam)?;
                        let name = format!("room-{}-{}", room.id(), p.id());
                        live.publish(&name, &relay);
                        relays.insert(p.id(), relay);
                    }
                }
                RoomEvent::ParticipantLeft { participant, .. } => {
                    relays.remove(&participant);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Room via relay — SFU mode, media routes through relay server.
mod ex_room_via_relay {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn room_via_relay(live: Live, ticket: RoomTicket) -> Result<()> {
        let room = live
            .join_room_with(
                ticket,
                RoomOptions::default()
                    .relay(RelayConfig::new("https://relay.example.com").token("room-jwt-token")),
            )
            .await?;

        let audio_backend = AudioBackend::new();
        let camera = CameraCapture::open(0)?;
        room.local().broadcast(TrackName::Camera).video().set(
            camera,
            VideoCodec::H264,
            [VideoPreset::P720],
        )?;
        let mic = audio_backend.default_input().await?;
        room.local().broadcast(TrackName::Camera).audio().set(
            mic,
            AudioCodec::Opus,
            [AudioPreset::Hq],
        )?;

        let mut events = room.events();
        while let Some(event) = events.next().await {
            if let RoomEvent::ParticipantJoined(p) = event {
                if let Some(cam) = p.broadcast(TrackName::Camera) {
                    let mut _video = cam.video()?;
                    let _audio = cam.audio(&audio_backend).await?;
                }
            }
        }
        Ok(())
    }
}

/// Audio-only streaming — no video boilerplate needed.
mod ex_audio_only {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn studio_publish(live: Live) -> Result<()> {
        let audio_backend = AudioBackend::new();
        let source = audio_backend.default_input().await?;

        let broadcast = LocalBroadcast::new();
        broadcast
            .audio()
            .set(source, AudioCodec::Opus, [AudioPreset::Hq])?;

        live.publish("monitor-mix", &broadcast);
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }

    async fn musician_subscribe(live: Live, remote: EndpointAddr) -> Result<()> {
        let sub = live.subscribe(remote, "monitor-mix").await?;

        let audio_backend = AudioBackend::new();
        let track = sub.audio(&audio_backend).await?;
        track.stopped().await;
        Ok(())
    }
}

/// Error handling patterns.
mod ex_error_handling {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn robust_subscribe(live: Live, remote: EndpointAddr) -> Result<()> {
        let sub = match live.subscribe(remote, "stream").await {
            Ok(sub) => sub,
            Err(SubscribeError::NotFound) => {
                eprintln!("Broadcast not available");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        if sub.has_video() {
            match sub.video() {
                Ok(mut video) => {
                    let _ = video.current_frame();
                }
                Err(SubscribeError::DecoderFailed(e)) => {
                    eprintln!("No decoder for this codec: {e}");
                }
                Err(e) => return Err(e.into()),
            }
        }

        if sub.has_audio() {
            let audio_backend = AudioBackend::new();
            let _track = sub.audio(&audio_backend).await?;
        }

        sub.closed().await;
        Ok(())
    }
}

/// Publish to a moq-relay for fan-out, subscribe from relay.
mod ex_relay_announce {
    use super::iroh_live::*;
    use super::iroh_moq::*;
    use super::moq_media::*;
    use super::*;

    /// Publisher: announce to relay, relay pulls on-demand.
    async fn publish_to_relay() -> Result<()> {
        let endpoint = Endpoint::builder()
            .secret_key(SecretKey::generate())
            .bind()
            .await?;
        let live = Live::builder(endpoint).spawn_with_router();

        let camera = CameraCapture::open(0)?;
        let audio_backend = AudioBackend::new();
        let mic = audio_backend.default_input().await?;

        let broadcast = LocalBroadcast::new();
        broadcast.video().set(
            camera,
            VideoCodec::H264,
            [VideoPreset::P720, VideoPreset::P360],
        )?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

        let relay_session = live
            .transport()
            .connect_url("https://relay.example.com?jwt=publisher-token")
            .await?;
        relay_session.publish("concert", broadcast.consume());

        relay_session.closed().await?;
        Ok(())
    }

    /// Subscriber: connect to relay, subscribe to "concert".
    async fn subscribe_from_relay() -> Result<()> {
        let endpoint = Endpoint::bind().await?;
        let live = Live::builder(endpoint).spawn_with_router();

        let relay_session = live
            .transport()
            .connect_url("https://relay.example.com?jwt=subscriber-token")
            .await?;
        let sub = RemoteBroadcast::new(relay_session.clone(), "concert").await?;

        let mut video = sub.video()?;
        let audio_backend = AudioBackend::new();
        let _audio = sub.audio(&audio_backend).await?;

        loop {
            if let Some(_frame) = video.current_frame() { /* render */ }
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
    }

    /// Hybrid: direct P2P + relay simultaneously, same broadcast.
    async fn publish_hybrid(live: Live) -> Result<()> {
        let broadcast = LocalBroadcast::new();
        let camera = CameraCapture::open(0)?;
        broadcast
            .video()
            .set(camera, VideoCodec::H264, [VideoPreset::P720])?;

        // Direct P2P
        live.publish("concert", &broadcast);

        // Also fan-out via relay — same producer, data encoded once
        let relay_session = live
            .transport()
            .connect_url("https://relay.example.com?jwt=token")
            .await?;
        relay_session.publish("concert", broadcast.consume());

        relay_session.closed().await?;
        Ok(())
    }

    /// Convenience: use Live relay helpers.
    async fn relay_convenience(live: Live) -> Result<()> {
        let broadcast = LocalBroadcast::new();
        let camera = CameraCapture::open(0)?;
        broadcast
            .video()
            .set(camera, VideoCodec::H264, [VideoPreset::P720])?;

        let _session = live
            .publish_to_relay(
                RelayConfig::new("https://relay.example.com").token("pub-token"),
                "concert",
                &broadcast,
            )
            .await?;

        Ok(())
    }
}

/// Recording pipeline using frames() stream.
mod ex_recording {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn record(live: Live, addr: EndpointAddr) -> Result<()> {
        let sub = live.subscribe(addr, "stream-to-record").await?;
        let mut video = sub.video_with(VideoOptions::default().quality(Quality::High))?;

        let mut frames = video.frames();
        while let Some(_frame) = frames.next().await {
            // writer.write_frame(&frame)?;
        }
        Ok(())
    }
}

/// Reactive UI with watchers.
mod ex_reactive_ui {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn participant_grid(room: Room) -> Result<()> {
        let mut watcher = room.remote_participants();
        loop {
            let participants = watcher.get();
            // update_grid_layout(&participants);
            let _ = participants;
            watcher.updated().await;
        }
    }

    async fn status_monitor(sub: &RemoteBroadcast) {
        let mut status = sub.status();
        loop {
            match status.get() {
                BroadcastStatus::Connecting => { /* show spinner */ }
                BroadcastStatus::Live => { /* show live indicator */ }
                BroadcastStatus::Ended => {
                    /* show ended */
                    break;
                }
            }
            status.updated().await;
        }
    }
}

/// Edge relay: subscribe from CDN relay, re-publish locally.
mod ex_edge_relay {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn edge_relay(live: Live) -> Result<()> {
        let sub = live
            .subscribe_from_relay(
                RelayConfig::new("https://cdn.example.com").token("edge-token"),
                "live-event",
            )
            .await?;

        let local_relay = LocalBroadcast::relay(&sub)?;
        live.publish("local-live-event", &local_relay);

        sub.closed().await;
        Ok(())
    }
}

/// Cascading relay chain: source -> relay A -> relay B.
mod ex_cascading_relay {
    use super::iroh_live::*;
    use super::moq_media::*;
    use super::*;

    async fn cascade(live: Live) -> Result<()> {
        let sub = live
            .subscribe_from_relay(
                RelayConfig::new("https://origin.example.com").token("source-token"),
                "stream",
            )
            .await?;

        let relay_broadcast = LocalBroadcast::relay(&sub)?;
        let _edge_session = live
            .publish_to_relay(
                RelayConfig::new("https://edge.example.com").token("edge-token"),
                "stream",
                &relay_broadcast,
            )
            .await?;

        sub.closed().await;
        Ok(())
    }
}

/// Compose Live with other iroh protocols on the same Router.
mod ex_compose_router {
    use super::iroh_live::*;
    use super::iroh_moq;
    use super::moq_media::*;
    use super::*;

    const MY_PROTOCOL_ALPN: &[u8] = b"my-app/1";

    struct MyProtocolHandler;
    impl ProtocolHandler for MyProtocolHandler {}

    async fn compose() -> Result<()> {
        let endpoint = Endpoint::builder()
            .secret_key(SecretKey::generate())
            .bind()
            .await?;

        // Create Live without a Router
        let live = Live::builder(endpoint.clone()).enable_gossip().spawn();

        // Mount Live + other protocols on a shared Router
        let mut router = Router::builder(endpoint);
        router = live.register_protocols(router);
        let _router = router.accept(MY_PROTOCOL_ALPN, MyProtocolHandler).spawn();

        // Live works normally
        let broadcast = LocalBroadcast::new();
        let camera = CameraCapture::open(0)?;
        broadcast
            .video()
            .set(camera, VideoCodec::H264, [VideoPreset::P720])?;
        live.publish("my-stream", &broadcast);

        Ok(())
    }

    /// Alternative: manage gossip externally (e.g. shared with other subsystems).
    async fn compose_external_gossip() -> Result<()> {
        let endpoint = Endpoint::builder().bind().await?;

        // External gossip — passed via builder, NOT auto-mounted
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let live = Live::builder(endpoint.clone())
            .gossip(gossip.clone())
            .spawn();

        // You mount both gossip and live yourself
        let _router = Router::builder(endpoint)
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_live::ALPN, live.protocol_handler())
            .spawn();

        Ok(())
    }
}

// ── main ─────────────────────────────────────────────────────────────────────

fn main() {
    println!("This is an API sketch -- read the source, don't run it.");
    println!("Modules: moq_lite, iroh_moq, moq_media, iroh_live + examples");
}
