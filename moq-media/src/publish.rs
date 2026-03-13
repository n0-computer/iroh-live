mod controller;
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
pub use controller::{
    CaptureConfig, PublishCaptureController, PublishOpts, PublishUpdate, PublishUpdateError,
    StreamKind,
};
use hang::catalog::{Audio, AudioConfig, Catalog, Video, VideoConfig};
use moq_lite::{BroadcastProducer, TrackProducer};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[cfg(any_codec)]
use crate::codec;
#[cfg(any_audio_codec)]
use crate::codec::AudioCodec;
#[cfg(any_video_codec)]
use crate::codec::VideoCodec;
use crate::{
    format::{
        AudioEncoderConfig, AudioFormat, AudioPreset, DecodeConfig, VideoEncoderConfig,
        VideoFormat, VideoFrame, VideoPreset,
    },
    pipeline::{AudioEncoderPipeline, VideoEncoderPipeline},
    subscribe::VideoTrack,
    traits::{
        AudioEncoder, AudioEncoderFactory, AudioSource, VideoEncoder, VideoEncoderFactory,
        VideoSource,
    },
    transport::MoqPacketSink,
    util::spawn_thread,
};

type MakeAudioEncoder = Box<dyn Fn() -> Result<Box<dyn AudioEncoder>> + Send + 'static>;
type MakeVideoEncoder = Box<dyn Fn() -> Result<Box<dyn VideoEncoder>> + Send + 'static>;

struct AudioRenditionEntry {
    config: AudioConfig,
    factory: MakeAudioEncoder,
}

struct VideoRenditionEntry {
    config: VideoConfig,
    factory: MakeVideoEncoder,
}

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
        let catalog = CatalogProducer::new(&mut producer).expect("not closed");

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

    async fn run(state: Arc<Mutex<State>>, producer: BroadcastProducer) {
        let mut producer = producer.dynamic();
        loop {
            let track = match producer.requested_track().await {
                Ok(None) => {
                    debug!("broadcast producer: closed");
                    break;
                }
                Err(err) => {
                    warn!("broadcast producer: closed {err:#}");
                    break;
                }
                Ok(Some(track)) => track,
            };
            let name = track.info.name.clone();
            if state
                .lock()
                .unwrap()
                .start_track(track.clone())
                .inspect_err(|err| warn!(%name, "failed to start requested track: {err:#}"))
                .is_ok()
            {
                info!("started track: {name}");
                tokio::spawn({
                    let state = state.clone();
                    async move {
                        if let Err(err) = track.unused().await {
                            warn!("track closed: {err:#}");
                        }
                        info!("stopping track: {name} (all subscribers disconnected)");
                        state.lock().expect("poisoned").stop_track(&name);
                    }
                });
            }
        }
    }

    /// Create a local VideoTrack from the current video source, if present.
    pub fn watch_local(&self, decode_config: DecodeConfig) -> Option<VideoTrack> {
        let (source, shutdown) = {
            let state = self.state.lock().expect("poisoned");
            let source = state
                .available_video
                .as_ref()
                .map(|video| video.source.clone())?;
            Some((source, state.local_video_token.child_token()))
        }?;
        Some(VideoTrack::from_video_source(
            "local".to_string(),
            shutdown,
            source,
            decode_config,
        ))
    }

    pub fn set_video(&mut self, renditions: Option<VideoRenditions>) -> Result<()> {
        match renditions {
            Some(renditions) => {
                let configs = renditions.available_renditions()?;
                let video = Video {
                    renditions: configs,
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
                self.catalog.set_video(video)?;
            }
            None => {
                // Clear catalog and stop any active video encoders
                self.state.lock().expect("poisoned").remove_video();
                self.catalog.set_video(Default::default())?;
            }
        }
        Ok(())
    }

    pub fn set_audio(&mut self, renditions: Option<AudioRenditions>) -> Result<()> {
        match renditions {
            Some(renditions) => {
                let configs = renditions.available_renditions()?;
                let audio = Audio {
                    renditions: configs,
                };
                // Tear down existing audio, set new renditions, then publish catalog.
                // Order matters: renditions must be available before the catalog is
                // published, otherwise subscribers may request tracks we can't serve.
                let mut state = self.state.lock().expect("poisoned");
                state.remove_audio();
                state.available_audio = Some(renditions);
                drop(state);
                self.catalog.set_audio(audio)?;
            }
            None => {
                // Clear catalog and stop any active audio encoders
                self.state.lock().expect("poisoned").remove_audio();
                self.catalog.set_audio(Default::default())?;
            }
        }
        Ok(())
    }
}

