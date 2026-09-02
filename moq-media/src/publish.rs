//! Publishing a broadcast: one source, one or more encoded renditions.
//!
//! [`LocalBroadcast`] owns a `moq_net` broadcast producer and the catalog that
//! describes it. Video goes through [`VideoPublisher`], audio through
//! [`AudioPublisher`]; both accept a source and a set of renditions and take
//! care of the rest.
//!
//! The one thing this adds over `moq_video::encode::publish_capture` is
//! simulcast. Upstream, one producer publishes one rendition and owns the
//! device it captures from. A subscriber that adapts to its downlink needs
//! several renditions of the same picture, so [`VideoPublisher`] opens the
//! source once and fans its frames out to an encoder per rendition, each
//! encoding only while someone is watching it.

use std::sync::{Arc, Mutex};

use moq_video::Size;
use n0_error::{Result, stack_error};
use n0_future::{StreamExt, boxed::BoxStream, task::AbortOnDropHandle};
use tracing::{Instrument, error_span, warn};

use crate::{
    catalog::{Catalog, Chat, IrohLiveExt, User},
    frame_channel::{FrameReceiver, frame_channel},
    stats::PublishStats,
};

mod video;

/// The catalog producer for an iroh-live broadcast.
type CatalogProducer = moq_mux::catalog::Producer<IrohLiveExt>;

/// Errors raised while publishing.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum PublishError {
    /// The transport rejected a track or broadcast operation.
    #[error(transparent)]
    Net(#[error(source, std_err)] moq_net::Error),
    /// The catalog could not be written.
    #[error(transparent)]
    Mux(#[error(source, std_err)] moq_mux::Error),
    /// A video encoder or capture device failed.
    #[error(transparent)]
    Video(#[error(source, std_err)] moq_video::Error),
    /// An audio encoder or capture device failed.
    #[error(transparent)]
    Audio(#[error(source, std_err)] moq_audio::Error),
    /// Two renditions were given the same name.
    #[error("duplicate rendition name: {name}")]
    DuplicateRendition {
        /// The name that appeared twice.
        name: String,
    },
    /// A rendition set was empty.
    #[error("no renditions given")]
    NoRenditions,
}

/// Where a video track's pictures come from.
///
/// `#[non_exhaustive]` so a new source kind stays additive.
#[non_exhaustive]
// The Capture config is the largest variant by a wide margin, and exactly one
// VideoSource exists per publish, so boxing it would trade a real allocation
// for a saving nobody measures.
#[allow(clippy::large_enum_variant, reason = "one per publish")]
pub enum VideoSource {
    /// A capture device: a camera, a display, or a window.
    #[cfg(feature = "capture")]
    Capture(moq_video::capture::Config),

    /// Frames the application produces, such as an Android app handing over
    /// Camera2 buffers or a test pattern generator.
    Frames(BoxStream<moq_video::Frame>),

    /// An Annex-B H.264 byte stream the source already encoded.
    ///
    /// The Raspberry Pi path: `rpicam-vid` encodes in hardware and we never see
    /// a raw picture. `moq_mux::codec::h264` splits the stream into access
    /// units and derives the catalog rendition from the first SPS, so nothing
    /// here has to describe the encoding it did not perform.
    AnnexB(BoxStream<bytes::Bytes>),
}

impl std::fmt::Debug for VideoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "capture")]
            Self::Capture(config) => f.debug_tuple("Capture").field(config).finish(),
            Self::Frames(_) => f.write_str("Frames"),
            Self::AnnexB(_) => f.write_str("AnnexB"),
        }
    }
}

#[cfg(feature = "capture")]
impl From<moq_video::capture::Config> for VideoSource {
    fn from(config: moq_video::capture::Config) -> Self {
        Self::Capture(config)
    }
}

