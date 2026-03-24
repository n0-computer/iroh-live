//! Subscribe side: receiving and decoding remote broadcasts.
//!
//! [`RemoteBroadcast`] wraps a catalog consumer and provides
//! [`VideoTrack`] and [`AudioTrack`] handles for decoded media.
//! [`AdaptiveVideoTrack`](crate::adaptive::AdaptiveVideoTrack) adds
//! automatic rendition switching based on network conditions.

use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};

use hang::{
    catalog::{AudioConfig, Catalog, CatalogConsumer, VideoConfig},
    container::OrderedConsumer,
};
use moq_lite::{BroadcastConsumer, Track};
use n0_error::{Result, StackResultExt, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::sync::watch;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Instrument, debug, warn};

use crate::{
    adaptive::{AdaptiveConfig, AdaptiveVideoTrack},
    format::{DecodeConfig, PlaybackConfig, Quality, VideoFrame},
    net::NetworkSignals,
    pipeline::{
        AudioDecoderPipeline, AudioPosition, DecodeOpts, VideoDecoderHandle, VideoDecoderPipeline,
    },
    playout::{FreshnessPolicy, PlaybackPolicy},
    processing::scale::Scaler,
    traits::{
        AudioDecoder, AudioSinkHandle, AudioStreamFactory, Decoders, VideoDecoder, VideoSource,
    },
    transport::MoqPacketSource,
    util::spawn_thread,
};

const VIDEO_PRIORITY: u8 = 1u8;
const AUDIO_PRIORITY: u8 = 2u8;

// ── Subscription options ────────────────────────────────────────────────

/// Viewport-aware rendition selection target.
///
/// Subscribers describe what they need rather than naming specific
/// renditions. The catalog selects the best match. If `rendition` is set,
/// it takes priority over pixel/bitrate constraints.
#[derive(Debug, Clone, Default)]
pub struct VideoTarget {
    /// Maximum pixel count (width * height). Renditions above this are skipped.
    pub max_pixels: Option<u32>,
    /// Maximum bitrate in kbps. Renditions above this are skipped.
    pub max_bitrate_kbps: Option<u32>,
    /// Pin to a specific rendition by name, bypassing automatic selection.
    pub rendition: Option<String>,
}

impl VideoTarget {
    /// Limits the maximum pixel count (width × height) for rendition selection.
    pub fn max_pixels(mut self, pixels: u32) -> Self {
        self.max_pixels = Some(pixels);
        self
    }
    /// Limits the maximum bitrate in kilobits per second for rendition selection.
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

impl From<Quality> for VideoTarget {
    fn from(q: Quality) -> Self {
        match q {
            Quality::Highest => Self::default(),
            Quality::High => Self::default().max_pixels(1280 * 720),
            Quality::Mid => Self::default().max_pixels(640 * 480),
            Quality::Low => Self::default().max_pixels(320 * 240),
        }
    }
}

/// Options for video subscription and decoding.
#[derive(Debug, Clone, Default)]
pub struct VideoOptions {
    /// Decoder configuration (backend, pixel format).
    pub playback: Option<DecodeConfig>,
    /// Rendition selection target (quality, resolution, bitrate).
    pub target: Option<VideoTarget>,
    /// Viewport dimensions `(width, height)` for resolution-aware decoding.
    pub viewport: Option<(u32, u32)>,
}

impl VideoOptions {
    /// Sets the rendition selection target.
    pub fn target(mut self, target: impl Into<VideoTarget>) -> Self {
        self.target = Some(target.into());
        self
    }
    /// Sets the desired quality level for rendition selection.
    pub fn quality(mut self, quality: Quality) -> Self {
        self.target = Some(quality.into());
        self
    }
    /// Sets the viewport dimensions for resolution-aware decoding.
    pub fn viewport(mut self, w: u32, h: u32) -> Self {
        self.viewport = Some((w, h));
        self
    }
    /// Sets the decoder configuration (backend, etc.).
    pub fn playback(mut self, config: DecodeConfig) -> Self {
        self.playback = Some(config);
        self
    }

    fn decode_config(&self) -> DecodeConfig {
        self.playback.clone().unwrap_or_default()
    }

