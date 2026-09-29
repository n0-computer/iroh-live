//! Opened video and audio sources.
//!
//! A source is already open when the application gets it. Opening is async
//! and fails if the device does not open, so a missing camera is an error
//! where the application asked for it. Each device lives on its own thread,
//! because platform capture objects are often not `Send`. Only frames cross
//! threads.
//!
//! A source runs while any clone of it exists, including the clone a
//! [`LocalBroadcast`](crate::LocalBroadcast) holds. Any number of broadcasts
//! and previews can read it at once.

use std::{path::Path, sync::Arc};

use tokio_util::sync::CancellationToken;
#[cfg(feature = "capture")]
use tracing::info;
use tracing::{debug, warn};

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::rpicam::RpicamConfig;
pub use self::sender::FrameSender;
use self::sender::{Demand, DemandGuard, WeakPcmFanout};
#[cfg(feature = "capture")]
use crate::output::AudioOutput;
use crate::{
    audio,
    error::Error,
    frames::{FrameReader, FrameSlot, VideoFrames},
    video,
};

#[cfg(feature = "capture")]
mod capture;
mod file;
mod generator;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
mod rpicam;
mod sender;

/// How many PCM frames a broadcast may lag before it loses the oldest.
///
/// Sources deliver 10 to 60 ms per frame, so this is one to six seconds of
/// audio.
const PCM_BUFFER: usize = 100;

/// The size and cadence of a raw video source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    /// The picture size.
    pub size: video::Size,
    /// The frame rate.
    pub rate: video::Rate,
}

/// The sample rate and speaker layout of a raw audio source.
///
/// Samples are interleaved 32-bit floats in `-1.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// The channels and their order.
    pub layout: audio::Layout,
}

/// What keeps a source's producer running, dropped with the last handle.
#[derive(Debug)]
enum Driver {
    /// A thread that watches the stop token.
    Thread,
    /// A task on a thread of its own, for capture objects that cannot move.
    #[cfg(any(feature = "capture", all(target_os = "linux", feature = "rpicam")))]
    Local {
        /// Stops the task when dropped.
        _task: crate::local_task::LocalTask,
    },
    /// Nothing: the application pushes.
    Pushed,
}

/// The shared half of a [`VideoSource`].
#[derive(Debug)]
struct VideoInner {
    /// What kind of source this is, for logs and status.
    kind: String,
    format: VideoFormat,
    frames: FrameReader,
    demand: Demand,
    /// Cancelled when the last handle drops, to stop the producer.
    stop: CancellationToken,
    _driver: Driver,
}

impl Drop for VideoInner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// A raw video source running on its own thread.
///
/// Capture runs while any clone exists, including the clone a broadcast holds.
/// Encoding runs only while something wants the frames. Cheap to clone.
#[derive(Debug, Clone)]
pub struct VideoSource {
    inner: Arc<VideoInner>,
}

impl VideoSource {
    fn new(
        kind: impl Into<String>,
        format: VideoFormat,
        frames: FrameReader,
        stop: CancellationToken,
        driver: Driver,
    ) -> Self {
        Self {
            inner: Arc::new(VideoInner {
                kind: kind.into(),
                format,
                frames,
                demand: Demand::default(),
                stop,
                _driver: driver,
            }),
        }
    }

    /// Opens the camera, display or window that `config.source` names.
    ///
    /// Returns once the device has produced a frame. A device that produces
    /// none within 30 seconds fails. A screen capture may wait that long on a
    /// permission dialog.
    ///
    /// Cancellation safe: dropping the future stops the thread and releases
    /// the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Device`] if the device does not open or produces no
    /// frame.
    #[cfg(feature = "capture")]
    pub async fn capture(config: video::capture::Config) -> Result<Self, Error> {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let (format, task) = capture::open(config, slot, stop.clone()).await?;
        Ok(Self::new(
            "capture",
            format,
            reader,
            stop,
            Driver::Local { _task: task },
        ))
    }

    /// Returns a generated test pattern at `size` and `rate`.
    ///
    /// The pattern shows a sweeping bar, a frame counter and a clock. A marker
    /// flashes in step with the beep of [`AudioSource::test_pattern`].
    pub fn test_pattern(size: video::Size, rate: video::Rate) -> Self {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("test-pattern".into())
            .spawn(move || generator::run_pattern(size, rate, slot, token));
        if let Err(err) = spawned {
            // The slot closes with the unstarted thread, so the source reads
            // as ended.
            warn!(error = %err, "the test pattern thread did not start");
        }
        Self::new(
            "test-pattern",
            VideoFormat { size, rate },
            reader,
            stop,
            Driver::Thread,
        )
    }

