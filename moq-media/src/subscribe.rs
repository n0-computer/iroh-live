use std::{
    collections::BTreeMap,
    future,
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
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, warn};

use crate::{
    format::{DecodeConfig, DecodedVideoFrame, PixelFormat, PlaybackConfig, Quality},
    pipeline::{AudioDecoderPipeline, VideoDecoderHandle, VideoDecoderPipeline},
    processing::scale::Scaler,
    traits::{
        AudioDecoder, AudioSinkHandle, AudioStreamFactory, Decoders, VideoDecoder, VideoSource,
    },
    transport::MoqPacketSource,
    util::spawn_thread,
};

const DEFAULT_MAX_LATENCY: Duration = Duration::from_millis(150);
const VIDEO_PRIORITY: u8 = 1u8;
const AUDIO_PRIORITY: u8 = 2u8;

#[derive(derive_more::Debug, Clone)]
pub struct SubscribeBroadcast {
    broadcast_name: String,
    #[debug("BroadcastConsumer")]
    broadcast: BroadcastConsumer,
    // catalog_watcher: n0_watcher::Direct<CatalogWrapper>,
    catalog_watchable: Watchable<CatalogWrapper>,
    shutdown: CancellationToken,
    _catalog_task: Arc<AbortOnDropHandle<()>>,
}

#[derive(Debug, derive_more::PartialEq, derive_more::Eq, Default, Clone, derive_more::Deref)]
pub struct CatalogWrapper {
    #[eq(skip)]
    #[deref]
    inner: Arc<Catalog>,
    seq: usize,
}

impl CatalogWrapper {
    fn new(inner: Catalog, seq: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            seq,
        }
    }

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

    pub fn audio_renditions(&self) -> impl Iterator<Item = &str> + '_ {
        self.inner
            .audio
            .renditions
            .iter()
            .map(|(name, _config)| name.as_str())
    }

    pub fn select_video_rendition(&self, quality: Quality) -> Result<String> {
        let video = &self.inner.video;
        let track_name =
            select_video_rendition(&video.renditions, quality).context("no video renditions")?;
        Ok(track_name)
    }

    pub fn select_audio_rendition(&self, quality: Quality) -> Result<String> {
        let audio = &self.inner.audio;
        let track_name =
            select_audio_rendition(&audio.renditions, quality).context("no audio renditions")?;
        Ok(track_name)
    }

    pub fn into_inner(self) -> Arc<Catalog> {
        self.inner
    }
}