    fn resolve_quality(&self) -> Quality {
        // If a specific rendition is pinned, we'll use video_rendition() directly.
        // Otherwise map VideoTarget to Quality for the existing selection logic.
        match &self.target {
            Some(t) if t.max_pixels.is_some() => {
                let px = t.max_pixels.unwrap();
                if px <= 320 * 240 {
                    Quality::Low
                } else if px <= 640 * 480 {
                    Quality::Mid
                } else if px <= 1280 * 720 {
                    Quality::High
                } else {
                    Quality::Highest
                }
            }
            _ => Quality::Highest,
        }
    }
}

/// Options for audio subscription.
#[derive(Debug, Clone, Default)]
pub struct AudioOptions {
    /// Pin to a specific audio rendition by name.
    pub rendition: Option<String>,
}

impl AudioOptions {
    /// Pins to a specific audio rendition by name.
    pub fn rendition(mut self, name: impl Into<String>) -> Self {
        self.rendition = Some(name.into());
        self
    }
}

// ── Error types ─────────────────────────────────────────────────────────

/// Errors from subscription operations.
#[derive(Debug)]
pub enum SubscribeError {
    /// The requested broadcast was not found.
    NotFound,
    /// No catalog was received from the broadcast.
    NoCatalog,
    /// The requested rendition does not exist.
    RenditionNotFound(String),
    /// The decoder failed to initialize or process media.
    DecoderFailed(anyhow::Error),
    /// The broadcast ended.
    Ended,
}

impl std::fmt::Display for SubscribeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "broadcast not found"),
            Self::NoCatalog => write!(f, "no catalog received"),
            Self::RenditionNotFound(name) => write!(f, "rendition not found: {name}"),
            Self::DecoderFailed(err) => write!(f, "decoder failed: {err}"),
            Self::Ended => write!(f, "broadcast ended"),
        }
    }
}

impl std::error::Error for SubscribeError {}

impl From<anyhow::Error> for SubscribeError {
    fn from(err: anyhow::Error) -> Self {
        Self::DecoderFailed(err)
    }
}

/// Lifecycle state of a remote broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastStatus {
    /// Actively receiving media.
    Live,
    /// Producer closed the broadcast.
    Ended,
}

/// Subscribes to a remote broadcast and provides access to its media tracks.
///
/// Wraps a [`BroadcastConsumer`] and watches the catalog for available
/// video and audio renditions. Create individual [`VideoTrack`] or
/// [`AudioTrack`] handles to start decoding.
#[derive(derive_more::Debug, Clone)]
pub struct RemoteBroadcast {
    broadcast_name: String,
    #[debug("BroadcastConsumer")]
    broadcast: BroadcastConsumer,
    catalog_watchable: Watchable<CatalogSnapshot>,
    playback_policy: PlaybackPolicy,
    shutdown: CancellationToken,
    _catalog_task: Arc<AbortOnDropHandle<()>>,
    stats: crate::stats::SubscribeStats,
    audio_position: AudioPosition,
    video_started: Arc<AtomicBool>,
    /// Shared freshness threshold used by new and existing tracks.
    max_stale_duration_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Skip generation counter: video increments on skip, audio flushes to resync.
    skip_generation: Arc<std::sync::atomic::AtomicU64>,
}

/// Point-in-time snapshot of a broadcast's catalog.
///
/// Derefs to [`Catalog`] for direct access to video/audio configuration.
/// Each snapshot carries a sequence number for change detection.
#[derive(Debug, derive_more::PartialEq, derive_more::Eq, Default, Clone, derive_more::Deref)]
pub struct CatalogSnapshot {
    #[eq(skip)]
    #[deref]
    inner: Arc<Catalog>,
    seq: usize,
}

