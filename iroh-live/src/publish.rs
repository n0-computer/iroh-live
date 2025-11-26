use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use hang::{
    Catalog, CatalogProducer,
    catalog::{AudioConfig, VideoConfig},
};
use moq_lite::BroadcastProducer;
use n0_error::Result;
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Span, debug, error, info, info_span, trace, warn};

use crate::av::{
    AudioEncoder, AudioEncoderInner, AudioPreset, AudioSource, TrackKind, VideoEncoder,
    VideoEncoderInner, VideoPreset, VideoSource,
};

pub struct PublishBroadcast {
    producer: BroadcastProducer,
    catalog: CatalogProducer,
    inner: Arc<Mutex<Inner>>,
    _task: AbortOnDropHandle<()>,
}

impl PublishBroadcast {
    pub fn new() -> Self {
        let mut producer = BroadcastProducer::default();
        let catalog = Catalog::default().produce();
        producer.insert_track(catalog.consumer.track);
        let catalog = catalog.producer;

        let inner = Arc::new(Mutex::new(Inner::default()));
        let task_handle = tokio::spawn(Self::run(inner.clone(), producer.clone()));

        Self {
            producer,
            catalog,
            inner,
            _task: AbortOnDropHandle::new(task_handle),
        }
    }

    pub fn producer(&self) -> BroadcastProducer {
        self.producer.clone()
    }

    async fn run(inner: Arc<Mutex<Inner>>, mut producer: BroadcastProducer) {
        while let Some(track) = producer.requested_track().await {
            let name = track.info.name.clone();
            let Some(kind) = TrackKind::from_name(&name) else {
                info!("ignoring unsupported track: {name}");
                continue;
            };
            if inner
                .lock()
                .expect("poisoned")
                .start_track(kind, track.clone())
            {
                tokio::spawn({
                    let inner = inner.clone();
                    async move {
                        track.unused().await;
                        inner.lock().expect("poisoned").stop_track(&name);
                    }
                });
            }
        }
    }

    /// Create a local WatchTrack from the current video source, if present.
    pub fn watch_local(&self) -> Option<crate::subscribe::WatchTrack> {
        let (source, shutdown) = {
            let inner = self.inner.lock().expect("poisoned");
            let source = inner
                .available_video
                .as_ref()
                .map(|video| video.source.clone())?;
            Some((source, inner.shutdown_token.child_token()))
        }?;
        Some(crate::subscribe::WatchTrack::from_shared_source(
            "local".to_string(),
            shutdown,
            source,
        ))
    }

    pub fn set_video(&mut self, renditions: Option<VideoRenditions>) -> Result<()> {
        match renditions {
            Some(renditions) => {
                let priority = 1u8;
                let configs = renditions.available_renditions()?;
                let video = hang::catalog::Video {
                    renditions: configs,
                    priority,
                    display: None,
                    rotation: None,
                    flip: None,
                    detection: None,
                };
                self.catalog.set_video(Some(video));
                self.catalog.publish();
                self.inner.lock().expect("poisoned").available_video = Some(renditions);
            }
            None => {
                // Clear catalog and stop any active video encoders
                self.inner.lock().expect("poisoned").remove_video();
                self.catalog.set_video(None);
                self.catalog.publish();
            }
        }
        Ok(())
    }

    pub fn set_audio(&mut self, renditions: Option<AudioRenditions>) -> Result<()> {
        match renditions {
            Some(renditions) => {
                let priority = 2u8;
                let configs = renditions.available_renditions()?;
                let audio = hang::catalog::Audio {
                    renditions: configs,
                    priority,
                    captions: None,
                    speaking: None,
                };
                self.catalog.set_audio(Some(audio));
                self.catalog.publish();
                self.inner.lock().expect("poisoned").available_audio = Some(renditions);
            }
            None => {
                // Clear catalog and stop any active audio encoders
                self.inner.lock().expect("poisoned").remove_audio();
                self.catalog.set_audio(None);
                self.catalog.publish();
            }
        }
        Ok(())
    }
}

impl Drop for PublishBroadcast {
    fn drop(&mut self) {
        self.inner.lock().expect("poisoned").shutdown_token.cancel();
        self.producer.close();
    }
}

#[derive(Default)]
struct Inner {
    shutdown_token: CancellationToken,
    available_video: Option<VideoRenditions>,
    available_audio: Option<AudioRenditions>,
    active_video: HashMap<String, EncoderThread>,
    active_audio: HashMap<String, EncoderThread>,
}

impl Inner {
    fn stop_track(&mut self, name: &str) {
        let thread = self
            .active_video
            .remove(name)
            .or_else(|| self.active_audio.remove(name));
        if let Some(thread) = thread {
            thread.shutdown.cancel();
        }
    }