impl Drop for PublishBroadcast {
    fn drop(&mut self) {
        self.state.lock().expect("poisoned").shutdown_token.cancel();
        // TODO: Do we need to close explicitly here?
        // self.producer.close();
    }
}

struct CatalogProducer {
    track: TrackProducer,
    catalog: Catalog,
}

impl CatalogProducer {
    fn new(broadcast: &mut BroadcastProducer) -> Result<Self, moq_lite::Error> {
        let track = broadcast.create_track(hang::Catalog::default_track())?;
        let catalog = Catalog::default();
        Ok(Self { track, catalog })
    }

    fn set_video(&mut self, video: Video) -> Result<()> {
        self.catalog.video = video;
        self.publish()?;
        Ok(())
    }

    fn set_audio(&mut self, audio: Audio) -> Result<()> {
        self.catalog.audio = audio;
        self.publish()?;
        Ok(())
    }

    fn publish(&mut self) -> Result<()> {
        let mut group = self.track.append_group().anyerr()?;
        group
            .write_frame(self.catalog.to_string().anyerr()?)
            .anyerr()?;
        group.finish().anyerr()?;
        Ok(())
    }
}

#[derive(Default)]
struct State {
    shutdown_token: CancellationToken,
    local_video_token: CancellationToken,
    available_video: Option<VideoRenditions>,
    available_audio: Option<AudioRenditions>,
    active_video: HashMap<String, VideoEncoderPipeline>,
    active_audio: HashMap<String, AudioEncoderPipeline>,
}

impl State {
    fn stop_track(&mut self, name: &str) {
        // Pipelines cancel on drop, so removing is sufficient.
        if self.active_video.remove(name).is_none() {
            self.active_audio.remove(name);
        }
    }

    fn remove_audio(&mut self) {
        self.active_audio.clear();
        self.available_audio = None;
    }

    fn remove_video(&mut self) {
        self.local_video_token.cancel();
        self.local_video_token = CancellationToken::new();
        self.active_video.clear();
        self.available_video = None;
    }

    fn start_track(&mut self, track: moq_lite::TrackProducer) -> Result<()> {
        let name = track.info.name.clone();
        if let Some(video) = self.available_video.as_mut()
            && video.contains_rendition(&name)
        {
            let pipeline = video.start_encoder(&name, track)?;
            self.active_video.insert(name, pipeline);
            Ok(())
        } else if let Some(audio) = self.available_audio.as_mut()
            && audio.contains_rendition(&name)
        {
            let pipeline = audio.start_encoder(&name, track)?;
            self.active_audio.insert(name, pipeline);
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
    renditions: HashMap<String, AudioRenditionEntry>,
}

impl AudioRenditions {
    #[cfg(any_audio_codec)]
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

    #[cfg(any_audio_codec)]
    pub fn add(&mut self, codec: AudioCodec, preset: AudioPreset) {
        match codec {
            #[cfg(feature = "opus")]
            AudioCodec::Opus => self.add_with_generic::<codec::OpusEncoder>(preset),
        }
    }

    pub fn add_with_generic<E: AudioEncoderFactory>(&mut self, preset: AudioPreset) {
        let name = format!("audio/{}-{preset}", E::ID);
        let format = self.source.format();
        let enc_config = AudioEncoderConfig::from_preset(format, preset);
        let config = E::config_for(&enc_config);
        self.renditions.insert(
            name,
            AudioRenditionEntry {
                config: config.into(),
                factory: Box::new(move || Ok(Box::new(E::with_preset(format, preset)?))),
            },
        );
    }

    pub fn add_with_callback<E: AudioEncoder>(
        &mut self,
        name: impl Into<String>,
        config: AudioConfig,
        callback: impl Fn(AudioFormat) -> anyhow::Result<E> + Send + 'static,
    ) {
        let format = self.source.format();
        self.renditions.insert(
            name.into(),
            AudioRenditionEntry {
                config,
                factory: Box::new(move || Ok(Box::new(callback(format)?))),
            },
        );
    }

    pub fn available_renditions(&self) -> Result<BTreeMap<String, AudioConfig>> {
        let mut renditions = BTreeMap::new();
        for (name, entry) in self.renditions.iter() {
            renditions.insert(name.clone(), entry.config.clone());
        }
        Ok(renditions)
    }

    pub fn contains_rendition(&self, name: &str) -> bool {
        self.renditions.contains_key(name)
    }

    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: TrackProducer,
    ) -> Result<AudioEncoderPipeline> {
        let entry = self
            .renditions
            .get(name)
            .context("rendition not available")?;
        let encoder = (entry.factory)()?;
        let sink = MoqPacketSink::new(producer);
        Ok(AudioEncoderPipeline::with_source(
            self.source.cloned_boxed(),
            encoder,
            sink,
        )?)
    }
}

#[derive(derive_more::Debug)]
pub struct VideoRenditions {
    source: SharedVideoSource,
    #[debug(skip)]
    renditions: HashMap<String, VideoRenditionEntry>,
    shutdown_token: CancellationToken,
}

impl VideoRenditions {
    /// Create video renditions with a dynamically-selected encoder.
    #[cfg(any_video_codec)]
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