/// One rung of a simulcast ladder.
///
/// [`size`](Self::size) is `None` for the source's own resolution, which is the
/// only sensible default for a single-rendition publish. Everything else falls
/// back to the encoder's defaults.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VideoRendition {
    /// The rendition name, as it appears in the catalog.
    pub name: String,
    /// The encoded resolution, or `None` for the source's own.
    pub size: Option<Size>,
    /// The target bitrate in bits per second, or `None` to derive one.
    pub bitrate: Option<u64>,
    /// Which codec to encode.
    pub codec: moq_video::encode::Codec,
    /// Which backend to encode with.
    pub kind: moq_video::encode::Kind,
}

impl VideoRendition {
    /// Creates a rendition at the source's own resolution.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: None,
            bitrate: None,
            codec: Default::default(),
            kind: Default::default(),
        }
    }

    /// Returns the rendition scaled to `size`.
    #[must_use]
    pub fn with_size(mut self, size: Size) -> Self {
        self.size = Some(size);
        self
    }

    /// Returns the rendition with a target bitrate in bits per second.
    #[must_use]
    pub fn with_bitrate(mut self, bitrate: u64) -> Self {
        self.bitrate = Some(bitrate);
        self
    }

    /// Returns the rendition encoded with `codec`.
    #[must_use]
    pub fn with_codec(mut self, codec: moq_video::encode::Codec) -> Self {
        self.codec = codec;
        self
    }

    /// Returns the rendition encoded by a specific backend.
    #[must_use]
    pub fn with_kind(mut self, kind: moq_video::encode::Kind) -> Self {
        self.kind = kind;
        self
    }
}

/// Where an audio track's samples come from.
#[non_exhaustive]
pub enum AudioSource {
    /// A capture device: a microphone, or the system mix on macOS.
    #[cfg(feature = "capture")]
    Device(moq_audio::capture::Config),
    /// PCM the application produces, such as a decoded file.
    Frames {
        /// The layout of the samples in each frame.
        input: moq_audio::encode::Input,
        /// Interleaved PCM, timestamped on the broadcast clock.
        frames: BoxStream<moq_audio::Frame>,
    },
}

impl std::fmt::Debug for AudioSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "capture")]
            Self::Device(config) => f.debug_tuple("Device").field(config).finish(),
            Self::Frames { input, .. } => f.debug_struct("Frames").field("input", input).finish(),
        }
    }
}

#[cfg(feature = "capture")]
impl From<moq_audio::capture::Config> for AudioSource {
    fn from(config: moq_audio::capture::Config) -> Self {
        Self::Device(config)
    }
}

/// A broadcast this node publishes.
///
/// Created from a `moq_net` broadcast producer, which for iroh-live comes from
/// [`iroh_moq::Moq::publish`](https://docs.rs/iroh-moq). Owns the catalog and
/// the per-medium publish tasks; dropping it ends them.
#[derive(derive_more::Debug)]
pub struct LocalBroadcast {
    #[debug(skip)]
    broadcast: moq_net::broadcast::Producer,
    #[debug(skip)]
    catalog: Mutex<CatalogProducer>,
    /// Held for a publish task's whole life, so a replacement waits for its
    /// predecessor's track producers to drop before creating its own. Without
    /// it, swapping a source races the old track name and `create_track`
    /// returns `Error::Duplicate` on a name that is about to be free.
    #[debug(skip)]
    video_slot: Arc<tokio::sync::Mutex<()>>,
    clock: moq_mux::Clock,
    stats: PublishStats,
    video: Mutex<Option<VideoPublish>>,
    audio: Mutex<Option<AudioPublish>>,
    preview: Mutex<Option<Arc<FrameReceiver<Arc<moq_video::Frame>>>>>,
}

/// A running video publish: the tasks driving it, plus the preview tap.
#[derive(Debug)]
struct VideoPublish {
    renditions: Vec<String>,
    _task: AbortOnDropHandle<()>,
}

/// A running audio publish.
#[derive(Debug)]
struct AudioPublish {
    rendition: String,
    _task: AudioTask,
}

