use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use hang::catalog::{AudioConfig, Catalog, CatalogProducer, VideoConfig};
use moq_lite::BroadcastProducer;
use n0_error::Result;
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Span, debug, error, info, info_span, trace, warn};

use crate::{
    av::{
        AudioEncoder, AudioEncoderInner, AudioPreset, AudioSource, DecodeConfig, TrackKind,
        VideoEncoder, VideoEncoderInner, VideoPreset, VideoSource,
    },
    util::spawn_thread,
};

pub struct PublishBroadcast {
    producer: BroadcastProducer,
    catalog: CatalogProducer,
    state: Arc<Mutex<State>>,
    _task: Arc<AbortOnDropHandle<()>>,
}

impl PublishBroadcast {
    pub fn new() -> Self {
        let mut producer = BroadcastProducer::default();
        let catalog = Catalog::default().produce();
        producer.insert_track(catalog.consumer.track);
        let catalog = catalog.producer;

        let state = Arc::new(Mutex::new(State::default()));
        let task_handle = tokio::spawn(Self::run(state.clone(), producer.clone()));

        Self {
            producer,
            catalog,
            state,
            _task: Arc::new(AbortOnDropHandle::new(task_handle)),
        }
    }

    pub fn producer(&self) -> BroadcastProducer {
        self.producer.clone()
    }

    async fn run(state: Arc<Mutex<State>>, mut producer: BroadcastProducer) {
        while let Some(track) = producer.requested_track().await {
            let name = track.info.name.clone();
            let Some(kind) = TrackKind::from_name(&name) else {
                info!("ignoring unsupported track: {name}");
                continue;
            };
            if state
                .lock()
                .expect("poisoned")
                .start_track(kind, track.clone())
            {
                info!("started track: {name}");
                tokio::spawn({
                    let inner = state.clone();
                    async move {
                        track.unused().await;
                        info!("stopping track: {name}");
                        inner.lock().expect("poisoned").stop_track(&name);
                    }
                });
            }
        }
    }

    /// Create a local WatchTrack from the current video source, if present.
    pub fn watch_local(&self, decode_config: DecodeConfig) -> Option<crate::subscribe::WatchTrack> {
        let (source, shutdown) = {
            let inner = self.state.lock().expect("poisoned");
            let source = inner
                .available_video
                .as_ref()
                .map(|video| video.source.clone())?;
            Some((source, inner.shutdown_token.child_token()))
        }?;
        Some(crate::subscribe::WatchTrack::from_video_source(
            "local".to_string(),
            shutdown,
            source,
            decode_config,
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
                };
                {
                    let mut catalog = self.catalog.lock();
                    catalog.video = Some(video);
                }
                self.state.lock().expect("poisoned").available_video = Some(renditions);
            }
            None => {
                // Clear catalog and stop any active video encoders
                self.state.lock().expect("poisoned").remove_video();
                {
                    let mut catalog = self.catalog.lock();
                    catalog.video = None;
                }
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
                };
                {
                    let mut catalog = self.catalog.lock();
                    catalog.audio = Some(audio);
                }
                self.state.lock().expect("poisoned").available_audio = Some(renditions);
            }
            None => {
                // Clear catalog and stop any active audio encoders
                self.state.lock().expect("poisoned").remove_audio();
                {
                    let mut catalog = self.catalog.lock();
                    catalog.audio = None;
                }
            }
        }
        Ok(())
    }
}

impl Drop for PublishBroadcast {
    fn drop(&mut self) {
        self.state.lock().expect("poisoned").shutdown_token.cancel();
        self.producer.close();
    }
}

#[derive(Default)]
struct State {
    shutdown_token: CancellationToken,
    available_video: Option<VideoRenditions>,
    available_audio: Option<AudioRenditions>,
    active_video: HashMap<String, EncoderThread>,
    active_audio: HashMap<String, EncoderThread>,
}

impl State {
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
    name: String,
    frames_rx: tokio::sync::watch::Receiver<Option<crate::av::VideoFrame>>,
    format: crate::av::VideoFormat,
    running: Arc<AtomicBool>,
    thread: Arc<std::thread::JoinHandle<()>>,
    subscriber_count: Arc<AtomicU32>,
}

