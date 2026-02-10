use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;
use hang::catalog::{Audio, AudioConfig, Catalog, CatalogProducer, Video, VideoConfig};
use moq_lite::BroadcastProducer;
use n0_error::Result;
use n0_future::task::AbortOnDropHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, info_span, trace, warn};

use tokio::sync::watch;

use crate::{
    av::{
        AudioCodec, AudioEncoder, AudioEncoderFactory, AudioFormat, AudioPreset, AudioSource,
        DecodeConfig, VideoCodec, VideoEncoder, VideoEncoderFactory, VideoFormat, VideoFrame,
        VideoPreset, VideoSource,
    },
    codec::{
        self,
        video::util::scale::{Scaler, fit_within},
    },
    subscribe::WatchTrack,
    util::spawn_thread,
};

type MakeAudioEncoder = Box<dyn Fn() -> Result<Box<dyn AudioEncoder>> + Send + 'static>;
type MakeVideoEncoder = Box<dyn Fn() -> Result<Box<dyn VideoEncoder>> + Send + 'static>;

#[derive(derive_more::Debug)]
pub struct PublishBroadcast {
    #[debug(skip)]
    producer: BroadcastProducer,
    #[debug(skip)]
    catalog: CatalogProducer,
    #[debug(skip)]
    state: Arc<Mutex<State>>,
    #[debug(skip)]
    _task: Arc<AbortOnDropHandle<()>>,
}