impl CatalogSnapshot {
    fn new(inner: Catalog, seq: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            seq,
        }
    }

    /// Returns an iterator over video rendition names, sorted by width (ascending).
    pub fn video_renditions(&self) -> impl Iterator<Item = &str> {
        let mut renditions: Vec<_> = self
            .inner
            .video
            .renditions
            .iter()
            .map(|(name, config)| (name.as_str(), config.coded_width))
            .collect();
        renditions.sort_by(|a, b| a.1.cmp(&b.1));
        renditions.into_iter().map(|(name, _w)| name)
    }

    /// Returns an iterator over audio rendition names.
    pub fn audio_renditions(&self) -> impl Iterator<Item = &str> + '_ {
        self.inner.audio.renditions.keys().map(|name| name.as_str())
    }

    /// Selects the best video rendition for the given quality level.
    pub fn select_video_rendition(&self, quality: Quality) -> Result<String> {
        let video = &self.inner.video;
        let track_name =
            select_video_rendition(&video.renditions, quality).context("no video renditions")?;
        Ok(track_name)
    }

    /// Selects the best audio rendition for the given quality level.
    pub fn select_audio_rendition(&self, quality: Quality) -> Result<String> {
        let audio = &self.inner.audio;
        let track_name =
            select_audio_rendition(&audio.renditions, quality).context("no audio renditions")?;
        Ok(track_name)
    }

    /// Consumes the snapshot and returns the inner [`Catalog`].
    pub fn into_inner(self) -> Arc<Catalog> {
        self.inner
    }
}

impl RemoteBroadcast {
    /// Creates a new remote broadcast subscription.
    ///
    /// Waits for the initial catalog before returning. Spawns a background
    /// task that watches for catalog updates.
    ///
    /// This uses the default interactive playback policy:
    /// [`PlaybackPolicy::audio_master()`]. Use
    /// [`RemoteBroadcast::with_playback_policy`] when you need unmanaged
    /// playout or a different freshness threshold.
    pub async fn new(broadcast_name: impl ToString, broadcast: BroadcastConsumer) -> Result<Self> {
        Self::with_playback_policy(broadcast_name, broadcast, PlaybackPolicy::default()).await
    }