    fn remove_audio(&mut self) {
        for (_name, thread) in self.active_audio.drain() {
            thread.shutdown.cancel();
        }
        self.available_audio = None;
    }

    fn remove_video(&mut self) {
        for (_name, thread) in self.active_video.drain() {
            thread.shutdown.cancel();
        }
        self.available_video = None;
    }

    fn start_track(&mut self, kind: TrackKind, track: moq_lite::TrackProducer) -> bool {
        let name = track.info.name.clone();
        let track = hang::TrackProducer::new(track);
        let shutdown_token = self.shutdown_token.child_token();
        match kind {
            TrackKind::Video => {
                if let Some(video) = self.available_video.as_mut()
                    && let Some(encoder_thread) = video.start_encoder(&name, track, shutdown_token)
                {
                    self.active_video.insert(name, encoder_thread);
                    true
                } else {
                    info!("ignoring video track request {name}: rendition not available");
                    false
                }
            }
            TrackKind::Audio => {
                if let Some(audio) = self.available_audio.as_mut()
                    && let Some(encoder_thread) = audio.start_encoder(&name, track, shutdown_token)
                {
                    self.active_audio.insert(name, encoder_thread);
                    true
                } else {
                    info!("ignoring audio track request {name}: rendition not available");
                    false
                }
            }
        }
    }
}

pub struct AudioRenditions {
    make_encoder: Box<dyn Fn(AudioPreset) -> Result<Box<dyn AudioEncoder>> + Send>,
    source: Box<dyn AudioSource>,
    renditions: HashMap<String, AudioPreset>,
}

impl AudioRenditions {
    pub fn new<E: AudioEncoder>(
        source: impl AudioSource,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Self {
        let renditions = presets
            .into_iter()
            .map(|preset| (format!("audio-{preset}"), preset))
            .collect();
        Self {
            make_encoder: Box::new(|preset| Ok(Box::new(E::with_preset(preset)?))),
            renditions,
            source: Box::new(source),
        }
    }

    pub fn available_renditions(&self) -> Result<HashMap<String, AudioConfig>> {
        let mut renditions = HashMap::new();
        for (name, preset) in self.renditions.iter() {
            // We need to create the encoder to get the config, even though we drop it
            // again (it will be created on deman). Not ideal, but works for now.
            let config = (self.make_encoder)(*preset)?.config();
            renditions.insert(name.clone(), config);
        }
        Ok(renditions)
    }

    pub fn encoder(&mut self, name: &str) -> Option<Result<Box<dyn AudioEncoder>>> {
        let preset = self.renditions.get(name)?;
        Some((self.make_encoder)(*preset))
    }

    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: hang::TrackProducer,
        shutdown_token: CancellationToken,
    ) -> Option<EncoderThread> {
        let encoder = self
            .encoder(name)?
            .inspect_err(|err| error!("failed to create audio encoder: {err:#?}"))
            .ok()?;
        let span = info_span!("audioenc", %name);
        let thread = EncoderThread::spawn_audio(
            self.source.cloned_boxed(),
            encoder,
            producer,
            shutdown_token,
            span,
        );
        Some(thread)
    }
}

pub struct VideoRenditions {
    make_encoder: Box<dyn Fn(VideoPreset) -> Result<Box<dyn VideoEncoder>> + Send>,
    source: SharedVideoSource,
    renditions: HashMap<String, VideoPreset>,
    _shared_source_cancel_guard: DropGuard,
}

impl VideoRenditions {
    pub fn new<E: VideoEncoder>(
        source: impl VideoSource,
        presets: impl IntoIterator<Item = VideoPreset>,
    ) -> Self {
        let shutdown_token = CancellationToken::new();
        let source = SharedVideoSource::new(source, shutdown_token.clone());
        let renditions = presets
            .into_iter()
            .map(|preset| (format!("video-{preset}"), preset))
            .collect();
        Self {
            make_encoder: Box::new(|preset| Ok(Box::new(E::with_preset(preset)?))),
            renditions,
            source,
            _shared_source_cancel_guard: shutdown_token.drop_guard(),
        }
    }

    pub fn available_renditions(&self) -> Result<HashMap<String, VideoConfig>> {
        let mut renditions = HashMap::new();
        for (name, preset) in self.renditions.iter() {
            // We need to create the encoder to get the config, even though we drop it
            // again (it will be created on deman). Not ideal, but works for now.
            let config = (self.make_encoder)(*preset)?.config();
            renditions.insert(name.clone(), config);
        }
        Ok(renditions)
    }