impl LocalBroadcast {
    /// Creates a broadcast that publishes through `broadcast`.
    ///
    /// # Errors
    ///
    /// Fails if the catalog track cannot be created on the broadcast.
    pub fn new(mut broadcast: moq_net::broadcast::Producer) -> Result<Self, PublishError> {
        let catalog = CatalogProducer::with_catalog(&mut broadcast, Catalog::default())?;
        // Behind a lock so every mutator takes `&self`, which is what a UI loop
        // holding the broadcast needs. The publish tasks clone it out.
        Ok(Self {
            broadcast,
            catalog: Mutex::new(catalog),
            clock: moq_mux::Clock::new(),
            stats: PublishStats::default(),
            video_slot: Arc::new(tokio::sync::Mutex::new(())),
            video: Mutex::new(None),
            audio: Mutex::new(None),
            preview: Mutex::new(None),
        })
    }

    /// Returns a consumer for this broadcast, for in-process playback.
    pub fn consume(&self) -> moq_net::broadcast::Consumer {
        self.broadcast.consume()
    }

    /// Returns the clock both media tracks are stamped from.
    ///
    /// Audio and video share it so their timelines stay aligned even though the
    /// two devices open at different times.
    pub fn clock(&self) -> &moq_mux::Clock {
        &self.clock
    }

    /// Returns the publish-side counters, for a UI or a log.
    pub fn stats(&self) -> &PublishStats {
        &self.stats
    }

    /// Returns the video half of the broadcast.
    pub fn video(&self) -> VideoPublisher<'_> {
        VideoPublisher(self)
    }

    /// Returns the audio half of the broadcast.
    pub fn audio(&self) -> AudioPublisher<'_> {
        AudioPublisher(self)
    }

    /// Reports whether a video source is publishing.
    pub fn has_video(&self) -> bool {
        self.video.lock().expect("poisoned").is_some()
    }

    /// Reports whether an audio source is publishing.
    pub fn has_audio(&self) -> bool {
        self.audio.lock().expect("poisoned").is_some()
    }

    /// Creates a track on the broadcast and advertises it as the chat track.
    ///
    /// Chat itself is not a media concern: the caller wraps the returned
    /// producer in whatever message codec it uses. This only owns the catalog
    /// entry, so a subscriber can find the track by reading the catalog rather
    /// than guessing at a name.
    ///
    /// # Errors
    ///
    /// Fails if the track already exists or the catalog cannot be updated.
    pub fn enable_chat(
        &self,
        track: crate::catalog::TrackRef,
    ) -> Result<moq_net::track::Producer, PublishError> {
        let mut info = moq_net::track::Info::default();
        info.priority = track.priority;
        // Chat only makes sense read oldest first, where the moq-net default
        // favours the newest group.
        info.ordered = true;
        // A cloned producer shares the broadcast's track table, so this creates
        // the track on the same broadcast without needing `&mut self`.
        let producer = self
            .broadcast
            .clone()
            .create_track(track.name.as_str(), Some(info))?;
        let mut catalog = self.catalog.lock().expect("poisoned");
        let mut guard = catalog.lock();
        guard.ext.chat = Some(Chat {
            message: Some(track),
            typing: None,
        });
        guard.commit()?;
        Ok(producer)
    }

    /// Advertises the publisher's identity in the catalog.
    ///
    /// # Errors
    ///
    /// Fails if the catalog cannot be updated.
    pub fn set_user(&self, user: User) -> Result<(), PublishError> {
        let mut catalog = self.catalog.lock().expect("poisoned");
        let mut guard = catalog.lock();
        guard.ext.user = Some(user);
        guard.commit()?;
        Ok(())
    }

    /// Returns a receiver for the raw frames the video source produces, before
    /// they are encoded.
    ///
    /// This is the local preview: it costs no extra decode, because the frames
    /// are the ones already on their way to the encoders. Returns `None` when
    /// no video source is publishing, or when the source is already encoded.
    pub fn preview(&self) -> Option<Arc<FrameReceiver<Arc<moq_video::Frame>>>> {
        self.preview.lock().expect("poisoned").clone()
    }

    /// Returns the broadcast producer, for callers that publish their own
    /// tracks alongside the media ones.
    pub fn producer(&self) -> &moq_net::broadcast::Producer {
        &self.broadcast
    }

    /// Ends the broadcast so subscribers see a clean close.
    pub fn finish(mut self) {
        self.video.lock().expect("poisoned").take();
        self.audio.lock().expect("poisoned").take();
        self.broadcast.finish();
    }
}