    /// Returns a source fed by the returned sender.
    ///
    /// The source ends when every sender is dropped. `format` describes what
    /// the sender will push. The broadcast encodes at the size of the frames
    /// that arrive.
    pub fn push(format: VideoFormat) -> (FrameSender<video::Frame>, Self) {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let source = Self::new("push", format, reader, stop.clone(), Driver::Pushed);
        let sender = FrameSender::new(Arc::new(slot), stop, source.inner.demand.clone());
        (sender, source)
    }

    /// Runs `run` on a dedicated thread that feeds the source.
    ///
    /// Use it for thread-bound platform objects. The thread has a
    /// current-thread Tokio runtime entered, so `run` can drive async code
    /// with `tokio::runtime::Handle::current().block_on`. The source ends once
    /// `run` returns and every clone of its sender is dropped. If `run` fails,
    /// the source fails with its error. `run` should return once
    /// [`FrameSender::push`] reports [`Closed`](crate::Closed).
    ///
    /// # Errors
    ///
    /// Fails if the thread or its runtime cannot be started.
    pub fn spawn<F>(name: &str, format: VideoFormat, run: F) -> Result<Self, Error>
    where
        F: FnOnce(FrameSender<video::Frame>) -> Result<(), Error> + Send + 'static,
    {
        let slot = FrameSlot::new();
        let stop = CancellationToken::new();
        let source = Self::new(name, format, slot.reader(), stop.clone(), Driver::Thread);
        let sender = FrameSender::new(Arc::new(slot.clone()), stop, source.inner.demand.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let thread_name = name.to_string();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let _entered = runtime.enter();
                match run(sender) {
                    Ok(()) => debug!(source = %thread_name, "video source ended"),
                    Err(err) => {
                        warn!(source = %thread_name, error = %format!("{err:#}"), "video source failed");
                        slot.fail(Arc::new(err));
                    }
                }
            })?;
        Ok(source)
    }

    /// Starts the Raspberry Pi camera through `rpicam-vid`, for raw pictures.
    ///
    /// The width is rounded up to one libcamera writes without row padding, so
    /// [`format`](Self::format) may be a few columns wider than asked for. For
    /// the camera's hardware H.264, use [`EncodedVideoSource::rpicam`].
    ///
    /// Cancellation safe: dropping the future kills the subprocess.
    ///
    /// # Errors
    ///
    /// Fails if `rpicam-vid` is not installed or cannot open the camera.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    pub async fn rpicam(config: RpicamConfig) -> Result<Self, Error> {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let (format, task) = rpicam::open_raw(config, slot, stop.clone()).await?;
        Ok(Self::new(
            "rpicam",
            format,
            reader,
            stop,
            Driver::Local { _task: task },
        ))
    }

    /// Returns the captured frames, for a local preview or a QR scanner.
    ///
    /// Every call returns a handle onto the same stream. Reading it does not
    /// encode and does not count as demand.
    pub fn frames(&self) -> VideoFrames {
        self.inner.frames.frames()
    }

    /// Returns the format the source opened at.
    ///
    /// It may differ from what the config asked for.
    pub fn format(&self) -> VideoFormat {
        self.inner.format
    }

    /// Registers a consumer that wants frames, for [`FrameSender::demand`].
    pub(crate) fn want(&self) -> DemandGuard {
        self.inner.demand.acquire()
    }

    /// Returns why the source stopped, if it failed.
    pub(crate) fn failure(&self) -> Option<Arc<Error>> {
        self.inner.frames.failure()
    }

    /// Returns the kind of source, for logs.
    pub(crate) fn kind(&self) -> &str {
        &self.inner.kind
    }
}

/// A source of pre-encoded H.264 that skips the encoders.
///
/// The catalog rendition is derived from the stream's first SPS. Not `Clone`,
/// because a byte stream has one reader.
#[derive(derive_more::Debug)]
pub struct EncodedVideoSource {
    #[debug(skip)]
    pub(crate) bytes: n0_future::boxed::BoxStream<bytes::Bytes>,
    /// Keeps whatever produces the bytes alive, such as a subprocess.
    #[debug(skip)]
    pub(crate) _guard: Option<Box<dyn std::any::Any + Send>>,
}

impl EncodedVideoSource {
    /// Wraps an Annex-B H.264 byte stream.
    ///
    /// The stream may split anywhere, since access units are found by their
    /// start codes. It should repeat its parameter sets before every keyframe,
    /// or a late subscriber cannot start decoding.
    pub fn annex_b(bytes: impl n0_future::Stream<Item = bytes::Bytes> + Send + 'static) -> Self {
        Self {
            bytes: Box::pin(bytes),
            _guard: None,
        }
    }