    pub fn encoder(&mut self, name: &str) -> Option<Result<Box<dyn VideoEncoder>>> {
        let preset = self.renditions.get(name)?;
        Some((self.make_encoder)(*preset))
    }

    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: hang::TrackProducer,
        shutdown_token: CancellationToken,
    ) -> Option<EncoderThread> {
        let encoder = self
            .encoder(name)?
            .inspect_err(|err| error!("failed to create video encoder: {err:#?}"))
            .ok()?;
        let span = info_span!("videoenc", %name);
        let thread = EncoderThread::spawn_video(
            self.source.clone(),
            encoder,
            producer,
            shutdown_token,
            span,
        );
        Some(thread)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SharedVideoSource {
    frames_rx: tokio::sync::watch::Receiver<Option<crate::av::VideoFrame>>,
    format: crate::av::VideoFormat,
}

impl SharedVideoSource {
    fn new(mut source: impl VideoSource, shutdown: CancellationToken) -> Self {
        let format = source.format();
        let (tx, rx) = tokio::sync::watch::channel(None);
        std::thread::spawn({
            let shutdown = shutdown.clone();
            move || {
                loop {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let _ = tx.send(Some(frame));
                        }
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                        Err(_) => break,
                    }
                }
            }
        });
        Self {
            format,
            frames_rx: rx,
        }
    }
}

impl VideoSource for SharedVideoSource {
    fn format(&self) -> crate::av::VideoFormat {
        self.format.clone()
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<crate::av::VideoFrame>> {
        let frame = self.frames_rx.borrow_and_update().clone();
        Ok(frame)
    }
}

pub struct EncoderThread {
    _thread_handle: std::thread::JoinHandle<()>,
    shutdown: CancellationToken,
}

impl EncoderThread {
    pub fn spawn_video(
        mut source: impl VideoSource,
        mut encoder: impl VideoEncoderInner,
        mut producer: hang::TrackProducer,
        shutdown: CancellationToken,
        span: Span,
    ) -> Self {
        let handle = std::thread::spawn({
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                let format = source.format();
                tracing::debug!(
                    src_format = ?format,
                    dst_config = ?encoder.config(),
                    "video encoder thread start"
                );
                let framerate = encoder.config().framerate.unwrap_or(30.0);
                let interval = Duration::from_secs_f64(1. / framerate);
                loop {
                    let start = Instant::now();
                    if shutdown.is_cancelled() {
                        debug!("stop video encoder: cancelled");
                        break;
                    }
                    let frame = match source.pop_frame() {
                        Ok(frame) => frame,
                        Err(err) => {
                            warn!("video encoder failed: {err:#}");
                            break;
                        }
                    };
                    if let Some(frame) = frame {
                        if let Err(err) = encoder.push_frame(frame) {
                            warn!("video encoder failed: {err:#}");
                            break;
                        };
                        while let Ok(Some(pkt)) = encoder.pop_packet() {
                            producer.write(pkt);
                        }
                    }
                    std::thread::sleep(interval.saturating_sub(start.elapsed()));
                }
                tracing::debug!("video encoder thread stop");
            }
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }

    pub fn spawn_audio(
        mut source: Box<dyn AudioSource>,
        mut encoder: impl AudioEncoderInner,
        mut producer: hang::TrackProducer,
        shutdown: CancellationToken,
        span: tracing::Span,
    ) -> Self {
        let sd = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let _guard = span.enter();
            tracing::debug!(config=?encoder.config(), "audio encoder thread start");
            let shutdown = sd;
            // 20ms framing to align with typical Opus config (48kHz → 960 samples/ch)
            const INTERVAL: Duration = Duration::from_millis(20);
            let format = source.format();
            let samples_per_frame = (format.sample_rate / 1000) * INTERVAL.as_millis() as u32;
            let mut buf = vec![0.0f32; samples_per_frame as usize * format.channel_count as usize];
            loop {
                trace!("tick");
                let start = Instant::now();
                if shutdown.is_cancelled() {
                    break;
                }
                match source.pop_samples(&mut buf) {
                    Ok(Some(_n)) => {
                        // Expect a full frame; if shorter, zero-pad via slice len
                        if let Err(err) = encoder.push_samples(&buf) {
                            error!("audio push_samples failed: {err:#}");
                            break;
                        }
                        while let Ok(Some(pkt)) = encoder.pop_packet() {
                            producer.write(pkt);
                        }
                    }
                    Ok(None) => {
                        // keep pacing
                    }
                    Err(err) => {
                        error!("audio source failed: {err:#}");
                        break;
                    }
                }
                let elapsed = start.elapsed();
                if elapsed > INTERVAL {
                    warn!(
                        "audio thread too slow: took {:?} for interval of {:?}",
                        elapsed, INTERVAL
                    );
                }
                let sleep = INTERVAL.saturating_sub(start.elapsed());
                std::thread::sleep(sleep);
            }
            // drain
            while let Ok(Some(pkt)) = encoder.pop_packet() {
                producer.write(pkt);
            }
            producer.inner.close();
            tracing::debug!("audio encoder thread stop");
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }
}

impl Drop for EncoderThread {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