/// The video half of a [`LocalBroadcast`].
#[derive(Debug)]
pub struct VideoPublisher<'a>(&'a LocalBroadcast);

impl VideoPublisher<'_> {
    /// Publishes `source` as a single rendition at its own resolution.
    ///
    /// # Errors
    ///
    /// See [`set_renditions`](Self::set_renditions).
    pub fn set(&self, source: impl Into<VideoSource>) -> Result<(), PublishError> {
        self.set_renditions(source, vec![VideoRendition::new("video")])
    }

    /// Publishes `source` as a simulcast ladder.
    ///
    /// Replaces whatever was publishing before. The source is opened once and
    /// its frames fan out to one encoder per rendition; an encoder runs only
    /// while at least one subscriber is watching its rendition.
    ///
    /// # Errors
    ///
    /// Fails if `renditions` is empty or has duplicate names. Everything after
    /// that happens inside the publish task, so a device that will not open or
    /// an encoder that fails surfaces in the log and ends the track rather than
    /// here.
    pub fn set_renditions(
        &self,
        source: impl Into<VideoSource>,
        renditions: Vec<VideoRendition>,
    ) -> Result<(), PublishError> {
        let source = source.into();
        if renditions.is_empty() {
            return Err(n0_error::e!(PublishError::NoRenditions));
        }
        check_unique(renditions.iter().map(|r| r.name.as_str()))?;

        let names: Vec<String> = renditions.iter().map(|r| r.name.clone()).collect();
        // A pre-encoded source has no raw frames to tap, so there is nothing to
        // preview and `preview()` says so rather than handing back a receiver
        // that never fills.
        let previewable = !matches!(source, VideoSource::AnnexB(_));
        let (preview_tx, preview_rx) = frame_channel::<Arc<moq_video::Frame>>();
        // Drop the previous publish before spawning the replacement, so its
        // abort is already in flight while the new task waits on the slot.
        self.0.video.lock().expect("poisoned").take();

        let task = video::spawn_publish(video::Publish {
            broadcast: self.0.broadcast.clone(),
            catalog: self.0.catalog.lock().expect("poisoned").clone(),
            clock: self.0.clock,
            stats: self.0.stats.clone(),
            slot: self.0.video_slot.clone(),
            source,
            renditions,
            preview: preview_tx,
        });

        *self.0.preview.lock().expect("poisoned") = previewable.then(|| Arc::new(preview_rx));
        *self.0.video.lock().expect("poisoned") = Some(VideoPublish {
            renditions: names,
            _task: task,
        });
        Ok(())
    }

    /// Stops publishing video.
    pub fn clear(&self) {
        self.0.video.lock().expect("poisoned").take();
        self.0.preview.lock().expect("poisoned").take();
    }

    /// Returns the names of the renditions currently publishing.
    pub fn renditions(&self) -> Vec<String> {
        self.0
            .video
            .lock()
            .expect("poisoned")
            .as_ref()
            .map(|track| track.renditions.clone())
            .unwrap_or_default()
    }
}

/// The audio half of a [`LocalBroadcast`].
#[derive(Debug)]
pub struct AudioPublisher<'a>(&'a LocalBroadcast);