    /// Starts the Raspberry Pi camera's hardware H.264 through `rpicam-vid`.
    ///
    /// This is the cheapest thing a Pi Zero can publish: no raw pipe and no
    /// second encode. Dropping the source kills the subprocess.
    ///
    /// Cancellation safe: the future resolves at its first poll.
    ///
    /// # Errors
    ///
    /// Fails if `rpicam-vid` is not installed or cannot be started.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    #[allow(
        clippy::unused_async,
        reason = "starting a camera is async in every other source"
    )]
    pub async fn rpicam(config: RpicamConfig) -> Result<Self, Error> {
        Ok(Self::annex_b(rpicam::open_encoded(config)?))
    }
}

/// The microphone to open, and the output to cancel from it.
#[cfg(feature = "capture")]
#[derive(Debug, Clone, Default)]
pub struct MicrophoneConfig {
    /// The device and its capture settings.
    pub capture: audio::capture::Config,
    /// The output whose signal is removed from the microphone.
    ///
    /// `None` turns echo cancellation off, and a handset on speakerphone then
    /// sends its own output back to the peer. Needs the `aec` feature:
    /// [`AudioSource::microphone`] refuses the config without it. An output
    /// feeds one canceller at a time, so the canceller is built when a
    /// broadcast starts publishing the microphone, after the publication it
    /// replaces has released its own.
    pub echo_reference: Option<AudioOutput>,
}

#[cfg(feature = "capture")]
impl MicrophoneConfig {
    /// Returns the config with the microphone set to `device`.
    ///
    /// `device` is an id as `audio::capture::devices` reports it.
    #[must_use]
    pub fn with_device(mut self, device: impl Into<String>) -> Self {
        self.capture.source = audio::capture::Source::Microphone(Some(device.into()));
        self
    }

    /// Checks that echo cancellation, if asked for, is compiled in.
    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.echo_reference.is_some() && !cfg!(feature = "aec") {
            return Err(Error::invalid(
                "echo cancellation needs the `aec` feature, which this build was \
                 compiled without",
            ));
        }
        Ok(())
    }

    /// Returns the capture config with the echo canceller attached.
    ///
    /// Fails if echo cancellation is not compiled in, or if the output already
    /// feeds another canceller.
    pub(crate) fn resolve(&self) -> Result<audio::capture::Config, Error> {
        self.check()?;
        #[cfg_attr(not(feature = "aec"), allow(unused_mut, reason = "only aec attaches"))]
        let mut capture = self.capture.clone();
        #[cfg(feature = "aec")]
        if let Some(output) = &self.echo_reference {
            capture.aec = output.canceller()?;
        }
        Ok(capture)
    }
}

/// What an audio source produces.
#[derive(Debug)]
pub(crate) enum AudioKind {
    /// A microphone, opened by the publication that encodes it.
    ///
    /// The publication also builds its echo canceller.
    #[cfg(feature = "capture")]
    Microphone(MicrophoneConfig),
    /// PCM, fanned out to every broadcast that reads it.
    ///
    /// The fan-out is weak, so the source ends when its producer drops the
    /// sender.
    Pcm {
        format: AudioFormat,
        fanout: WeakPcmFanout,
    },
}

/// The shared half of an [`AudioSource`].
#[derive(Debug)]
struct AudioInner {
    kind_name: &'static str,
    kind: AudioKind,
    /// Held while a broadcast publishes this source.
    demand: Demand,
    stop: CancellationToken,
    _driver: Driver,
}

impl Drop for AudioInner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// A raw audio source.
///
/// Runs while any clone exists, until its input ends: a file that does not
/// loop, or a push source whose senders are gone. Cheap to clone. Every
/// broadcast it feeds reads all of its samples.
#[derive(Debug, Clone)]
pub struct AudioSource {
    inner: Arc<AudioInner>,
}

impl AudioSource {
    fn new(
        kind_name: &'static str,
        kind: AudioKind,
        stop: CancellationToken,
        driver: Driver,
    ) -> Self {
        Self {
            inner: Arc::new(AudioInner {
                kind_name,
                kind,
                demand: Demand::default(),
                stop,
                _driver: driver,
            }),
        }
    }

    /// Registers a broadcast that publishes this source.
    pub(crate) fn want(&self) -> DemandGuard {
        self.inner.demand.acquire()
    }

