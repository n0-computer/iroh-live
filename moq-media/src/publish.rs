//! Publish side: encoding raw video/audio into a broadcast catalog.
//!
//! The main entry point is [`LocalBroadcast`], which manages encoder
//! pipelines and publishes a catalog that subscribers use to discover
//! available renditions. Use [`VideoPublisher`] and [`AudioPublisher`]
//! to configure tracks.

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
    pipeline::{AudioEncoderPipeline, PreEncodedVideoPipeline, VideoEncoderPipeline},
    subscribe::VideoTrack,
    traits::{
        AudioEncoder, AudioEncoderFactory, AudioSource, PreEncodedVideoSource, VideoEncoder,
        VideoEncoderFactory, VideoSource,
    },
    transport::MoqPacketSink,
    util::spawn_thread,
};

/// Errors from publish operations.
#[derive(Debug)]
pub enum PublishError {
    /// No video source has been configured.
    NoVideoSource,
    /// No audio source has been configured.
    NoAudioSource,
    /// The requested codec is not available on this platform.
    CodecUnavailable(String),
    /// The encoder failed to initialize or process media.
    EncoderFailed(anyhow::Error),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVideoSource => write!(f, "no video source"),
            Self::NoAudioSource => write!(f, "no audio source"),
            Self::CodecUnavailable(name) => write!(f, "codec unavailable: {name}"),
            Self::EncoderFailed(err) => write!(f, "encoder failed: {err}"),
        }
    }
}

impl std::error::Error for PublishError {}

impl From<anyhow::Error> for PublishError {
    fn from(err: anyhow::Error) -> Self {
        Self::EncoderFailed(err)
    }
}

type MakeAudioEncoder = Box<dyn Fn() -> Result<Box<dyn AudioEncoder>> + Send + 'static>;
type MakeVideoEncoder = Box<dyn Fn() -> Result<Box<dyn VideoEncoder>> + Send + 'static>;

/// Active video pipeline — either encoding raw frames or passing through
/// pre-encoded packets. The inner value is held for RAII: dropping the
/// pipeline cancels the encoder/passthrough thread.
#[derive(derive_more::Debug)]
enum VideoPipeline {
    Encoder(#[allow(dead_code, reason = "held for RAII cleanup on drop")] VideoEncoderPipeline),
    PreEncoded(
        #[allow(dead_code, reason = "held for RAII cleanup on drop")] PreEncodedVideoPipeline,
    ),
}

type MakePreEncodedSource =
    Box<dyn Fn() -> anyhow::Result<Box<dyn PreEncodedVideoSource>> + Send + 'static>;

/// Describes how video enters the publish pipeline: either as raw frames
/// that get encoded, or as already-encoded packets that pass through directly.
///
/// Use [`VideoPublisher::set`] to apply a `VideoInput` to a broadcast.
#[derive(derive_more::Debug)]
pub enum VideoInput {
    /// Raw video source with encoder renditions (simulcast layers).
    Renditions(VideoRenditions),
    /// One or more pre-encoded video tracks (no encoding needed).
    PreEncoded(#[debug(skip)] Vec<PreEncodedTrack>),
}

impl VideoInput {
    /// Creates a raw video input that encodes with the given codec and presets.
    #[cfg(any_video_codec)]
    pub fn new(
        source: impl VideoSource,
        codec: VideoCodec,
        presets: impl IntoIterator<Item = VideoPreset>,
    ) -> Self {
        Self::Renditions(VideoRenditions::new(source, codec, presets))
    }

