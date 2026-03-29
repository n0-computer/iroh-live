//! Subscribe side: receiving and decoding remote broadcasts.
//!
//! [`RemoteBroadcast`] wraps a catalog consumer and provides
//! [`VideoTrack`] and [`AudioTrack`] handles for decoded media.
//! [`AdaptiveVideoTrack`](crate::adaptive::AdaptiveVideoTrack) adds
//! automatic rendition switching based on network conditions.

use std::{
    collections::BTreeMap,
    sync::Arc,
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
#[cfg(any_video_codec)]
use tokio::sync::watch;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Instrument, debug, warn};

#[cfg(any_video_codec)]
use crate::adaptive::{AdaptiveConfig, AdaptiveVideoTrack};
#[cfg(any_video_codec)]
use crate::net::NetworkSignals;
use crate::{
    format::{DecodeConfig, PlaybackConfig, Quality, VideoFrame},
    pipeline::{AudioDecoderPipeline, PipelineContext, VideoDecoderHandle, VideoDecoderPipeline},
    playout::{PlaybackPolicy, SyncMode},
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
    #[must_use]
    pub fn max_pixels(mut self, pixels: u32) -> Self {
        self.max_pixels = Some(pixels);
        self
    }
    /// Limits the maximum bitrate in kilobits per second for rendition selection.
    #[must_use]
    pub fn max_bitrate_kbps(mut self, kbps: u32) -> Self {
        self.max_bitrate_kbps = Some(kbps);
        self
    }
    /// Pins to a specific rendition by name, bypassing automatic selection.
    #[must_use]
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
#[non_exhaustive]
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
    #[must_use]
    pub fn target(mut self, target: impl Into<VideoTarget>) -> Self {
        self.target = Some(target.into());
        self
    }
    /// Sets the desired quality level for rendition selection.
    #[must_use]
    pub fn quality(mut self, quality: Quality) -> Self {
        self.target = Some(quality.into());
        self
    }
    /// Sets the viewport dimensions for resolution-aware decoding.
    #[must_use]
    pub fn viewport(mut self, w: u32, h: u32) -> Self {
        self.viewport = Some((w, h));
        self
    }
    /// Sets the decoder configuration (backend, etc.).
    #[must_use]
    pub fn playback(mut self, config: DecodeConfig) -> Self {
        self.playback = Some(config);
        self
    }

    #[cfg(any_video_codec)]
    fn decode_config(&self) -> DecodeConfig {
        self.playback.clone().unwrap_or_default()
    }

    #[cfg(any_video_codec)]
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
#[non_exhaustive]
pub struct AudioOptions {
    /// Pin to a specific audio rendition by name.
    pub rendition: Option<String>,
}

impl AudioOptions {
    /// Pins to a specific audio rendition by name.
    #[must_use]
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
    /// Shared playout clock for A/V synchronization. Created once per
    /// broadcast and passed to video decode pipelines when
    /// [`SyncMode::Synced`] is active.
    sync: crate::sync::Sync,
}

/// Point-in-time snapshot of a broadcast's catalog.
///
/// Derefs to [`Catalog`] for direct access to video/audio configuration.
/// Each snapshot carries a sequence number for change detection.
/// Equality compares only the sequence number, not the catalog content — two
/// snapshots from different broadcasts with the same `seq` compare as equal.
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
    /// Creates a new remote broadcast subscription with the default
    /// [`PlaybackPolicy`] (synced playout, 150 ms max latency).
    ///
    /// Waits for the initial catalog before returning. Spawns a background
    /// task that watches for catalog updates. Use
    /// [`with_playback_policy`](Self::with_playback_policy) when you need
    /// unmanaged playout or a different latency budget.
    pub async fn new(broadcast_name: impl ToString, broadcast: BroadcastConsumer) -> Result<Self> {
        Self::with_playback_policy(broadcast_name, broadcast, PlaybackPolicy::default()).await
    }

    /// Creates a new remote broadcast subscription with an explicit
    /// [`PlaybackPolicy`].
    ///
    /// The policy controls A/V sync mode and the max latency for the
    /// ordered consumer. You can change it later with
    /// [`set_playback_policy`](Self::set_playback_policy) before
    /// subscribing to new tracks.
    #[tracing::instrument("RemoteBroadcast", skip_all, fields(name=tracing::field::Empty))]
    pub async fn with_playback_policy(
        broadcast_name: impl ToString,
        broadcast: BroadcastConsumer,
        playback_policy: PlaybackPolicy,
    ) -> Result<Self> {
        let broadcast_name = broadcast_name.to_string();
        tracing::Span::current().record("name", tracing::field::display(&broadcast_name));
        let shutdown = CancellationToken::new();

        let (catalog_watchable, catalog_task) = {
            let track = broadcast
                .subscribe_track(&Catalog::default_track())
                .std_context("missing catalog track")?;
            debug!("catalog track subscribed");
            let mut catalog_consumer = CatalogConsumer::new(track);
            let initial_catalog = catalog_consumer
                .next()
                .await
                .std_context("Broadcast closed before receiving catalog")?
                .context("Catalog track closed before receiving catalog")?;
            debug!(
                video = initial_catalog.video.renditions.len(),
                audio = initial_catalog.audio.renditions.len(),
                "initial catalog received"
            );
            let watchable = Watchable::new(CatalogSnapshot::new(initial_catalog, 0));

            let task = tokio::spawn({
                let shutdown = shutdown.clone();
                let watchable = watchable.clone();
                async move {
                    for seq in 1.. {
                        match catalog_consumer.next().await {
                            Ok(Some(catalog)) => {
                                debug!(
                                    video = catalog.video.renditions.len(),
                                    audio = catalog.audio.renditions.len(),
                                    "catalog updated"
                                );
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
        // Always create the Sync (100 ms jitter default). It is only
        // passed to pipelines when SyncMode::Synced is active.
        let sync = crate::sync::Sync::new();

        Ok(Self {
            broadcast_name,
            broadcast,
            catalog_watchable,
            playback_policy,
            _catalog_task: Arc::new(AbortOnDropHandle::new(catalog_task)),
            shutdown,
            stats: crate::stats::SubscribeStats::default(),
            sync,
        })
    }

    /// Returns the name of this broadcast.
    pub fn broadcast_name(&self) -> &str {
        &self.broadcast_name
    }

    /// Builds a [`PipelineContext`] from the current policy and stats.
    ///
    /// When [`SyncMode::Synced`], the shared playout clock is included
    /// so the video decode loop gates frames on playout time. When
    /// [`SyncMode::Unmanaged`], `sync` is `None` and the decode loop
    /// falls back to PTS-cadence pacing.
    fn pipeline_ctx(&self) -> PipelineContext {
        let sync = match self.playback_policy.sync {
            SyncMode::Synced => Some(self.sync.clone()),
            SyncMode::Unmanaged => None,
        };
        PipelineContext {
            stats: self.stats.decode_stats(),
            sync,
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

    /// Returns true if the catalog advertises a chat track.
    pub fn has_chat(&self) -> bool {
        self.catalog()
            .chat
            .as_ref()
            .is_some_and(|c| c.message.is_some())
    }

    /// Subscribes to the chat track and returns a [`ChatSubscriber`](crate::chat::ChatSubscriber).
    ///
    /// Returns `None` if the catalog does not advertise a chat track.
    pub fn chat(&self) -> Option<crate::chat::ChatSubscriber> {
        let track_info = self.catalog().chat.as_ref()?.message.as_ref()?.clone();
        let consumer = self.broadcast.subscribe_track(&track_info).ok()?;
        Some(crate::chat::ChatSubscriber::new(consumer))
    }

    /// Returns the user metadata from the catalog, if set by the publisher.
    pub fn user(&self) -> Option<hang::catalog::User> {
        self.catalog().user.clone()
    }

    /// Returns the subscribe-side stats. Decode and playout pipelines
    /// record into these automatically. External producers (e.g. iroh
    /// transport stats) can record additional metrics into the net
    /// stats.
    pub fn stats(&self) -> &crate::stats::SubscribeStats {
        &self.stats
    }

    /// Returns the current playback policy.
    pub fn playback_policy(&self) -> &PlaybackPolicy {
        &self.playback_policy
    }

    /// Replaces the playback policy for future track subscriptions.
    ///
    /// Already-running pipelines are not affected: they keep whatever
    /// sync mode and latency budget they were created with. Call
    /// this before subscribing to new tracks (e.g. in a resubscribe
    /// flow triggered by a UI toggle).
    pub fn set_playback_policy(&mut self, policy: PlaybackPolicy) {
        self.playback_policy = policy;
    }

    // -- Non-generic convenience methods (dynamic decoder dispatch) ──────

    /// Subscribes to both video and audio, returning combined [`MediaTracks`].
    ///
    /// Uses dynamic decoder dispatch (codec determined from the catalog).
    /// For explicit decoder selection, use [`media_with_decoders`](Self::media_with_decoders).
    pub async fn media(
        &self,
        audio_backend: &dyn AudioStreamFactory,
        playback_config: PlaybackConfig,
    ) -> Result<MediaTracks> {
        self.media_with_decoders::<crate::codec::DefaultDecoders>(audio_backend, playback_config)
            .await
    }

    // -- Generic subscription methods (for custom decoders) --

    /// Subscribes to both video and audio with a custom decoder type.
    pub async fn media_with_decoders<D: Decoders>(
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
        let max_latency = self.playback_policy.max_latency;
        let catalog = self.catalog();
        let video = &catalog.video;
        let config = video
            .renditions
            .get(track_name)
            .context("rendition not found")?;
        tracing::debug!(
            track = track_name,
            max_latency_ms = max_latency.as_millis(),
            "subscribing to video rendition"
        );
        let track_consumer = self
            .broadcast
            .subscribe_track(&Track {
                name: track_name.to_string(),
                priority: VIDEO_PRIORITY,
            })
            .anyerr()?;
        tracing::debug!(track = track_name, "track subscription created");
        let consumer = OrderedConsumer::new(track_consumer, max_latency);
        VideoTrack::from_consumer::<D>(
            track_name.to_string(),
            consumer,
            config,
            playback_config,
            self.pipeline_ctx(),
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
        let max_latency = self.playback_policy.max_latency;
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
            self.pipeline_ctx(),
        )
        .await
    }

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

    #[cfg(any_video_codec)]
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

    #[cfg(any_audio_codec)]
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

    /// Subscribes to a video track and returns a raw [`MoqPacketSource`]
    /// for reading encoded packets without decoding.
    ///
    /// Useful for recording or relaying encoded media directly.
    pub fn raw_video_track(
        &self,
        track_name: &str,
    ) -> Result<(MoqPacketSource, hang::catalog::VideoConfig)> {
        let catalog = self.catalog();
        let config = catalog
            .video
            .renditions
            .get(track_name)
            .context("video rendition not found")?
            .clone();
        let track_consumer = self
            .broadcast
            .subscribe_track(&Track {
                name: track_name.to_string(),
                priority: VIDEO_PRIORITY,
            })
            .anyerr()?;
        let consumer = OrderedConsumer::new(track_consumer, self.playback_policy.max_latency);
        Ok((MoqPacketSource::new(consumer), config))
    }

    /// Subscribes to an audio track and returns a raw [`MoqPacketSource`]
    /// for reading encoded packets without decoding.
    ///
    /// Useful for recording or relaying encoded media directly.
    pub fn raw_audio_track(
        &self,
        track_name: &str,
    ) -> Result<(MoqPacketSource, hang::catalog::AudioConfig)> {
        let catalog = self.catalog();
        let config = catalog
            .audio
            .renditions
            .get(track_name)
            .context("audio rendition not found")?
            .clone();
        let track_consumer = self
            .broadcast
            .subscribe_track(&Track {
                name: track_name.to_string(),
                priority: AUDIO_PRIORITY,
            })
            .anyerr()?;
        let consumer = OrderedConsumer::new(track_consumer, self.playback_policy.max_latency);
        Ok((MoqPacketSource::new(consumer), config))
    }

    /// Shuts down this remote broadcast subscription.
    pub fn shutdown(&self) {
        self.sync.close();
        self.shutdown.cancel();
    }
}

fn select_rendition<T, P: ToString>(
    renditions: &BTreeMap<String, T>,
    order: &[P],
) -> Option<String> {
    // Rendition keys are full track names (e.g. "video/h264-720p") while
    // presets produce short suffixes (e.g. "720p"). Match by suffix so
    // that quality selection works regardless of the codec prefix.
    for preset in order {
        let suffix = preset.to_string();
        if let Some(key) = renditions.keys().find(|k| k.ends_with(&suffix)) {
            return Some(key.clone());
        }
    }
    renditions.keys().next().cloned()
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
        opts: PipelineContext,
    ) -> Result<Self> {
        let source = MoqPacketSource::new(consumer);
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

impl Drop for VideoTrack {
    fn drop(&mut self) {
        tracing::debug!(rendition = %self.rendition(), "VideoTrack dropped");
    }
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
        opts: PipelineContext,
    ) -> Result<Self> {
        let source = MoqPacketSource::new(consumer);
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

    /// Non-blocking receive: returns the latest frame if one arrived since
    /// the last call, or `None` otherwise.
    ///
    /// Identical to [`current_frame`](Self::current_frame). Provided as an
    /// ergonomic alias for game-loop and ECS integrations that conventionally
    /// name their poll method `try_recv`.
    pub fn try_recv(&mut self) -> Option<VideoFrame> {
        self.rx.take()
    }

    /// Returns `true` if a decoded frame is available without consuming it.
    ///
    /// Useful in game loops to check whether rendering work is needed before
    /// committing to a `try_recv` call.
    pub fn has_frame(&self) -> bool {
        self.rx.has_value()
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
                .video_rendition::<D::Video>(&playback_config.decode_config(), &name)
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

/// Creates a subscribe-side preview from any [`BroadcastConsumer`](moq_lite::BroadcastConsumer).
///
/// Subscribes to the consumer's catalog, spawns decoders, and returns media
/// tracks suitable for rendering. This is the building block for previewing
/// both live capture and file import output.
pub async fn subscribe_preview_from_consumer<D: Decoders>(
    consumer: moq_lite::BroadcastConsumer,
    audio_backend: &dyn crate::traits::AudioStreamFactory,
    config: crate::format::PlaybackConfig,
) -> Result<MediaTracks> {
    let broadcast = RemoteBroadcast::new("preview", consumer).await?;
    broadcast
        .media_with_decoders::<D>(audio_backend, config)
        .await
}

/// Creates a subscribe-side preview using dynamic decoder dispatch.
///
/// Non-generic convenience over [`subscribe_preview_from_consumer`] that uses
/// [`DefaultDecoders`](crate::codec::DefaultDecoders).
pub async fn subscribe_preview(
    consumer: moq_lite::BroadcastConsumer,
    audio_backend: &dyn crate::traits::AudioStreamFactory,
    config: crate::format::PlaybackConfig,
) -> Result<MediaTracks> {
    subscribe_preview_from_consumer::<crate::codec::DefaultDecoders>(
        consumer,
        audio_backend,
        config,
    )
    .await
}