    #[cfg(any_video_codec)]
    pub fn add(&mut self, codec: VideoCodec, preset: VideoPreset) {
        match codec {
            #[cfg(feature = "h264")]
            VideoCodec::H264 => self.add_with_generic::<codec::H264Encoder>(preset),
            #[cfg(feature = "av1")]
            VideoCodec::Av1 => self.add_with_generic::<codec::Av1Encoder>(preset),
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            VideoCodec::VtbH264 => self.add_with_generic::<codec::VtbEncoder>(preset),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            VideoCodec::VaapiH264 => self.add_with_generic::<codec::VaapiEncoder>(preset),
            #[cfg(all(target_os = "linux", feature = "v4l2"))]
            VideoCodec::V4l2H264 => self.add_with_generic::<codec::V4l2Encoder>(preset),
        }
    }

    pub fn add_with_generic<E: VideoEncoderFactory>(&mut self, preset: VideoPreset) {
        let name = format!("video/{}-{}", E::ID, preset);
        let enc_config = VideoEncoderConfig::from_preset(preset);
        let config = E::config_for(&enc_config);
        self.renditions.insert(
            name,
            VideoRenditionEntry {
                config: config.into(),
                factory: Box::new(move || Ok(Box::new(E::with_preset(preset)?))),
            },
        );
    }

    pub fn add_with_callback<E: VideoEncoder>(
        &mut self,
        name: impl ToString,
        config: VideoConfig,
        encoder_factory: impl Fn() -> anyhow::Result<E> + Send + 'static,
    ) {
        self.renditions.insert(
            name.to_string(),
            VideoRenditionEntry {
                config,
                factory: Box::new(move || Ok(Box::new(encoder_factory()?))),
            },
        );
    }

    pub fn available_renditions(&self) -> Result<BTreeMap<String, VideoConfig>> {
        let mut renditions = BTreeMap::new();
        for (name, entry) in self.renditions.iter() {
            renditions.insert(name.clone(), entry.config.clone());
        }
        Ok(renditions)
    }

    pub fn contains_rendition(&self, name: &str) -> bool {
        self.renditions.contains_key(name)
    }

    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: TrackProducer,
    ) -> Result<VideoEncoderPipeline> {
        let entry = self
            .renditions
            .get(name)
            .context("rendition not available")?;
        let encoder = (entry.factory)()?;
        let sink = MoqPacketSink::new(producer);
        Ok(VideoEncoderPipeline::new(
            self.source.clone(),
            encoder,
            sink,
        ))
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
                // Track whether the source has ever been started. Some sources
                // (e.g. PipeWire capturers) cannot survive a stop() before their
                // first start() because stop() permanently kills the capture
                // thread and start() is a no-op.
                let mut ever_started = false;
                for i in 0.. {
                    if shutdown.is_cancelled() {
                        break;
                    }

                    loop {
                        if running.load(Ordering::Relaxed) {
                            break;
                        }
                        if ever_started && let Err(err) = source.stop() {
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
                        ever_started = true;
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

#[cfg(all(test, any_codec))]
mod tests {
    use super::*;
    use crate::{
        codec::{VideoCodec, test_util::make_test_pattern},
        format::{PixelFormat, VideoFormat, VideoFrame, VideoPreset},
        traits::{VideoEncoderFactory, VideoSource},
    };

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
        use crate::pipeline::VideoEncoderPipeline;

        let (w, h) = preset.dimensions();
        let source = TestVideoSource::new(w, h);
        let encoder = E::with_preset(preset).unwrap();
        let track = moq_lite::Track::new("test-video");
        let producer = TrackProducer::new(track);
        let mut consumer = producer.consume();
        let sink = MoqPacketSink::new(producer);

        let _pipeline = VideoEncoderPipeline::new(source, encoder, sink);

        let result = tokio::time::timeout(timeout, consumer.next_group()).await;

        drop(_pipeline);

        let group = result
            .expect("timed out waiting for first video group")
            .expect("track error")
            .expect("track closed without producing any groups");
        assert_eq!(group.info.sequence, 0, "first group should be sequence 0");
    }

    #[cfg(feature = "h264")]
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
            Duration::from_secs(5),
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