impl SharedVideoSource {
    fn new(mut source: impl VideoSource, shutdown: CancellationToken) -> Self {
        let name = source.name().to_string();
        let format = source.format();
        let (tx, rx) = tokio::sync::watch::channel(None);
        let running = Arc::new(AtomicBool::new(false));
        let thread = spawn_thread(format!("vshr-{}", source.name()), {
            let shutdown = shutdown.clone();
            let running = running.clone();
            move || {
                let frame_time = Duration::from_secs_f32(1. / 30.);
                let start = Instant::now();
                for i in 0.. {
                    if shutdown.is_cancelled() {
                        break;
                    }

                    loop {
                        if running.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Err(err) = source.stop() {
                            warn!("Failed to stop video source: {err:#}");
                        }
                        std::thread::park();
                        if let Err(err) = source.start() {
                            warn!("Failed to stop video source: {err:#}");
                        }
                    }

                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let _ = tx.send(Some(frame));
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                    let expected = frame_time * i;
                    let actual = start.elapsed();
                    if actual < expected {
                        std::thread::sleep(expected - actual);
                    }
                }
            }
        });
        Self {
            name,
            format,
            frames_rx: rx,
            thread: Arc::new(thread),
            running,
            subscriber_count: Default::default(),
        }
    }
}

impl VideoSource for SharedVideoSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn format(&self) -> crate::av::VideoFormat {
        self.format.clone()
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let prev_count = self.subscriber_count.fetch_add(1, Ordering::Relaxed);
        if prev_count == 0 {
            self.running.store(true, Ordering::Relaxed);
            self.thread.thread().unpark();
        }
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self
            .subscriber_count
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |val| {
                Some(val.saturating_sub(1))
            })
            .expect("always returns Some")
            == 1
        {
            self.running.store(false, Ordering::Relaxed);
        }
        Ok(())
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
        let thread_name = format!("venc-{:<4}-{:<4}", source.name(), encoder.name());
        let handle = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                if let Err(err) = source.start() {
                    warn!("video source failed to start: {err:#}");
                    return;
                }
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
                            if let Err(err) = producer.write(pkt) {
                                warn!("failed to write frame to producer: {err:#}");
                            }
                        }
                    }
                    std::thread::sleep(interval.saturating_sub(start.elapsed()));
                }
                producer.inner.close();
                if let Err(err) = source.stop() {
                    warn!("video source failed to stop: {err:#}");
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
        let thread_name = format!("aenc-{:<4}", encoder.name());
        let handle = spawn_thread(thread_name, move || {
            let _guard = span.enter();
            tracing::debug!(config=?encoder.config(), "audio encoder thread start");
            let shutdown = sd;
            // 20ms framing to align with typical Opus config (48kHz → 960 samples/ch)
            const INTERVAL: Duration = Duration::from_millis(20);
            let format = source.format();
            let samples_per_frame = (format.sample_rate / 1000) * INTERVAL.as_millis() as u32;
            let mut buf = vec![0.0f32; samples_per_frame as usize * format.channel_count as usize];
            let start = Instant::now();
            for tick in 0.. {
                trace!("tick");
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
                        while let Ok(Some(pkt)) = encoder
                            .pop_packet()
                            .inspect_err(|err| warn!("encoder error: {err:#}"))
                        {
                            if let Err(err) = producer.write(pkt) {
                                warn!("failed to write frame to producer: {err:#}");
                            }
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
                let expected_time = (tick + 1) * INTERVAL;
                let actual_time = start.elapsed();
                if actual_time > expected_time {
                    warn!("audio thread too slow by {:?}", actual_time - expected_time);
                }
                let sleep = expected_time.saturating_sub(start.elapsed());
                if sleep > Duration::ZERO {
                    std::thread::sleep(sleep);
                }
            }
            // drain
            while let Ok(Some(pkt)) = encoder.pop_packet() {
                if let Err(err) = producer.write(pkt) {
                    warn!("failed to write frame to producer: {err:#}");
                }
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