    /// Creates a microphone source.
    ///
    /// This checks that the device exists and that echo cancellation, if asked
    /// for, is compiled in. The device itself opens when a broadcast first has
    /// a listener for it. A failure then shows in the broadcast's
    /// [`PublishStatus`](crate::PublishStatus). The same source set on two
    /// broadcasts captures the device twice.
    ///
    /// Cancellation safe: dropping the future only abandons the device query.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Device`] if no microphone matches the config, and
    /// [`Error::InvalidConfig`] if echo cancellation is asked for in a build
    /// without it.
    #[cfg(feature = "capture")]
    pub async fn microphone(config: MicrophoneConfig) -> Result<Self, Error> {
        config.check()?;
        if let audio::capture::Source::Microphone(wanted) = &config.capture.source {
            let devices = audio::capture::devices().await.map_err(Error::device)?;
            let found = match wanted {
                Some(id) => devices.iter().any(|device| &device.id == id),
                None => !devices.is_empty(),
            };
            if !found {
                return Err(Error::device_msg(match wanted {
                    Some(id) => format!("no microphone with the id {id}"),
                    None => "this machine has no microphone".to_string(),
                }));
            }
        }
        info!(source = ?config.capture.source, echo_cancellation = config.echo_reference.is_some(), "microphone ready");
        Ok(Self::microphone_unchecked(config))
    }

    /// Wraps a microphone config without looking for the device.
    #[cfg(feature = "capture")]
    pub(crate) fn microphone_unchecked(config: MicrophoneConfig) -> Self {
        Self::new(
            "microphone",
            AudioKind::Microphone(config),
            CancellationToken::new(),
            Driver::Pushed,
        )
    }

    /// Decodes an audio file in real time.
    ///
    /// Restarts at the beginning when `looping`, and ends with the file
    /// otherwise. Reads WAV and MP3. The file is probed before this returns,
    /// and decoding starts on its own thread.
    ///
    /// Cancellation safe: dropping the future stops the decode thread.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read, holds no audio track, or uses a codec
    /// this build cannot decode.
    pub async fn file(path: impl AsRef<Path>, looping: bool) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let (fanout, _) = tokio::sync::broadcast::channel(PCM_BUFFER);
        let weak = fanout.downgrade();
        let stop = CancellationToken::new();
        // Stops the decode thread if this future is dropped. A looping file
        // would otherwise decode for the rest of the process.
        let abandoned = stop.clone().drop_guard();
        let format = {
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || file::spawn(path, looping, fanout, stop))
                .await
                .map_err(|err| Error::device_msg(format!("the file reader failed: {err}")))??
        };
        abandoned.disarm();
        let kind = AudioKind::Pcm {
            format,
            fanout: weak,
        };
        Ok(Self::new("file", kind, stop, Driver::Thread))
    }

    /// Returns a steady sine tone at `hz`, at 48 kHz in `layout`.
    pub fn tone(hz: f32, layout: audio::Layout) -> Self {
        Self::generated("tone", f64::from(hz), layout, generator::Gate::Continuous)
    }

    /// Returns the beeping tone that goes with [`VideoSource::test_pattern`].
    ///
    /// It beeps for a tenth of a second every second, on the timeline the
    /// pattern's marker flashes on. A viewer can see and hear whether the two
    /// line up.
    pub fn test_pattern(layout: audio::Layout) -> Self {
        Self::generated("test-pattern", generator::BEEP_HZ, layout, generator::BEEP)
    }

    fn generated(
        name: &'static str,
        hz: f64,
        layout: audio::Layout,
        gate: generator::Gate,
    ) -> Self {
        let format = AudioFormat {
            sample_rate: generator::TONE_RATE,
            layout,
        };
        let (fanout, _) = tokio::sync::broadcast::channel(PCM_BUFFER);
        let weak = fanout.downgrade();
        let stop = CancellationToken::new();
        let spawned = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || generator::run_tone(hz, format, gate, fanout, stop))
        };
        if let Err(err) = spawned {
            warn!(error = %err, "the tone thread did not start");
        }
        Self::new(
            name,
            AudioKind::Pcm {
                format,
                fanout: weak,
            },
            stop,
            Driver::Thread,
        )
    }

    /// Returns a source fed by the returned sender.
    ///
    /// The source ends when every sender is dropped. Frames carry interleaved
    /// 32-bit float samples in `format`. A broadcast that falls more than a
    /// few seconds behind loses the oldest frames and counts them in its
    /// stats. The sender's demand is true while a broadcast publishes the
    /// source.
    pub fn push(format: AudioFormat) -> (FrameSender<audio::Frame>, Self) {
        let (fanout, _) = tokio::sync::broadcast::channel(PCM_BUFFER);
        let stop = CancellationToken::new();
        let source = Self::new(
            "push",
            AudioKind::Pcm {
                format,
                fanout: fanout.downgrade(),
            },
            stop.clone(),
            Driver::Pushed,
        );
        let sender = FrameSender::new(Arc::new(fanout), stop, source.inner.demand.clone());
        (sender, source)
    }

    /// Returns what the source produces.
    pub(crate) fn kind(&self) -> &AudioKind {
        &self.inner.kind
    }

    /// Returns the kind of source, for logs.
    pub(crate) fn kind_name(&self) -> &'static str {
        self.inner.kind_name
    }
}