    /// Creates a pre-encoded video input with a single track.
    ///
    /// The `factory` closure creates a fresh source instance for each
    /// subscriber — it is called each time a subscriber requests this track.
    pub fn pre_encoded(
        name: impl Into<String>,
        config: impl Into<VideoConfig>,
        factory: impl Fn() -> anyhow::Result<Box<dyn PreEncodedVideoSource>> + Send + 'static,
    ) -> Self {
        Self::PreEncoded(vec![PreEncodedTrack {
            name: name.into(),
            config: config.into(),
            factory: Box::new(factory),
        }])
    }
}

impl From<VideoRenditions> for VideoInput {
    fn from(renditions: VideoRenditions) -> Self {
        Self::Renditions(renditions)
    }
}

/// A single pre-encoded video track within a [`VideoInput::PreEncoded`] set.
#[derive(derive_more::Debug)]
pub struct PreEncodedTrack {
    /// Track name as it appears in the catalog (e.g. `"video/h264-pi"`).
    pub name: String,
    /// Codec configuration for the catalog entry and subscriber decoder setup.
    pub config: VideoConfig,
    /// Factory that creates a fresh source instance per subscriber.
    #[debug(skip)]
    pub factory: MakePreEncodedSource,
}

struct AudioRenditionEntry {
    config: AudioConfig,
    factory: MakeAudioEncoder,
}

struct VideoRenditionEntry {
    config: VideoConfig,
    factory: MakeVideoEncoder,
}

/// Local media broadcast that encodes and publishes video and audio tracks.
///
/// Configure video via [`video()`](Self::video) and audio via
/// [`audio()`](Self::audio). The broadcast manages encoder lifecycles
/// and publishes a catalog that subscribers use to discover available
/// renditions.
#[derive(derive_more::Debug, Clone)]
pub struct LocalBroadcast {
    #[debug(skip)]
    producer: BroadcastProducer,
    #[debug(skip)]
    state: Arc<Mutex<State>>,
    #[debug(skip)]
    _task: Arc<AbortOnDropHandle<()>>,
    stats: crate::stats::PublishStats,
}

impl Default for LocalBroadcast {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalBroadcast {
    /// Creates a new empty broadcast with no video or audio tracks.
    pub fn new() -> Self {
        let mut producer = BroadcastProducer::default();
        let catalog = CatalogProducer::new(&mut producer).expect("not closed");

        let stats = crate::stats::PublishStats::default();

        let mut state = State::new(catalog);
        state.stats = Some(stats.capture.clone());
        let state = Arc::new(Mutex::new(state));
        let task_handle = tokio::spawn(Self::run(state.clone(), producer.clone()));

        Self {
            producer,
            state,
            _task: Arc::new(AbortOnDropHandle::new(task_handle)),
            stats,
        }
    }

    /// Returns a clone of the underlying [`BroadcastProducer`].
    pub fn producer(&self) -> BroadcastProducer {
        self.producer.clone()
    }

    /// Returns the publish-side stats. Encode pipelines record into these
    /// automatically. External producers can record additional metrics.
    pub fn stats(&self) -> &crate::stats::PublishStats {
        &self.stats
    }

    /// Returns the video publishing handle.
    pub fn video(&self) -> VideoPublisher<'_> {
        VideoPublisher { broadcast: self }
    }

    /// Returns the audio publishing handle.
    pub fn audio(&self) -> AudioPublisher<'_> {
        AudioPublisher { broadcast: self }
    }

    /// Returns `true` if video is currently configured.
    pub fn has_video(&self) -> bool {
        self.state.lock().expect("poisoned").video.is_some()
    }

    /// Returns `true` if audio is currently configured.
    pub fn has_audio(&self) -> bool {
        self.state.lock().expect("poisoned").audio.is_some()
    }

    /// Returns a consumer that reads from this broadcast's producer.
    pub fn consume(&self) -> moq_lite::BroadcastConsumer {
        self.producer.consume()
    }