impl AudioPublisher<'_> {
    /// Publishes `source` with the default encoder options.
    pub fn set(&self, source: impl Into<AudioSource>) {
        self.set_with(source, moq_audio::encode::Options::default());
    }

    /// Publishes `source` with explicit encoder options.
    ///
    /// Replaces whatever was publishing before. Unlike video there is no
    /// ladder: a subscriber adapts by dropping video renditions, never audio.
    ///
    /// Returns nothing because nothing can fail here: the device opens, the
    /// track is created, and the encoder starts inside the publish task, and a
    /// failure there is logged and ends the track. A caller that has to know
    /// watches the track rather than this call.
    pub fn set_with(&self, source: impl Into<AudioSource>, options: moq_audio::encode::Options) {
        let source = source.into();
        let rendition = options
            .track
            .clone()
            .unwrap_or_else(|| options.codec.to_string());
        let task = spawn_audio(
            self.0.broadcast.clone(),
            self.0.catalog.lock().expect("poisoned").clone(),
            self.0.clock,
            source,
            options,
        );
        *self.0.audio.lock().expect("poisoned") = Some(AudioPublish {
            rendition,
            _task: task,
        });
    }

    /// Stops publishing audio.
    pub fn clear(&self) {
        self.0.audio.lock().expect("poisoned").take();
    }

    /// Returns the name of the rendition currently publishing.
    pub fn rendition(&self) -> Option<String> {
        self.0
            .audio
            .lock()
            .expect("poisoned")
            .as_ref()
            .map(|track| track.rendition.clone())
    }
}

/// Drives an audio source until it ends or the track stops being watched.
fn spawn_audio(
    broadcast: moq_net::broadcast::Producer,
    catalog: CatalogProducer,
    #[cfg_attr(
        not(feature = "capture"),
        allow(
            unused_variables,
            reason = "only the device source stamps from the shared clock"
        )
    )]
    clock: moq_mux::Clock,
    source: AudioSource,
    options: moq_audio::encode::Options,
) -> AudioTask {
    match source {
        // A device publication owns its capture stream, and moq's native
        // backends are not all `Send`, so it gets a thread rather than a slot on
        // the shared executor. See `crate::local_task`.
        #[cfg(feature = "capture")]
        AudioSource::Device(config) => AudioTask::Local(crate::local_task::spawn(
            "audio-publish",
            move |shutdown| async move {
                let publish =
                    moq_audio::encode::publish_capture(broadcast, catalog, config, options, clock);
                let result = tokio::select! {
                    result = publish => result,
                    _ = shutdown.cancelled() => return,
                };
                if let Err(err) = result {
                    warn!(error = %err, "audio publish stopped");
                }
            },
        )),
        // Application-produced PCM crosses threads by definition, so it stays on
        // the runtime the caller is already using.
        AudioSource::Frames { input, frames } => {
            AudioTask::Shared(AbortOnDropHandle::new(n0_future::task::spawn(
                async move {
                    if let Err(err) =
                        publish_audio_frames(broadcast, catalog, input, frames, options).await
                    {
                        warn!(error = %err, "audio publish stopped");
                    }
                }
                .instrument(error_span!("audio-publish")),
            )))
        }
    }
}

/// Whichever executor an audio publication ended up on.
///
/// Dropping either stops the publication; they differ only in how. Neither is
/// ever read, which is the point: the handle exists so the publication lives
/// exactly as long as the track that owns it.
#[derive(derive_more::Debug)]
#[expect(
    dead_code,
    reason = "each variant is held for its Drop, which is what stops the publication"
)]
enum AudioTask {
    /// A device publication, on its own thread.
    #[debug("Local")]
    Local(crate::local_task::LocalTask),
    /// A frame publication, on the caller's runtime.
    #[debug("Shared")]
    Shared(AbortOnDropHandle<()>),
}

/// Publishes PCM the application produced, rather than a device's.
async fn publish_audio_frames(
    mut broadcast: moq_net::broadcast::Producer,
    catalog: CatalogProducer,
    input: moq_audio::encode::Input,
    mut frames: BoxStream<moq_audio::Frame>,
    options: moq_audio::encode::Options,
) -> Result<(), moq_audio::Error> {
    let mut producer = moq_audio::encode::Producer::new(&mut broadcast, catalog, input, &options)?;
    while let Some(frame) = frames.next().await {
        producer.write(&frame)?;
    }
    producer.finish()
}

/// Rejects a rendition set with a repeated name, which would otherwise collide
/// on one catalog entry and one track.
fn check_unique<'a>(names: impl Iterator<Item = &'a str>) -> Result<(), PublishError> {
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(n0_error::e!(PublishError::DuplicateRendition {
                name: name.to_string(),
            }));
        }
    }
    Ok(())
}