/// How long an opened capture device may take over its first frame.
#[cfg(any(feature = "capture", all(target_os = "linux", feature = "rpicam")))]
pub(crate) const FIRST_FRAME_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn rate(fps: u32) -> video::Rate {
        video::Rate::new(fps, 1).expect("a valid rate")
    }

    #[tokio::test]
    async fn a_test_pattern_produces_frames_and_stops_with_its_source() {
        let source = VideoSource::test_pattern(video::Size::new(64, 48), rate(30));
        let mut frames = source.frames();
        let first = tokio::time::timeout(Duration::from_secs(5), frames.next())
            .await
            .expect("the pattern draws")
            .expect("the pattern runs");
        assert_eq!(first.size(), video::Size::new(64, 48));
        drop(source);
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            while frames.next().await.is_some() {}
        })
        .await;
        assert!(ended.is_ok(), "the pattern kept running without its source");
    }

    #[tokio::test]
    async fn a_pushed_source_ends_when_its_senders_go() {
        let format = VideoFormat {
            size: video::Size::new(2, 2),
            rate: rate(30),
        };
        let (sender, source) = VideoSource::push(format);
        let mut frames = source.frames();
        let surface = video::Surface::rgba(&[0; 16], format.size).expect("2x2");
        sender
            .push(video::Frame::new(
                surface,
                moq_net::Timestamp::from_micros(0).expect("0"),
            ))
            .expect("open");
        assert!(frames.next().await.is_some());
        drop(sender);
        assert!(frames.next().await.is_none());
    }

    #[tokio::test]
    async fn a_pushed_source_reports_demand_while_something_encodes() {
        let format = VideoFormat {
            size: video::Size::new(2, 2),
            rate: rate(30),
        };
        let (sender, source) = VideoSource::push(format);
        let demand = sender.demand();
        assert!(!*demand.borrow());
        let wanted = source.want();
        assert!(*demand.borrow());
        drop(wanted);
        assert!(!*demand.borrow());
        drop(source);
        assert!(sender.is_closed());
    }

    #[tokio::test]
    async fn a_spawned_source_that_fails_reports_why() {
        let format = VideoFormat {
            size: video::Size::new(2, 2),
            rate: rate(30),
        };
        let source = VideoSource::spawn("failing", format, |_sender| {
            Err(Error::device_msg("the camera caught fire"))
        })
        .expect("the thread starts");
        let mut frames = source.frames();
        assert!(frames.next().await.is_none());
        let failure = source.failure().expect("the failure is kept");
        assert!(matches!(*failure, Error::Device { .. }));
    }

    /// The microphone config builds its echo canceller from its output.
    ///
    /// It needs an output device, so it runs by hand. `publish::tests` covers
    /// the publication side without a device.
    #[cfg(all(feature = "aec", feature = "playback"))]
    #[tokio::test]
    #[ignore = "needs an audio output device"]
    async fn echo_cancellation_attaches_the_canceller() {
        use crate::output::AudioOutput;
        let output = AudioOutput::open(None)
            .await
            .expect("an audio output device");
        let capture = MicrophoneConfig {
            echo_reference: Some(output.clone()),
            ..MicrophoneConfig::default()
        }
        .resolve()
        .expect("the canceller builds");
        assert!(
            capture.aec.is_some(),
            "the microphone config carries no canceller"
        );
        let plain = MicrophoneConfig::default().resolve().expect("resolves");
        assert!(plain.aec.is_none());
    }

    #[cfg(all(feature = "capture", not(feature = "aec")))]
    #[test]
    fn echo_cancellation_without_the_feature_is_refused() {
        use crate::output::AudioOutput;
        let config = MicrophoneConfig {
            echo_reference: Some(AudioOutput::null().clone()),
            ..MicrophoneConfig::default()
        };
        assert!(matches!(config.resolve(), Err(Error::InvalidConfig { .. })));
    }
}