    /// Creates a new remote broadcast subscription with an explicit playback
    /// policy.
    ///
    /// Choose the playback policy when you create the subscription. In normal
    /// applications, this is where you decide whether you want audio-master
    /// sync or unmanaged playout, and how aggressively you want to skip stale
    /// media after a stall.
    #[tracing::instrument("RemoteBroadcast", skip_all, fields(name=tracing::field::Empty))]
    pub async fn with_playback_policy(
        broadcast_name: impl ToString,
        broadcast: BroadcastConsumer,
        playback_policy: PlaybackPolicy,
    ) -> Result<Self> {
        let max_stale_duration_ms = playback_policy.freshness.max_stale_duration.as_millis() as u64;
        let broadcast_name = broadcast_name.to_string();
        tracing::Span::current().record("name", tracing::field::display(&broadcast_name));
        let shutdown = CancellationToken::new();

        let (catalog_watchable, catalog_task) = {
            let track = broadcast
                .subscribe_track(&Catalog::default_track())
                .std_context("missing catalog track")?;
            debug!("catalog received");
            let mut catalog_consumer = CatalogConsumer::new(track);
            let initial_catalog = catalog_consumer
                .next()
                .await
                .std_context("Broadcast closed before receiving catalog")?
                .context("Catalog track closed before receiving catalog")?;
            let watchable = Watchable::new(CatalogSnapshot::new(initial_catalog, 0));

            let task = tokio::spawn({
                let shutdown = shutdown.clone();
                let watchable = watchable.clone();
                async move {
                    for seq in 1.. {
                        match catalog_consumer.next().await {
                            Ok(Some(catalog)) => {
                                watchable.set(CatalogSnapshot::new(catalog, seq)).ok();
                            }
                            Ok(None) => {
                                debug!("subscribed broadcast catalog track ended");
                                break;
                            }
                            Err(err) => {
                                debug!("subscribed broadcast closed: {err:#}");
                                break;
                            }
                        }
                    }
                    shutdown.cancel();
                }
                .instrument(tracing::Span::current())
            });
            (watchable, task)
        };
        Ok(Self {
            broadcast_name,
            broadcast,
            catalog_watchable,
            playback_policy,
            _catalog_task: Arc::new(AbortOnDropHandle::new(catalog_task)),
            shutdown,
            stats: crate::stats::SubscribeStats::default(),
            audio_position: AudioPosition::default(),
            video_started: Arc::new(AtomicBool::new(false)),
            max_stale_duration_ms: Arc::new(std::sync::atomic::AtomicU64::new(
                max_stale_duration_ms,
            )),
            skip_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Returns the name of this broadcast.
    pub fn broadcast_name(&self) -> &str {
        &self.broadcast_name
    }

    /// Returns a [`DecodeOpts`] value wired to this
    /// broadcast's timing state, stats, and skip coordination.
    fn decode_opts(&self) -> DecodeOpts {
        DecodeOpts {
            stats: self.stats.decode_stats(),
            audio_position: self.audio_position.clone(),
            video_started: self.video_started.clone(),
            playback_policy: self.playback_policy.clone(),
            max_stale_duration_ms: self.max_stale_duration_ms.clone(),
            skip_generation: self.skip_generation.clone(),
        }
    }

    /// Returns a watcher for the catalog (renditions added/removed).
    pub fn catalog_watcher(&self) -> n0_watcher::Direct<CatalogSnapshot> {
        self.catalog_watchable.watch()
    }

    /// Returns the current catalog snapshot.
    pub fn catalog(&self) -> CatalogSnapshot {
        self.catalog_watchable.get()
    }

    /// Returns true if the catalog has video renditions.
    pub fn has_video(&self) -> bool {
        !self.catalog().video.renditions.is_empty()
    }

    /// Returns true if the catalog has audio renditions.
    pub fn has_audio(&self) -> bool {
        !self.catalog().audio.renditions.is_empty()
    }

    /// Returns the subscribe-side stats. Decode and playout pipelines
    /// record into these automatically. External producers (e.g. iroh
    /// transport stats) can record additional metrics into the net stats.
    pub fn stats(&self) -> &crate::stats::SubscribeStats {
        &self.stats
    }

    /// Returns the current playback policy for this broadcast.
    ///
    /// We return an owned value because freshness may be adjusted at runtime.
    /// This is useful when UI code wants to show the current tuning state or
    /// clone the policy for a new subscription.
    pub fn playback_policy(&self) -> PlaybackPolicy {
        self.playback_policy
            .clone()
            .with_freshness(FreshnessPolicy::new(Duration::from_millis(
                self.max_stale_duration_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            )))
    }

    /// Resets sync state for a fresh pipeline start. Call before
    /// rebuilding video/audio tracks (decoder switch, resubscribe)
    /// so the new pipelines don't inherit stale timing from the
    /// old session.
    pub fn reset_sync(&self) {
        self.audio_position.reset();
        self.video_started
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Sets the maximum stale duration used for transport freshness and
    /// decoder skip recovery.
    ///
    /// This is the one playback knob that we support changing on a live
    /// subscription. Increase it when you want to preserve more continuity
    /// through congestion. Decrease it when you want faster recovery back to
    /// the live edge after a stall.
    ///
    /// We keep sync mode fixed for the lifetime of the subscription. Changing
    /// sync mode rebuilds the mental model of playout, while freshness is a
    /// straightforward transport/recovery tradeoff that is safe to retune
    /// live.
    pub fn set_max_stale_duration(&self, duration: Duration) {
        self.max_stale_duration_ms.store(
            duration.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    // -- Generic subscription methods (for custom decoders) --

    /// Subscribes to both video and audio with a custom decoder type.
    pub async fn media<D: Decoders>(
        &self,
        audio_backend: &dyn AudioStreamFactory,
        playback_config: PlaybackConfig,
    ) -> Result<MediaTracks> {
        MediaTracks::new::<D>(self.clone(), audio_backend, playback_config).await
    }

    /// Subscribes to video with explicit config and a custom decoder.
    pub fn video_with_decoder<D: VideoDecoder>(
        &self,
        playback_config: &DecodeConfig,
        quality: Quality,
    ) -> Result<VideoTrack> {
        let track_name = self.catalog().select_video_rendition(quality)?;
        self.video_rendition::<D>(playback_config, &track_name)
    }

    /// Subscribes to a specific video rendition with a custom decoder.
    pub fn video_rendition<D: VideoDecoder>(
        &self,
        playback_config: &DecodeConfig,
        track_name: &str,
    ) -> Result<VideoTrack> {
        let max_latency = Duration::from_millis(
            self.max_stale_duration_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let catalog = self.catalog();
        let video = &catalog.video;
        let config = video
            .renditions
            .get(track_name)
            .context("rendition not found")?;
        let consumer = OrderedConsumer::new(
            self.broadcast
                .subscribe_track(&Track {
                    name: track_name.to_string(),
                    priority: VIDEO_PRIORITY,
                })
                .anyerr()?,
            max_latency,
        );
        VideoTrack::from_consumer::<D>(
            track_name.to_string(),
            consumer,
            config,
            playback_config,
            self.decode_opts(),
        )
    }

    /// Subscribes to audio with explicit quality and a custom decoder.
    pub async fn audio_with_decoder<D: AudioDecoder>(
        &self,
        quality: Quality,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack> {
        let track_name = self.catalog().select_audio_rendition(quality)?;
        self.audio_rendition::<D>(&track_name, audio_backend).await
    }

    /// Subscribes to a specific audio rendition with a custom decoder.
    pub async fn audio_rendition<D: AudioDecoder>(
        &self,
        name: &str,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack> {
        let max_latency = Duration::from_millis(
            self.max_stale_duration_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let catalog = self.catalog();
        let audio = &catalog.audio;
        let config = audio.renditions.get(name).context("rendition not found")?;
        let consumer = OrderedConsumer::new(
            self.broadcast
                .subscribe_track(&Track {
                    name: name.to_string(),
                    priority: AUDIO_PRIORITY,
                })
                .anyerr()?,
            max_latency,
        );
        AudioTrack::spawn::<D>(
            name.to_string(),
            consumer,
            config.clone(),
            audio_backend,
            self.decode_opts(),
        )
        .await
    }

    // -- Non-generic convenience methods (dynamic decoder dispatch) ──────

    /// Subscribes to the best-quality video rendition.
    ///
    /// Uses dynamic decoder dispatch based on the codec in the catalog.
    /// For explicit decoder selection, use [`video_with_decoder`](Self::video_with_decoder).
    #[cfg(any_video_codec)]
    pub fn video(&self) -> Result<VideoTrack> {
        self.video_with(Default::default())
    }

    /// Subscribes to the best-quality audio rendition.
    ///
    /// Uses dynamic decoder dispatch based on the codec in the catalog.
    /// For explicit decoder selection, use [`audio_with_decoder`](Self::audio_with_decoder).
    #[cfg(any_audio_codec)]
    pub async fn audio(&self, audio_backend: &dyn AudioStreamFactory) -> Result<AudioTrack> {
        self.audio_with(Default::default(), audio_backend).await
    }

    /// Subscribes to video with options (non-generic, uses dynamic decoder dispatch).
    #[cfg(any_video_codec)]
    pub fn video_with(&self, opts: VideoOptions) -> Result<VideoTrack> {
        use crate::codec::DynamicVideoDecoder;
        let decode_config = opts.decode_config();
        if let Some(rendition) = opts.target.as_ref().and_then(|t| t.rendition.as_ref()) {
            return self.video_rendition::<DynamicVideoDecoder>(&decode_config, rendition);
        }
        let quality = opts.resolve_quality();
        self.video_with_decoder::<DynamicVideoDecoder>(&decode_config, quality)
    }

    /// Subscribes to audio with options (non-generic, uses dynamic decoder dispatch).
    #[cfg(any_audio_codec)]
    pub async fn audio_with(
        &self,
        opts: AudioOptions,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack> {
        use crate::codec::DynamicAudioDecoder;
        if let Some(ref rendition) = opts.rendition {
            self.audio_rendition::<DynamicAudioDecoder>(rendition, audio_backend)
                .await
        } else {
            self.audio_with_decoder::<DynamicAudioDecoder>(Quality::Highest, audio_backend)
                .await
        }
    }

    /// Waits until the broadcast closes.
    pub fn closed(&self) -> impl Future<Output = moq_lite::Error> + 'static {
        let broadcast = self.broadcast.clone();
        async move { broadcast.closed().await }
    }

    /// Returns the shutdown token for this broadcast.
    ///
    /// Useful for tying the lifetime of auxiliary tasks (e.g. signal
    /// producers) to this broadcast subscription.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Subscribes to an adaptive video track that automatically switches
    /// renditions based on network conditions.
    ///
    /// Uses dynamic decoder dispatch and default [`AdaptiveConfig`].
    /// Pass a [`NetworkSignals`] receiver produced by a signal poller
    /// (e.g. from polling QUIC connection stats).
    #[cfg(any_video_codec)]
    pub fn adaptive_video(
        &self,
        signals: watch::Receiver<NetworkSignals>,
    ) -> anyhow::Result<AdaptiveVideoTrack> {
        AdaptiveVideoTrack::new(
            self.clone(),
            signals,
            AdaptiveConfig::default(),
            DecodeConfig::default(),
        )
    }

    /// Subscribes to an adaptive video track with custom configuration.
    #[cfg(any_video_codec)]
    pub fn adaptive_video_with(
        &self,
        signals: watch::Receiver<NetworkSignals>,
        config: AdaptiveConfig,
        decode_config: DecodeConfig,
    ) -> anyhow::Result<AdaptiveVideoTrack> {
        AdaptiveVideoTrack::new(self.clone(), signals, config, decode_config)
    }

    /// Waits until the catalog contains at least one video or audio rendition.
    pub async fn ready(&self) {
        let mut watcher = self.catalog_watcher();
        loop {
            if self.has_video() || self.has_audio() {
                return;
            }
            if watcher.updated().await.is_err() {
                return;
            }
        }
    }

    /// Waits for video renditions to appear, then subscribes to the best quality.
    ///
    /// Async counterpart of [`video`](Self::video): blocks until the catalog
    /// advertises at least one video rendition, then behaves identically.
    #[cfg(any_video_codec)]
    pub async fn video_ready(&self) -> Result<VideoTrack> {
        self.wait_for_video().await;
        self.video()
    }

    /// Waits for audio renditions to appear, then subscribes to the best quality.
    ///
    /// Async counterpart of [`audio`](Self::audio).
    #[cfg(any_audio_codec)]
    pub async fn audio_ready(&self, audio_backend: &dyn AudioStreamFactory) -> Result<AudioTrack> {
        self.wait_for_audio().await;
        self.audio(audio_backend).await
    }

    async fn wait_for_video(&self) {
        let mut watcher = self.catalog_watcher();
        loop {
            if self.has_video() {
                return;
            }
            if watcher.updated().await.is_err() {
                return;
            }
        }
    }

    async fn wait_for_audio(&self) {
        let mut watcher = self.catalog_watcher();
        loop {
            if self.has_audio() {
                return;
            }
            if watcher.updated().await.is_err() {
                return;
            }
        }
    }

    /// Shuts down this remote broadcast subscription.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

fn select_rendition<T, P: ToString>(
    renditions: &BTreeMap<String, T>,
    order: &[P],
) -> Option<String> {
    order
        .iter()
        .map(ToString::to_string)
        .find(|k| renditions.contains_key(k.as_str()))
        .or_else(|| renditions.keys().next().cloned())
}

fn select_video_rendition<T>(renditions: &BTreeMap<String, T>, q: Quality) -> Option<String> {
    use crate::format::VideoPreset::*;
    let order = match q {
        Quality::Highest => [P1080, P720, P360, P180],
        Quality::High => [P720, P360, P180, P1080],
        Quality::Mid => [P360, P180, P720, P1080],
        Quality::Low => [P180, P360, P720, P1080],
    };

    select_rendition(renditions, &order)
}

fn select_audio_rendition<T>(renditions: &BTreeMap<String, T>, q: Quality) -> Option<String> {
    use crate::format::AudioPreset::*;
    let order = match q {
        Quality::Highest | Quality::High => [Hq, Lq],
        Quality::Mid | Quality::Low => [Lq, Hq],
    };
    select_rendition(renditions, &order)
}

/// Decoded audio track from a remote broadcast.
///
/// Wraps an [`AudioDecoderPipeline`] that decodes incoming audio packets
/// and routes them to the audio output backend.
#[derive(derive_more::Debug)]
pub struct AudioTrack {
    #[debug(skip)]
    pipeline: AudioDecoderPipeline,
}

impl AudioTrack {
    pub(crate) async fn spawn<D: AudioDecoder>(
        name: String,
        consumer: OrderedConsumer,
        config: AudioConfig,
        audio_backend: &dyn AudioStreamFactory,
        opts: DecodeOpts,
    ) -> Result<Self> {
        let source = MoqPacketSource(consumer);
        let config: rusty_codecs::config::AudioConfig = config.into();
        let pipeline =
            AudioDecoderPipeline::new::<D>(name, source, &config, audio_backend, opts).await?;
        Ok(Self { pipeline })
    }

    /// Returns a future that completes when the audio pipeline stops.
    pub fn stopped(&self) -> impl Future<Output = ()> + 'static {
        self.pipeline.stopped()
    }

    /// Returns the rendition name for this audio track.
    pub fn rendition(&self) -> &str {
        self.pipeline.name()
    }

    /// Returns a handle to the audio sink for playback control.
    pub fn handle(&self) -> &dyn AudioSinkHandle {
        self.pipeline.handle()
    }

    /// Returns `true` if the decoder pipeline has already stopped.
    pub fn is_stopped(&self) -> bool {
        self.pipeline.is_stopped()
    }
}

/// Decoded video track from a remote broadcast.
///
/// Produces [`VideoFrame`]s via [`current_frame`](Self::current_frame) (non-blocking)
/// or [`next_frame`](Self::next_frame) (async). Can also wrap a raw [`VideoSource`]
/// for local preview.
#[derive(derive_more::Debug)]
pub struct VideoTrack {
    #[debug(skip)]
    rx: crate::frame_channel::FrameReceiver<VideoFrame>,
    inner: VideoTrackInner,
}

#[derive(derive_more::Debug)]
enum VideoTrackInner {
    /// Wraps a [`VideoDecoderPipeline`] (from `from_consumer` / `from_pipeline`).
    Pipeline(VideoDecoderHandle),
    /// Raw video source capture (from `from_video_source`).
    #[debug("VideoSource")]
    VideoSource {
        rendition: String,
        viewport: Watchable<(u32, u32)>,
        _shutdown_guard: DropGuard,
        _thread: thread::JoinHandle<()>,
    },
}

impl VideoTrack {
    /// Creates a track from a raw [`VideoSource`] (e.g. camera capture).
    pub fn from_video_source(
        rendition: String,
        shutdown: CancellationToken,
        mut source: impl VideoSource,
    ) -> Self {
        let viewport = Watchable::new((1u32, 1u32));
        let (frame_tx, frame_rx) = crate::frame_channel::frame_channel();
        let thread_name = format!("vpr-{:>4}-{:>4}", source.name(), rendition);
        let thread = spawn_thread(thread_name, {
            let mut viewport = viewport.watch();
            let shutdown = shutdown.clone();
            move || {
                // TODO: Make configurable.
                let fps = 30;
                let mut scaler = Scaler::new(None);
                let frame_duration = Duration::from_secs_f32(1. / fps as f32);
                if let Err(err) = source.start() {
                    warn!("Video source failed to start: {err:?}");
                    return;
                }
                let start = Instant::now();
                for i in 1.. {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    if viewport.update() {
                        let (w, h) = viewport.peek();
                        scaler.set_target_dimensions(*w, *h);
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let [w, h] = frame.dimensions;
                            // Only convert to RGBA and scale if the viewport
                            // demands a different size. For passthrough (viewport
                            // 1×1 or matching source), forward the original frame
                            // to avoid a costly NV12→RGBA conversion.
                            let (vw, vh) = *viewport.peek();
                            let needs_scale = vw > 1 && vh > 1 && (vw != w || vh != h);
                            let decoded = if needs_scale {
                                let rgba = frame.rgba_image();
                                match scaler.scale_rgba(rgba.as_raw(), w, h) {
                                    Ok(Some((scaled, sw, sh))) => {
                                        let mut f = VideoFrame::new_rgba(
                                            scaled.into(),
                                            sw,
                                            sh,
                                            Duration::ZERO,
                                        );
                                        f.timestamp = start.elapsed();
                                        f
                                    }
                                    Ok(None) | Err(_) => {
                                        let mut f = frame;
                                        f.timestamp = start.elapsed();
                                        f
                                    }
                                }
                            } else {
                                let mut f = frame;
                                f.timestamp = start.elapsed();
                                f
                            };
                            frame_tx.send(decoded);
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                    let expected_time = i * frame_duration;
                    let actual_time = start.elapsed();
                    if expected_time > actual_time {
                        thread::sleep(expected_time - actual_time);
                    }
                }
                if let Err(err) = source.stop() {
                    warn!("Video source failed to stop: {err:?}");
                }
            }
        });
        Self {
            rx: frame_rx,
            inner: VideoTrackInner::VideoSource {
                rendition,
                viewport,
                _shutdown_guard: shutdown.drop_guard(),
                _thread: thread,
            },
        }
    }

    pub(crate) fn from_consumer<D: VideoDecoder>(
        rendition: String,
        consumer: OrderedConsumer,
        config: &VideoConfig,
        playback_config: &DecodeConfig,
        opts: DecodeOpts,
    ) -> Result<Self> {
        let source = MoqPacketSource(consumer);
        let config: rusty_codecs::config::VideoConfig = config.clone().into();
        let pipeline =
            VideoDecoderPipeline::new::<D>(rendition, source, &config, playback_config, opts)?;
        Ok(Self::from_pipeline(pipeline))
    }

    /// Creates a `VideoTrack` from a standalone [`VideoDecoderPipeline`].
    pub fn from_pipeline(pipeline: VideoDecoderPipeline) -> Self {
        let VideoDecoderPipeline { frames, handle } = pipeline;
        Self {
            rx: frames.rx,
            inner: VideoTrackInner::Pipeline(handle),
        }
    }

    /// Updates the viewport dimensions for resolution-aware scaling.
    pub fn set_viewport(&self, w: u32, h: u32) {
        match &self.inner {
            VideoTrackInner::Pipeline(handle) => handle.set_viewport(w, h),
            VideoTrackInner::VideoSource { viewport, .. } => {
                viewport.set((w, h)).ok();
            }
        }
    }

    /// Returns the rendition name for this video track.
    pub fn rendition(&self) -> &str {
        match &self.inner {
            VideoTrackInner::Pipeline(handle) => handle.rendition(),
            VideoTrackInner::VideoSource { rendition, .. } => rendition,
        }
    }

    /// Returns the name of the decoder backend in use.
    pub fn decoder_name(&self) -> &str {
        match &self.inner {
            VideoTrackInner::Pipeline(handle) => handle.decoder_name(),
            VideoTrackInner::VideoSource { .. } => "capture",
        }
    }

    /// Returns `true` if the track's frame producer has been dropped.
    pub fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }

    /// Returns the latest decoded frame, or `None` if no new frame
    /// has arrived since the last call.
    pub fn current_frame(&mut self) -> Option<VideoFrame> {
        self.rx.take()
    }

    /// Waits for the next frame. Returns `None` when the producer
    /// shuts down.
    pub async fn next_frame(&mut self) -> Option<VideoFrame> {
        self.rx.recv().await
    }
}

/// Combined video and audio tracks from a [`RemoteBroadcast`].
///
/// Convenience type that holds the broadcast alongside its decoded
/// media tracks and shared timing state.
#[derive(derive_more::Debug)]
pub struct MediaTracks {
    /// The underlying broadcast subscription.
    pub broadcast: RemoteBroadcast,
    /// The decoded video track, if the broadcast has video.
    pub video: Option<VideoTrack>,
    /// The decoded audio track, if the broadcast has audio.
    pub audio: Option<AudioTrack>,
}

impl MediaTracks {
    /// Creates media tracks by subscribing to both video and audio from the broadcast.
    pub async fn new<D: Decoders>(
        broadcast: RemoteBroadcast,
        audio_backend: &dyn AudioStreamFactory,
        playback_config: PlaybackConfig,
    ) -> Result<Self> {
        let audio_track_name = broadcast
            .catalog()
            .select_audio_rendition(playback_config.quality)
            .ok();
        let audio = match audio_track_name {
            Some(name) => broadcast
                .audio_rendition::<D::Audio>(&name, audio_backend)
                .await
                .inspect_err(|err| tracing::warn!("no audio track: {err}"))
                .ok(),
            None => None,
        };
        let track_name = broadcast
            .catalog()
            .select_video_rendition(playback_config.quality)
            .ok();
        let video = track_name.and_then(|name| {
            broadcast
                .video_rendition::<D::Video>(&playback_config.decode_config, &name)
                .inspect_err(|err| tracing::warn!("no video track: {err}"))
                .ok()
        });
        Ok(Self {
            broadcast,
            audio,
            video,
        })
    }
}