impl SubscribeBroadcast {
    pub async fn new(broadcast_name: String, broadcast: BroadcastConsumer) -> Result<Self> {
        let shutdown = CancellationToken::new();

        let (catalog_watchable, catalog_task) = {
            let track = broadcast
                .subscribe_track(&Catalog::default_track())
                .anyerr()?;
            let mut consumer = CatalogConsumer::new(track);
            let initial_catalog = consumer
                .next()
                .await
                .std_context("Broadcast closed before receiving catalog")?
                .context("Catalog track closed before receiving catalog")?;
            let watchable = Watchable::new(CatalogWrapper::new(initial_catalog, 0));

            let task = tokio::spawn({
                let shutdown = shutdown.clone();
                let watchable = watchable.clone();
                async move {
                    for seq in 1.. {
                        match consumer.next().await {
                            Ok(Some(catalog)) => {
                                watchable.set(CatalogWrapper::new(catalog, seq)).ok();
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
            });
            (watchable, task)
        };
        Ok(Self {
            broadcast_name,
            broadcast,
            catalog_watchable,
            _catalog_task: Arc::new(AbortOnDropHandle::new(catalog_task)),
            shutdown,
        })
    }

    pub fn broadcast_name(&self) -> &str {
        &self.broadcast_name
    }

    pub fn catalog_watcher(&mut self) -> n0_watcher::Direct<CatalogWrapper> {
        self.catalog_watchable.watch()
    }

    pub fn catalog(&self) -> CatalogWrapper {
        self.catalog_watchable.get()
    }

    pub async fn watch_and_listen<D: Decoders>(
        self,
        audio_backend: &dyn AudioStreamFactory,
        playback_config: PlaybackConfig,
    ) -> Result<AvRemoteTrack> {
        AvRemoteTrack::new::<D>(self, audio_backend, playback_config).await
    }

    pub fn watch<D: VideoDecoder>(&self) -> Result<WatchTrack> {
        self.watch_with::<D>(&Default::default(), Quality::Highest)
    }

    pub fn watch_with<D: VideoDecoder>(
        &self,
        playback_config: &DecodeConfig,
        quality: Quality,
    ) -> Result<WatchTrack> {
        let track_name = self.catalog().select_video_rendition(quality)?;
        self.watch_rendition::<D>(playback_config, &track_name)
    }

    pub fn watch_rendition<D: VideoDecoder>(
        &self,
        playback_config: &DecodeConfig,
        track_name: &str,
    ) -> Result<WatchTrack> {
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
            DEFAULT_MAX_LATENCY,
        );
        WatchTrack::from_consumer::<D>(track_name.to_string(), consumer, config, playback_config)
    }
    pub async fn listen<D: AudioDecoder>(
        &self,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack> {
        self.listen_with::<D>(Quality::Highest, audio_backend).await
    }

    pub async fn listen_with<D: AudioDecoder>(
        &self,
        quality: Quality,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack> {
        let track_name = self.catalog().select_audio_rendition(quality)?;
        self.listen_rendition::<D>(&track_name, audio_backend).await
    }

    pub async fn listen_rendition<D: AudioDecoder>(
        &self,
        name: &str,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<AudioTrack> {
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
            DEFAULT_MAX_LATENCY,
        );
        AudioTrack::spawn::<D>(name.to_string(), consumer, config.clone(), audio_backend).await
    }

    pub fn closed(&self) -> impl Future<Output = moq_lite::Error> + 'static {
        let broadcast = self.broadcast.clone();
        async move { broadcast.closed().await }
    }

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
    ) -> Result<Self> {
        let source = MoqPacketSource(consumer);
        let pipeline = AudioDecoderPipeline::new::<D>(name, source, &config, audio_backend).await?;
        Ok(Self { pipeline })
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + 'static {
        self.pipeline.stopped()
    }

    pub fn rendition(&self) -> &str {
        self.pipeline.name()
    }

    pub fn handle(&self) -> &dyn AudioSinkHandle {
        self.pipeline.handle()
    }
}

#[derive(derive_more::Debug)]
pub struct WatchTrack {
    #[debug(skip)]
    rx: mpsc::Receiver<DecodedVideoFrame>,
    inner: WatchTrackInner,
}

#[derive(derive_more::Debug)]
enum WatchTrackInner {
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
    /// Empty placeholder.
    #[debug("Empty")]
    Empty {
        rendition: String,
        viewport: Watchable<(u32, u32)>,
        _task: AbortOnDropHandle<()>,
    },
}

impl WatchTrack {
    /// Creates an empty placeholder that never produces frames.
    pub fn empty(rendition: impl ToString) -> Self {
        let (tx, rx) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            future::pending::<()>().await;
            let _ = tx;
        });
        Self {
            rx,
            inner: WatchTrackInner::Empty {
                rendition: rendition.to_string(),
                viewport: Default::default(),
                _task: AbortOnDropHandle::new(task),
            },
        }
    }

    /// Creates a track from a raw [`VideoSource`] (e.g. camera capture).
    pub fn from_video_source(
        rendition: String,
        shutdown: CancellationToken,
        mut source: impl VideoSource,
        decode_config: DecodeConfig,
    ) -> Self {
        let viewport = Watchable::new((1u32, 1u32));
        let (frame_tx, frame_rx) = mpsc::channel::<DecodedVideoFrame>(2);
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
                            let [w, h] = frame.format.dimensions;
                            let rgba = match scaler.scale_rgba(&frame.raw, w, h) {
                                Ok(Some((scaled, sw, sh))) => {
                                    image::RgbaImage::from_raw(sw, sh, scaled)
                                }
                                _ => image::RgbaImage::from_raw(w, h, frame.raw.to_vec()),
                            };
                            if let Some(mut img) = rgba {
                                if decode_config.pixel_format == PixelFormat::Bgra {
                                    for pixel in img.chunks_exact_mut(4) {
                                        pixel.swap(0, 2);
                                    }
                                }
                                let decoded = DecodedVideoFrame::from_image(img, start.elapsed());
                                let _ = frame_tx.blocking_send(decoded);
                            }
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
            inner: WatchTrackInner::VideoSource {
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
    ) -> Result<Self> {
        let source = MoqPacketSource(consumer);
        let pipeline = VideoDecoderPipeline::new::<D>(rendition, source, config, playback_config)?;
        Ok(Self::from_pipeline(pipeline))
    }

    /// Creates a `WatchTrack` from a standalone [`VideoDecoderPipeline`].
    pub fn from_pipeline(pipeline: VideoDecoderPipeline) -> Self {
        let VideoDecoderPipeline { frames, handle } = pipeline;
        Self {
            rx: frames.into_rx(),
            inner: WatchTrackInner::Pipeline(handle),
        }
    }

    pub fn set_viewport(&self, w: u32, h: u32) {
        match &self.inner {
            WatchTrackInner::Pipeline(handle) => handle.set_viewport(w, h),
            WatchTrackInner::VideoSource { viewport, .. }
            | WatchTrackInner::Empty { viewport, .. } => {
                viewport.set((w, h)).ok();
            }
        }
    }

    pub fn rendition(&self) -> &str {
        match &self.inner {
            WatchTrackInner::Pipeline(handle) => handle.rendition(),
            WatchTrackInner::VideoSource { rendition, .. }
            | WatchTrackInner::Empty { rendition, .. } => rendition,
        }
    }

    pub fn decoder_name(&self) -> &str {
        match &self.inner {
            WatchTrackInner::Pipeline(handle) => handle.decoder_name(),
            WatchTrackInner::VideoSource { .. } => "capture",
            WatchTrackInner::Empty { .. } => "",
        }
    }

    /// Returns the most recent decoded frame, draining any older buffered frames.
    pub fn current_frame(&mut self) -> Option<DecodedVideoFrame> {
        let mut out = None;
        while let Ok(item) = self.rx.try_recv() {
            out = Some(item);
        }
        out
    }

    /// Returns the next decoded frame, waiting if none is buffered.
    pub async fn next_frame(&mut self) -> Option<DecodedVideoFrame> {
        if let Some(frame) = self.current_frame() {
            Some(frame)
        } else {
            self.rx.recv().await
        }
    }
}

#[derive(derive_more::Debug)]
pub struct AvRemoteTrack {
    pub broadcast: SubscribeBroadcast,
    pub video: Option<WatchTrack>,
    pub audio: Option<AudioTrack>,
}

impl AvRemoteTrack {
    pub async fn new<D: Decoders>(
        broadcast: SubscribeBroadcast,
        audio_backend: &dyn AudioStreamFactory,
        playback_config: PlaybackConfig,
    ) -> Result<Self> {
        let audio = broadcast
            .listen_with::<D::Audio>(playback_config.quality, audio_backend)
            .await
            .inspect_err(|err| tracing::warn!("no audio track: {err}"))
            .ok();
        let video = broadcast
            .watch_with::<D::Video>(&playback_config.decode_config, playback_config.quality)
            .inspect_err(|err| tracing::warn!("no video track: {err}"))
            .ok();
        Ok(Self {
            broadcast,
            audio,
            video,
        })
    }
}