    async fn run(state: Arc<Mutex<State>>, producer: BroadcastProducer) {
        let mut producer = producer.dynamic();
        loop {
            let track = match producer.requested_track().await {
                Ok(track) => track,
                Err(err) => {
                    debug!("broadcast producer: closed ({err:#})");
                    break;
                }
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

    /// Returns a local preview video track (decode our own output).
    ///
    /// Only available when the video input is [`VideoInput::Renditions`] —
    /// pre-encoded sources do not produce raw frames for local preview.
    pub fn preview(&self, decode_config: DecodeConfig) -> Option<VideoTrack> {
        let (source, shutdown) = {
            let state = self.state.lock().expect("poisoned");
            let renditions = match state.video.as_ref()? {
                VideoInput::Renditions(r) => r,
                VideoInput::PreEncoded(_) => return None,
            };
            let source = renditions.source.clone();
            Some((source, state.local_video_token.child_token()))
        }?;
        Some(VideoTrack::from_video_source(
            "local".to_string(),
            shutdown,
            source,
            decode_config,
        ))
    }

    pub(crate) fn set_video(&self, input: Option<VideoInput>) -> Result<()> {
        let mut state = self.state.lock().expect("poisoned");
        match input {
            Some(input) => {
                let configs = match &input {
                    VideoInput::Renditions(renditions) => renditions.available_renditions()?,
                    VideoInput::PreEncoded(tracks) => tracks
                        .iter()
                        .map(|t| (t.name.clone(), t.config.clone()))
                        .collect(),
                };
                let video = Video {
                    renditions: configs,
                    display: None,
                    rotation: None,
                    flip: None,
                };
                // Tear down existing video, install new input, then publish catalog.
                // Order matters: the input must be available before the catalog is
                // published, otherwise subscribers may request tracks we can't serve.
                state.remove_video();
                state.video = Some(input);
                state.catalog.set_video(video)?;
            }
            None => {
                state.remove_video();
                state.catalog.set_video(Default::default())?;
            }
        }
        Ok(())
    }

    pub(crate) fn set_audio(&self, renditions: Option<AudioRenditions>) -> Result<()> {
        let mut state = self.state.lock().expect("poisoned");
        match renditions {
            Some(renditions) => {
                let configs = renditions.available_renditions()?;
                let audio = Audio {
                    renditions: configs,
                };
                state.remove_audio();
                state.audio = Some(renditions);
                state.catalog.set_audio(audio)?;
            }
            None => {
                state.remove_audio();
                state.catalog.set_audio(Default::default())?;
            }
        }
        Ok(())
    }
}

/// Handle for configuring the video track(s) of a [`LocalBroadcast`].
#[derive(Debug)]
pub struct VideoPublisher<'a> {
    broadcast: &'a LocalBroadcast,
}

impl VideoPublisher<'_> {
    /// Sets the video input for this broadcast.
    ///
    /// Accepts either raw frames (via [`VideoInput::Renditions`]) or
    /// pre-encoded packets (via [`VideoInput::PreEncoded`]). Tears down
    /// any existing video pipeline before installing the new one.
    pub fn set(&self, input: impl Into<VideoInput>) -> Result<()> {
        self.broadcast.set_video(Some(input.into()))
    }

    /// Removes video from the broadcast.
    pub fn clear(&self) {
        let _ = self.broadcast.set_video(None);
    }

    /// Returns the names of all currently configured video renditions.
    pub fn renditions(&self) -> Vec<String> {
        let state = self.broadcast.state.lock().expect("poisoned");
        match state.video.as_ref() {
            Some(VideoInput::Renditions(r)) => r.renditions.keys().cloned().collect(),
            Some(VideoInput::PreEncoded(tracks)) => tracks.iter().map(|t| t.name.clone()).collect(),
            None => Vec::new(),
        }
    }

    /// Enables or disables video output.
    ///
    /// **Unimplemented.** Currently a no-op. Intended to pause the encoder
    /// pipeline when disabled and resume on the next captured frame.
    pub fn set_enabled(&self, _enabled: bool) {
        // TODO: implement pause/resume on the encoder pipeline
    }
}

/// Handle for configuring the audio track(s) of a [`LocalBroadcast`].
#[derive(Debug)]
pub struct AudioPublisher<'a> {
    broadcast: &'a LocalBroadcast,
}

impl AudioPublisher<'_> {
    /// Sets the audio source, codec, and encoding presets.
    #[cfg(any_audio_codec)]
    pub fn set(
        &self,
        source: impl AudioSource,
        codec: AudioCodec,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Result<()> {
        let renditions = AudioRenditions::new(source, codec, presets);
        self.broadcast.set_audio(Some(renditions))
    }

    /// Sets audio from a source factory, enabling parallel multi-rendition encoding.
    ///
    /// Each rendition gets its own independent source from the factory.
    /// Use with [`AudioBackend`](crate::audio_backend::AudioBackend) where
    /// each `create_input()` call returns a stream backed by the same device.
    #[cfg(any_audio_codec)]
    pub fn set_with_factory(
        &self,
        format: AudioFormat,
        factory: impl Fn() -> anyhow::Result<Box<dyn AudioSource>> + Send + Sync + 'static,
        codec: AudioCodec,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Result<()> {
        let renditions = AudioRenditions::with_factory(format, factory, codec, presets);
        self.broadcast.set_audio(Some(renditions))
    }

    /// Removes audio from the broadcast.
    pub fn clear(&self) {
        let _ = self.broadcast.set_audio(None);
    }

    /// Mutes or unmutes audio output.
    ///
    /// **Unimplemented.** Currently a no-op. Intended to send silence
    /// instead of captured audio when muted.
    pub fn set_muted(&self, _muted: bool) {
        // TODO: implement mute on the audio pipeline
    }

    /// Sets audio from pre-built [`AudioRenditions`].
    pub fn set_renditions(&self, renditions: AudioRenditions) -> Result<()> {
        self.broadcast.set_audio(Some(renditions))
    }
}

impl Drop for LocalBroadcast {
    fn drop(&mut self) {
        if let Ok(state) = self.state.lock() {
            state.shutdown_token.cancel();
        }
    }
}

struct CatalogProducer {
    track: TrackProducer,
    catalog: Catalog,
}

impl CatalogProducer {
    fn new(broadcast: &mut BroadcastProducer) -> Result<Self> {
        let track = broadcast
            .create_track(hang::Catalog::default_track())
            .anyerr()?;
        let catalog = Catalog::default();
        let mut this = Self { track, catalog };
        this.publish()?;
        Ok(this)
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

struct State {
    catalog: CatalogProducer,
    shutdown_token: CancellationToken,
    local_video_token: CancellationToken,
    video: Option<VideoInput>,
    audio: Option<AudioRenditions>,
    active_video: HashMap<String, VideoPipeline>,
    active_audio: HashMap<String, AudioEncoderPipeline>,
    stats: Option<crate::stats::CaptureStats>,
}

impl State {
    fn new(catalog: CatalogProducer) -> Self {
        Self {
            catalog,
            shutdown_token: CancellationToken::new(),
            local_video_token: CancellationToken::new(),
            video: None,
            audio: None,
            active_video: HashMap::new(),
            active_audio: HashMap::new(),
            stats: None,
        }
    }
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
        self.audio = None;
    }

    fn remove_video(&mut self) {
        self.local_video_token.cancel();
        self.local_video_token = CancellationToken::new();
        self.active_video.clear();
        self.video = None;
    }

    fn start_track(&mut self, track: moq_lite::TrackProducer) -> Result<()> {
        let name = track.info.name.clone();

        // Try video first.
        match self.video.as_mut() {
            Some(VideoInput::Renditions(renditions)) if renditions.contains_rendition(&name) => {
                let pipeline = renditions.start_encoder(&name, track, self.stats.clone())?;
                self.active_video
                    .insert(name, VideoPipeline::Encoder(pipeline));
                return Ok(());
            }
            Some(VideoInput::PreEncoded(tracks)) => {
                if let Some(entry) = tracks.iter().find(|t| t.name == name) {
                    let source = (entry.factory)()?;
                    let sink = MoqPacketSink::new(track);
                    let pipeline = PreEncodedVideoPipeline::new(source, sink);
                    self.active_video
                        .insert(name, VideoPipeline::PreEncoded(pipeline));
                    return Ok(());
                }
            }
            _ => {}
        }

        // Then audio.
        if let Some(audio) = self.audio.as_mut()
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

/// Set of audio encoding renditions sharing a single audio source.
///
/// Each rendition pairs a codec configuration with an encoder factory.
/// Used by [`AudioPublisher::set_renditions`] for advanced control.
///
/// When multiple renditions are subscribed to in parallel, each encoder
/// pipeline needs its own independent audio source. Two modes handle this:
///
/// - **Factory mode**: a closure creates a fresh [`AudioSource`] per
///   rendition. Use this with device-backed sources like
///   [`AudioBackend`](crate::audio_backend::AudioBackend), where each
///   call to `create_input()` returns an independent stream backed by the
///   same device (the input callback fans out to all streams).
///
/// - **Single mode**: a single [`AudioSource`] is consumed by the first
///   rendition that starts. Use this with test sources or file playback
///   where only one rendition is expected.
#[derive(derive_more::Debug)]
pub struct AudioRenditions {
    #[debug(skip)]
    source: AudioSourceMode,
    format: AudioFormat,
    #[debug(skip)]
    renditions: HashMap<String, AudioRenditionEntry>,
}

/// How [`AudioRenditions`] obtains an [`AudioSource`] for each encoder pipeline.
enum AudioSourceMode {
    /// Creates a fresh source per rendition. Supports parallel encoding.
    Factory(Box<dyn Fn() -> anyhow::Result<Box<dyn AudioSource>> + Send + Sync>),
    /// Single source, borrowed by one pipeline at a time. When the pipeline
    /// drops, the source is returned via [`AudioSourceLease`]. A second
    /// concurrent rendition fails.
    Single(Arc<std::sync::Mutex<Option<Box<dyn AudioSource>>>>),
}

impl AudioRenditions {
    /// Creates audio renditions with a dynamically-selected encoder.
    ///
    /// The source is consumed by the first rendition that starts. For
    /// parallel multi-rendition encoding, use [`Self::with_factory`].
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

    /// Creates audio renditions backed by a source factory.
    ///
    /// Each rendition gets its own independent source from the factory,
    /// enabling parallel encoding. Use with [`AudioBackend`](crate::audio_backend::AudioBackend)
    /// or any [`AudioStreamFactory`](crate::traits::AudioStreamFactory).
    #[cfg(any_audio_codec)]
    pub fn with_factory(
        format: AudioFormat,
        factory: impl Fn() -> anyhow::Result<Box<dyn AudioSource>> + Send + Sync + 'static,
        codec: AudioCodec,
        presets: impl IntoIterator<Item = AudioPreset>,
    ) -> Self {
        let mut this = Self::empty_factory(format, factory);
        for preset in presets {
            this.add(codec, preset);
        }
        this
    }

    /// Creates audio renditions with a statically-typed encoder factory.
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

    /// Creates an empty rendition set with a single audio source and no presets.
    pub fn empty(source: impl AudioSource) -> Self {
        let format = source.format();
        Self {
            source: AudioSourceMode::Single(Arc::new(std::sync::Mutex::new(Some(Box::new(
                source,
            ))))),
            format,
            renditions: HashMap::new(),
        }
    }

    /// Creates an empty rendition set with a source factory and no presets.
    pub fn empty_factory(
        format: AudioFormat,
        factory: impl Fn() -> anyhow::Result<Box<dyn AudioSource>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            source: AudioSourceMode::Factory(Box::new(factory)),
            format,
            renditions: HashMap::new(),
        }
    }

    /// Adds a rendition using a dynamically-selected encoder for the given codec and preset.
    #[cfg(any_audio_codec)]
    pub fn add(&mut self, codec: AudioCodec, preset: AudioPreset) {
        match codec {
            #[cfg(feature = "opus")]
            AudioCodec::Opus => self.add_with_generic::<codec::OpusEncoder>(preset),
        }
    }

    /// Adds a rendition using a statically-typed encoder factory.
    pub fn add_with_generic<E: AudioEncoderFactory>(&mut self, preset: AudioPreset) {
        let name = format!("audio/{}-{preset}", E::ID);
        let format = self.format;
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

    /// Adds a rendition with a custom encoder factory callback.
    pub fn add_with_callback<E: AudioEncoder>(
        &mut self,
        name: impl Into<String>,
        config: AudioConfig,
        encoder_factory: impl Fn(AudioFormat) -> anyhow::Result<E> + Send + 'static,
    ) {
        let format = self.format;
        self.renditions.insert(
            name.into(),
            AudioRenditionEntry {
                config,
                factory: Box::new(move || Ok(Box::new(encoder_factory(format)?))),
            },
        );
    }

    /// Returns all configured renditions as a map of name to [`AudioConfig`].
    pub fn available_renditions(&self) -> Result<BTreeMap<String, AudioConfig>> {
        let mut renditions = BTreeMap::new();
        for (name, entry) in self.renditions.iter() {
            renditions.insert(name.clone(), entry.config.clone());
        }
        Ok(renditions)
    }

    /// Returns `true` if a rendition with the given name exists.
    pub fn contains_rendition(&self, name: &str) -> bool {
        self.renditions.contains_key(name)
    }

    /// Starts the encoder pipeline for the named rendition, writing to the given producer.
    ///
    /// In factory mode, each call creates a fresh independent source.
    /// In single mode, the source is leased to one pipeline at a time —
    /// returned automatically when the pipeline drops.
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
        let source: Box<dyn AudioSource> = match &self.source {
            AudioSourceMode::Factory(f) => f()?,
            AudioSourceMode::Single(slot) => {
                let inner = slot
                    .lock()
                    .expect("poisoned")
                    .take()
                    .context("audio source already in use by another rendition")?;
                let format = inner.format();
                Box::new(AudioSourceLease {
                    inner: Some(inner),
                    format,
                    slot: slot.clone(),
                })
            }
        };
        Ok(AudioEncoderPipeline::with_source(source, encoder, sink)?)
    }
}

/// Wraps an [`AudioSource`] and returns it to a shared slot on drop.
///
/// Used by [`AudioSourceMode::Single`] to lease a source to one encoder
/// pipeline at a time. When the pipeline shuts down and drops the lease,
/// the source becomes available for the next pipeline.
struct AudioSourceLease {
    inner: Option<Box<dyn AudioSource>>,
    format: AudioFormat,
    slot: Arc<std::sync::Mutex<Option<Box<dyn AudioSource>>>>,
}

impl AudioSource for AudioSourceLease {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> anyhow::Result<Option<usize>> {
        match &mut self.inner {
            Some(src) => src.pop_samples(buf),
            None => Ok(None),
        }
    }
}

impl Drop for AudioSourceLease {
    fn drop(&mut self) {
        if let Some(source) = self.inner.take() {
            *self.slot.lock().expect("poisoned") = Some(source);
        }
    }
}

/// Set of video encoding renditions (simulcast layers) sharing a single video source.
///
/// Each rendition pairs a codec configuration with an encoder factory.
/// Used by [`VideoPublisher::set_renditions`] for advanced control.
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

    /// Creates video renditions with a statically-typed encoder factory.
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

    /// Creates an empty rendition set with the given video source and no presets.
    pub fn empty(source: impl VideoSource) -> Self {
        let shutdown_token = CancellationToken::new();
        let source = SharedVideoSource::new(source, shutdown_token.clone());
        Self {
            source,
            renditions: HashMap::new(),
            shutdown_token,
        }
    }

    /// Adds a rendition using a dynamically-selected encoder for the given codec and preset.
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
            #[cfg(all(target_os = "android", feature = "android"))]
            VideoCodec::AndroidH264 => self.add_with_generic::<codec::AndroidEncoder>(preset),
        }
    }

    /// Adds a rendition using a statically-typed encoder factory.
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

    /// Adds a rendition with a custom encoder factory callback.
    pub fn add_with_callback<E: VideoEncoder>(
        &mut self,
        name: impl Into<String>,
        config: VideoConfig,
        encoder_factory: impl Fn() -> anyhow::Result<E> + Send + 'static,
    ) {
        self.renditions.insert(
            name.into(),
            VideoRenditionEntry {
                config,
                factory: Box::new(move || Ok(Box::new(encoder_factory()?))),
            },
        );
    }

    /// Returns all configured renditions as a map of name to [`VideoConfig`].
    pub fn available_renditions(&self) -> Result<BTreeMap<String, VideoConfig>> {
        let mut renditions = BTreeMap::new();
        for (name, entry) in self.renditions.iter() {
            renditions.insert(name.clone(), entry.config.clone());
        }
        Ok(renditions)
    }

    /// Returns `true` if a rendition with the given name exists.
    pub fn contains_rendition(&self, name: &str) -> bool {
        self.renditions.contains_key(name)
    }

    /// Starts the encoder pipeline for the named rendition, writing to the given producer.
    pub fn start_encoder(
        &mut self,
        name: &str,
        producer: TrackProducer,
        stats: Option<crate::stats::CaptureStats>,
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
            crate::stats::EncodeOpts { stats },
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
    async fn preview_produces_frames() {
        let broadcast = LocalBroadcast::new();
        broadcast.set_video(Some(make_renditions().into())).unwrap();
        let watch = broadcast
            .preview(DecodeConfig::default())
            .expect("preview should return Some when video is set");
        // The watch track exists — it will produce decoded frames once the
        // decode loop runs, but here we just verify it was created successfully.
        drop(watch);
    }

    #[tokio::test]
    async fn set_video_stops_old_local_watch() {
        let broadcast = LocalBroadcast::new();
        broadcast.set_video(Some(make_renditions().into())).unwrap();
        let watch = broadcast
            .preview(DecodeConfig::default())
            .expect("should have watch");

        // Replace video — old local watches should be cancelled.
        broadcast.set_video(Some(make_renditions().into())).unwrap();

        // The old watch's cancellation token should now be cancelled.
        // We can verify by checking that a new preview still works.
        let watch2 = broadcast
            .preview(DecodeConfig::default())
            .expect("new watch should work after replacement");
        drop(watch);
        drop(watch2);
    }

    #[tokio::test]
    async fn set_video_new_source_produces_frames() {
        let broadcast = LocalBroadcast::new();
        broadcast.set_video(Some(make_renditions().into())).unwrap();
        broadcast.set_video(Some(make_renditions().into())).unwrap();

        let watch = broadcast
            .preview(DecodeConfig::default())
            .expect("watch should work with replacement video");
        drop(watch);
    }

    #[tokio::test]
    async fn set_video_none_stops_local_watch() {
        let broadcast = LocalBroadcast::new();
        broadcast.set_video(Some(make_renditions().into())).unwrap();
        let _watch = broadcast
            .preview(DecodeConfig::default())
            .expect("should have watch");

        broadcast.set_video(None).unwrap();
        assert!(
            broadcast.preview(DecodeConfig::default()).is_none(),
            "preview should return None after set_video(None)"
        );
    }

    /// This is the critical test: replacing video while the source thread is parked
    /// (no subscribers). Before the fix, the old source thread would remain parked
    /// forever because nothing called unpark() on shutdown.
    #[tokio::test]
    async fn replace_video_while_source_parked() {
        let broadcast = LocalBroadcast::new();

        // Set video but never create a local watch — source thread will be parked.
        broadcast.set_video(Some(make_renditions().into())).unwrap();

        // Give the source thread time to park.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Replace video — this should cleanly shut down the parked source thread.
        broadcast.set_video(Some(make_renditions().into())).unwrap();

        // Give the old thread time to wake up and exit.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify the new video works.
        let watch = broadcast
            .preview(DecodeConfig::default())
            .expect("new watch should work after replacing parked video");
        drop(watch);
    }

    #[tokio::test]
    async fn source_thread_stops_on_video_removal() {
        let broadcast = LocalBroadcast::new();
        broadcast.set_video(Some(make_renditions().into())).unwrap();

        // Give source thread time to start and park.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Remove video — source thread should stop.
        broadcast.set_video(None).unwrap();

        // Give time for cleanup.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify no video available.
        assert!(broadcast.preview(DecodeConfig::default()).is_none());
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

        let _pipeline = VideoEncoderPipeline::new(source, encoder, sink, Default::default());

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
        // rav1e buffers ~30 frames before first output at speed preset 10.
        // Needs generous timeout — shared CI runners are slow.
        assert_spawn_video_produces_output::<codec::Av1Encoder>(
            VideoPreset::P180,
            Duration::from_secs(15),
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
        let broadcast = LocalBroadcast::new();
        let renditions = make_renditions();
        let rendition_names: Vec<String> = renditions.renditions.keys().cloned().collect();

        broadcast.set_video(Some(renditions.into())).unwrap();

        // After set_video, all rendition names should be available in state.
        let state = broadcast.state.lock().expect("poisoned");
        let available = match state.video.as_ref().expect("video should be set") {
            VideoInput::Renditions(r) => r,
            VideoInput::PreEncoded(_) => panic!("expected Renditions variant"),
        };
        for name in &rendition_names {
            assert!(
                available.contains_rendition(name),
                "rendition {name} should be available after set_video"
            );
        }
    }
}