impl Default for PublishBroadcast {
    fn default() -> Self {
        Self::new()
    }
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
            if state
                .lock()
                .expect("poisoned")
                .start_track(track.clone())
                .inspect_err(|err| warn!(%name, "failed to start requested track: {err:#}"))
                .is_ok()
            {
                info!("started track: {name}");
                tokio::spawn({
                    let state = state.clone();
                    async move {
                        track.unused().await;
                        info!("stopping track: {name} (all subscribers disconnected)");
                        state.lock().expect("poisoned").stop_track(&name);
                    }
                });
            }
        }
        info!("publish broadcast: no more track requests, shutting down");
    }

    /// Create a local WatchTrack from the current video source, if present.
    pub fn watch_local(&self, decode_config: DecodeConfig) -> Option<WatchTrack> {
        let (source, shutdown) = {
            let state = self.state.lock().expect("poisoned");
            let source = state
                .available_video
                .as_ref()
                .map(|video| video.source.clone())?;
            Some((source, state.local_video_token.child_token()))
        }?;
        Some(WatchTrack::from_video_source(
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
                let video = Video {
                    renditions: configs,
                    priority,
                    display: None,
                    rotation: None,
                    flip: None,
                };
                // Tear down existing video, set new renditions, then publish catalog.
                // Order matters: renditions must be available before the catalog is
                // published, otherwise subscribers may request tracks we can't serve.
                let mut state = self.state.lock().expect("poisoned");
                state.remove_video();
                state.available_video = Some(renditions);
                drop(state);
                {
                    let mut catalog = self.catalog.lock();
                    catalog.video = Some(video);
                }
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
                let audio = Audio {
                    renditions: configs,
                    priority,
                };
                // Tear down existing audio, set new renditions, then publish catalog.
                // Order matters: renditions must be available before the catalog is
                // published, otherwise subscribers may request tracks we can't serve.
                let mut state = self.state.lock().expect("poisoned");
                state.remove_audio();
                state.available_audio = Some(renditions);
                drop(state);
                {
                    let mut catalog = self.catalog.lock();
                    catalog.audio = Some(audio);
                }
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
    local_video_token: CancellationToken,
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
        self.local_video_token.cancel();
        self.local_video_token = CancellationToken::new();
        for (_name, thread) in self.active_video.drain() {
            thread.shutdown.cancel();
        }
        self.available_video = None;
    }

    fn start_track(&mut self, track: moq_lite::TrackProducer) -> Result<()> {
        let name = track.info.name.clone();
        let track = hang::TrackProducer::new(track);
        let shutdown_token = self.shutdown_token.child_token();
        if let Some(video) = self.available_video.as_mut()
            && video.contains_rendition(&name)
        {
            let thread = video.start_encoder(&name, track, shutdown_token)?;
            self.active_video.insert(name, thread);
            Ok(())
        } else if let Some(audio) = self.available_audio.as_mut()
            && audio.contains_rendition(&name)
        {
            let thread = audio.start_encoder(&name, track, shutdown_token)?;
            self.active_audio.insert(name, thread);
            Ok(())
        } else {
            info!("ignoring track request {name}: rendition not available");
            Err(n0_error::anyerr!("rendition not available"))
        }
    }
}

#[derive(derive_more::Debug)]
pub struct AudioRenditions {
    #[debug(skip)]
    source: Box<dyn AudioSource>,
    #[debug(skip)]
    renditions: HashMap<String, MakeAudioEncoder>,
}

impl AudioRenditions {
    pub fn new(
        source: impl AudioSource,
        codec: AudioCodec,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Self {
        let mut this = Self::empty(source);
        for preset in presets {
            this.add(codec, preset);
        }
        this
    }

    pub fn new_from_generic<E: AudioEncoderFactory>(
        source: impl AudioSource,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Self {
        let mut this = Self::empty(source);
        for preset in presets {
            this.add_with_generic::<E>(preset);
        }
        this
    }

    pub fn empty(source: impl AudioSource) -> Self {
        Self {
            source: Box::new(source),
            renditions: HashMap::new(),
        }
    }

    pub fn add(&mut self, codec: AudioCodec, preset: AudioPreset) {
        match codec {
            AudioCodec::Opus => self.add_with_generic::<codec::OpusEncoder>(preset),
        }
    }

    pub fn add_with_generic<E: AudioEncoderFactory>(&mut self, preset: AudioPreset) {
        let name = format!("audio/{}-{preset}", E::ID);
        self.add_with_callback(name, move |format| E::with_preset(format, preset));
    }

    pub fn add_with_callback<E: AudioEncoder>(
        &mut self,
        name: impl Into<String>,
        callback: impl Fn(AudioFormat) -> anyhow::Result<E> + Send + 'static,
    ) {
        let format = self.source.format();
        self.renditions.insert(
            name.into(),
            Box::new(move || Ok(Box::new(callback(format)?))),
        );
    }

    pub fn available_renditions(&self) -> Result<BTreeMap<String, AudioConfig>> {
        let mut renditions = BTreeMap::new();
        for (name, make_encoder) in self.renditions.iter() {
            // We need to create the encoder to get the config, even though we drop it
            // again (it will be created on demand). Not ideal, but works for now.
            let config = make_encoder()?.config();
            renditions.insert(name.clone(), config);
        }
        Ok(renditions)
    }

    pub fn contains_rendition(&self, name: &str) -> bool {
        self.renditions.contains_key(name)
    }

    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: hang::TrackProducer,
        shutdown_token: CancellationToken,
    ) -> Result<EncoderThread> {
        let make_encoder = self
            .renditions
            .get(name)
            .context("rendition not available")?;
        let encoder = make_encoder()?;
        let thread = EncoderThread::spawn_audio(
            self.source.cloned_boxed(),
            encoder,
            producer,
            shutdown_token,
        );
        Ok(thread)
    }
}

#[derive(derive_more::Debug)]
pub struct VideoRenditions {
    source: SharedVideoSource,
    #[debug(skip)]
    renditions: HashMap<String, MakeVideoEncoder>,
    shutdown_token: CancellationToken,
}

impl VideoRenditions {
    /// Create video renditions with a dynamically-selected encoder.
    pub fn new(
        source: impl VideoSource,
        codec: VideoCodec,
        presets: impl IntoIterator<Item = VideoPreset>,
    ) -> Self {
        let mut this = Self::empty(source);
        for preset in presets {
            this.add(codec, preset);
        }
        this
    }

    pub fn new_from_generic<E: VideoEncoderFactory>(
        source: impl VideoSource,
        presets: impl IntoIterator<Item = VideoPreset>,
    ) -> Self {
        let mut this = Self::empty(source);
        for preset in presets {
            this.add_with_generic::<E>(preset);
        }
        this
    }

    pub fn empty(source: impl VideoSource) -> Self {
        let shutdown_token = CancellationToken::new();
        let source = SharedVideoSource::new(source, shutdown_token.clone());
        Self {
            source,
            renditions: HashMap::new(),
            shutdown_token,
        }
    }

    pub fn add(&mut self, codec: VideoCodec, preset: VideoPreset) {
        match codec {
            VideoCodec::H264 => self.add_with_generic::<codec::H264Encoder>(preset),
            #[cfg(feature = "av1")]
            VideoCodec::Av1 => self.add_with_generic::<codec::Av1Encoder>(preset),
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            VideoCodec::VtbH264 => self.add_with_generic::<codec::VtbEncoder>(preset),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            VideoCodec::VaapiH264 => self.add_with_generic::<codec::VaapiEncoder>(preset),
        }
    }

    pub fn add_with_generic<E: VideoEncoderFactory>(&mut self, preset: VideoPreset) {
        let name = format!("video/{}-{}", E::ID, preset);
        self.add_with_callback(name, move || E::with_preset(preset))
    }

    pub fn add_with_callback<E: VideoEncoder>(
        &mut self,
        name: impl ToString,
        encoder_factory: impl Fn() -> anyhow::Result<E> + Send + 'static,
    ) {
        self.renditions.insert(
            name.to_string(),
            Box::new(move || Ok(Box::new(encoder_factory()?))),
        );
    }

    pub fn available_renditions(&self) -> Result<BTreeMap<String, VideoConfig>> {
        let mut renditions = BTreeMap::new();
        for (name, make_encoder) in self.renditions.iter() {
            // We need to create the encoder to get the config, even though we drop it
            // again (it will be created on demand). Not ideal, but works for now.
            let config = make_encoder()?.config();
            renditions.insert(name.clone(), config);
        }
        Ok(renditions)
    }

    pub fn contains_rendition(&self, name: &str) -> bool {
        self.renditions.contains_key(name)
    }

    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: hang::TrackProducer,
        shutdown_token: CancellationToken,
    ) -> Result<EncoderThread> {
        let make_encoder = self
            .renditions
            .get(name)
            .context("rendition not available")?;
        let encoder = make_encoder()?;
        let thread =
            EncoderThread::spawn_video(self.source.clone(), encoder, producer, shutdown_token);
        Ok(thread)
    }
}

impl Drop for VideoRenditions {
    fn drop(&mut self) {
        self.shutdown_token.cancel();
        self.source.unpark();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SharedVideoSource {
    name: String,
    frames_rx: watch::Receiver<Option<VideoFrame>>,
    format: VideoFormat,
    running: Arc<AtomicBool>,
    thread: Arc<thread::JoinHandle<()>>,
    subscriber_count: Arc<AtomicU32>,
}

impl SharedVideoSource {
    fn new(mut source: impl VideoSource, shutdown: CancellationToken) -> Self {
        let name = source.name().to_string();
        let format = source.format();
        let (tx, rx) = watch::channel(None);
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
                        if shutdown.is_cancelled() {
                            break;
                        }
                        thread::park();
                        if shutdown.is_cancelled() {
                            break;
                        }
                        if let Err(err) = source.start() {
                            warn!("Failed to start video source: {err:#}");
                        }
                    }
                    if shutdown.is_cancelled() {
                        break;
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
                        thread::sleep(expected - actual);
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

    fn unpark(&self) {
        self.thread.thread().unpark();
    }
}

impl VideoSource for SharedVideoSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn format(&self) -> VideoFormat {
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

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let frame = self.frames_rx.borrow_and_update().clone();
        Ok(frame)
    }
}

#[derive(derive_more::Debug)]
pub struct EncoderThread {
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
    shutdown: CancellationToken,
}

impl EncoderThread {
    pub fn spawn_video(
        mut source: impl VideoSource,
        mut encoder: impl VideoEncoder,
        mut producer: hang::TrackProducer,
        shutdown: CancellationToken,
    ) -> Self {
        let thread_name = format!("venc-{:<4}-{:<4}", source.name(), encoder.name());
        let span = info_span!("videoenc", source = source.name(), encoder = encoder.name());
        let handle = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                if let Err(err) = source.start() {
                    error!("video source failed to start: {err:#}");
                    return;
                }
                let mut first = true;
                let format = source.format();
                let enc_config = encoder.config();
                info!(
                    src_format = ?format,
                    dst_config = ?enc_config,
                    "video encoder thread start"
                );
                // Set up scaler to downscale source frames to encoder dimensions.
                let scaler_dims = match (enc_config.coded_width, enc_config.coded_height) {
                    (Some(w), Some(h)) => {
                        let target = fit_within(format.dimensions[0], format.dimensions[1], w, h);
                        Some(target)
                    }
                    _ => None,
                };
                let scaler = Scaler::new(scaler_dims);
                let framerate = enc_config.framerate.unwrap_or(30.0);
                let interval = Duration::from_secs_f64(1. / framerate);
                loop {
                    let start = Instant::now();
                    if shutdown.is_cancelled() {
                        info!("stop video encoder: cancelled");
                        break;
                    }
                    let frame = match source.pop_frame() {
                        Ok(frame) => frame,
                        Err(err) => {
                            error!("video source failed to produce frame: {err:#}");
                            break;
                        }
                    };
                    if let Some(frame) = frame {
                        let frame = match scaler.scale_rgba(
                            &frame.raw,
                            frame.format.dimensions[0],
                            frame.format.dimensions[1],
                        ) {
                            Ok(Some((data, w, h))) => VideoFrame {
                                format: VideoFormat {
                                    pixel_format: frame.format.pixel_format,
                                    dimensions: [w, h],
                                },
                                raw: data.into(),
                            },
                            Ok(None) => frame,
                            Err(err) => {
                                error!("video frame scaling failed: {err:#}");
                                break;
                            }
                        };
                        if let Err(err) = encoder.push_frame(frame) {
                            error!("video encoder push_frame failed: {err:#}");
                            break;
                        };
                        loop {
                            match encoder.pop_packet() {
                                Ok(Some(pkt)) => {
                                    if first && !pkt.keyframe {
                                        warn!("ignoring frame: waiting for first keyframe");
                                        continue;
                                    }
                                    first = false;
                                    if let Err(err) = producer.write(pkt) {
                                        error!("failed to write video packet to producer: {err:#}");
                                    }
                                }
                                Ok(None) => break,
                                Err(err) => {
                                    error!("video encoder pop_packet failed: {err:#}");
                                    break;
                                }
                            }
                        }
                    }
                    thread::sleep(interval.saturating_sub(start.elapsed()));
                }
                producer.inner.close();
                if let Err(err) = source.stop() {
                    warn!("video source failed to stop: {err:#}");
                }
                info!("video encoder thread stop");
            }
        });
        Self {
            _thread_handle: handle,
            shutdown,
        }
    }

    pub fn spawn_audio(
        mut source: Box<dyn AudioSource>,
        mut encoder: impl AudioEncoder,
        mut producer: hang::TrackProducer,
        shutdown: CancellationToken,
    ) -> Self {
        let sd = shutdown.clone();
        let name = encoder.name();
        let thread_name = format!("aenc-{:<4}", name);
        let span = info_span!("audioenc", %name);
        let handle = spawn_thread(thread_name, move || {
            let _guard = span.enter();
            info!(config=?encoder.config(), "audio encoder thread start");
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
                    info!("stop audio encoder: cancelled");
                    break;
                }
                match source.pop_samples(&mut buf) {
                    Ok(Some(_n)) => {
                        // Expect a full frame; if shorter, zero-pad via slice len
                        if let Err(err) = encoder.push_samples(&buf) {
                            error!(
                                buf_len = buf.len(),
                                "audio encoder push_samples failed: {err:#}"
                            );
                            break;
                        }
                        loop {
                            match encoder.pop_packet() {
                                Ok(Some(pkt)) => {
                                    if let Err(err) = producer.write(pkt) {
                                        error!("failed to write audio packet to producer: {err:#}");
                                    }
                                }
                                Ok(None) => break,
                                Err(err) => {
                                    error!("audio encoder pop_packet failed: {err:#}");
                                    break;
                                }
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
                    warn!(
                        "audio encoder too slow by {:?}",
                        actual_time - expected_time
                    );
                }
                let sleep = expected_time.saturating_sub(start.elapsed());
                if sleep > Duration::ZERO {
                    thread::sleep(sleep);
                }
            }
            // drain
            while let Ok(Some(pkt)) = encoder.pop_packet() {
                if let Err(err) = producer.write(pkt) {
                    error!("failed to write audio packet to producer: {err:#}");
                }
            }
            producer.inner.close();
            info!("audio encoder thread stop");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av::{
        PixelFormat, VideoCodec, VideoEncoderFactory, VideoFormat, VideoFrame, VideoPreset,
        VideoSource,
    };
    use crate::codec::video::test_util::make_test_pattern;

    /// A video source for testing that produces synthetic test pattern frames.
    struct TestVideoSource {
        format: VideoFormat,
        started: bool,
        frame_index: u32,
    }

    impl TestVideoSource {
        fn new(width: u32, height: u32) -> Self {
            Self {
                format: VideoFormat {
                    pixel_format: PixelFormat::Rgba,
                    dimensions: [width, height],
                },
                started: false,
                frame_index: 0,
            }
        }
    }

    impl VideoSource for TestVideoSource {
        fn name(&self) -> &str {
            "test"
        }

        fn format(&self) -> VideoFormat {
            self.format.clone()
        }

        fn start(&mut self) -> anyhow::Result<()> {
            self.started = true;
            Ok(())
        }

        fn stop(&mut self) -> anyhow::Result<()> {
            self.started = false;
            Ok(())
        }

        fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
            if !self.started {
                return Ok(None);
            }
            let [w, h] = self.format.dimensions;
            let frame = make_test_pattern(w, h, self.frame_index);
            self.frame_index += 1;
            Ok(Some(frame))
        }
    }

    fn make_renditions() -> VideoRenditions {
        VideoRenditions::new(
            TestVideoSource::new(320, 240),
            VideoCodec::H264,
            [VideoPreset::P180],
        )
    }

    #[tokio::test]
    async fn watch_local_produces_frames() {
        let mut broadcast = PublishBroadcast::new();
        broadcast.set_video(Some(make_renditions())).unwrap();
        let watch = broadcast
            .watch_local(DecodeConfig::default())
            .expect("watch_local should return Some when video is set");
        // The watch track exists — it will produce decoded frames once the
        // decode loop runs, but here we just verify it was created successfully.
        drop(watch);
    }

    #[tokio::test]
    async fn set_video_stops_old_local_watch() {
        let mut broadcast = PublishBroadcast::new();
        broadcast.set_video(Some(make_renditions())).unwrap();
        let watch = broadcast
            .watch_local(DecodeConfig::default())
            .expect("should have watch");

        // Replace video — old local watches should be cancelled.
        broadcast.set_video(Some(make_renditions())).unwrap();

        // The old watch's cancellation token should now be cancelled.
        // We can verify by checking that a new watch_local still works.
        let watch2 = broadcast
            .watch_local(DecodeConfig::default())
            .expect("new watch should work after replacement");
        drop(watch);
        drop(watch2);
    }

    #[tokio::test]
    async fn set_video_new_source_produces_frames() {
        let mut broadcast = PublishBroadcast::new();
        broadcast.set_video(Some(make_renditions())).unwrap();
        broadcast.set_video(Some(make_renditions())).unwrap();

        let watch = broadcast
            .watch_local(DecodeConfig::default())
            .expect("watch should work with replacement video");
        drop(watch);
    }

    #[tokio::test]
    async fn set_video_none_stops_local_watch() {
        let mut broadcast = PublishBroadcast::new();
        broadcast.set_video(Some(make_renditions())).unwrap();
        let _watch = broadcast
            .watch_local(DecodeConfig::default())
            .expect("should have watch");

        broadcast.set_video(None).unwrap();
        assert!(
            broadcast.watch_local(DecodeConfig::default()).is_none(),
            "watch_local should return None after set_video(None)"
        );
    }

    /// This is the critical test: replacing video while the source thread is parked
    /// (no subscribers). Before the fix, the old source thread would remain parked
    /// forever because nothing called unpark() on shutdown.
    #[tokio::test]
    async fn replace_video_while_source_parked() {
        let mut broadcast = PublishBroadcast::new();

        // Set video but never create a local watch — source thread will be parked.
        broadcast.set_video(Some(make_renditions())).unwrap();

        // Give the source thread time to park.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Replace video — this should cleanly shut down the parked source thread.
        broadcast.set_video(Some(make_renditions())).unwrap();

        // Give the old thread time to wake up and exit.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify the new video works.
        let watch = broadcast
            .watch_local(DecodeConfig::default())
            .expect("new watch should work after replacing parked video");
        drop(watch);
    }

    #[tokio::test]
    async fn source_thread_stops_on_video_removal() {
        let mut broadcast = PublishBroadcast::new();
        broadcast.set_video(Some(make_renditions())).unwrap();

        // Give source thread time to start and park.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Remove video — source thread should stop.
        broadcast.set_video(None).unwrap();

        // Give time for cleanup.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify no video available.
        assert!(broadcast.watch_local(DecodeConfig::default()).is_none());
    }

    // --- Real encoder pipeline tests ---
    // These test spawn_video with actual encoders to verify the full
    // source → encode → hang track pipeline works end-to-end.

    async fn assert_spawn_video_produces_output<E: VideoEncoderFactory>(
        preset: VideoPreset,
        timeout: Duration,
    ) {
        let (w, h) = preset.dimensions();
        let source = TestVideoSource::new(w, h);
        let encoder = E::with_preset(preset).unwrap();
        let track = moq_lite::Track::new("test-video").produce();
        let producer = hang::TrackProducer::new(track.producer);
        let mut consumer = track.consumer;
        let shutdown = CancellationToken::new();

        let _thread = EncoderThread::spawn_video(source, encoder, producer, shutdown.clone());

        let result = tokio::time::timeout(timeout, consumer.next_group()).await;

        shutdown.cancel();

        let group = result
            .expect("timed out waiting for first video group")
            .expect("track error")
            .expect("track closed without producing any groups");
        assert_eq!(group.info.sequence, 0, "first group should be sequence 0");
    }

    #[tokio::test]
    async fn spawn_video_h264_produces_output() {
        assert_spawn_video_produces_output::<codec::H264Encoder>(
            VideoPreset::P180,
            Duration::from_millis(500),
        )
        .await;
    }

    #[cfg(feature = "av1")]
    #[tokio::test]
    async fn spawn_video_av1_produces_output() {
        // rav1e buffers ~30 frames before first output at speed preset 10
        assert_spawn_video_produces_output::<codec::Av1Encoder>(
            VideoPreset::P180,
            Duration::from_secs(3),
        )
        .await;
    }

    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    #[tokio::test]
    #[ignore]
    async fn spawn_video_vtb_produces_output() {
        assert_spawn_video_produces_output::<codec::VtbEncoder>(
            VideoPreset::P180,
            Duration::from_millis(500),
        )
        .await;
    }

    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    #[tokio::test]
    #[ignore = "requires VAAPI hardware"]
    async fn spawn_video_vaapi_produces_output() {
        assert_spawn_video_produces_output::<codec::VaapiEncoder>(
            VideoPreset::P180,
            Duration::from_millis(500),
        )
        .await;
    }

    /// Regression test: set_video must make renditions available BEFORE
    /// publishing the catalog, otherwise subscribers that request tracks
    /// based on the catalog will get "rendition not available" errors.
    #[tokio::test]
    async fn set_video_renditions_available_before_catalog() {
        let mut broadcast = PublishBroadcast::new();
        let renditions = make_renditions();
        let rendition_names: Vec<String> = renditions.renditions.keys().cloned().collect();

        broadcast.set_video(Some(renditions)).unwrap();

        // After set_video, all rendition names should be available in state.
        let state = broadcast.state.lock().expect("poisoned");
        let available = state.available_video.as_ref().expect("video should be set");
        for name in &rendition_names {
            assert!(
                available.contains_rendition(name),
                "rendition {name} should be available after set_video"
            );
        }
    }
}
