//! Opened sources: cameras, screens, microphones, files, generators, and
//! frames the application pushes.
//!
//! A source is a value that is already open. Opening a device is async and
//! fails if the device will not open, so "the camera does not exist" is an
//! error where the application asked for the camera rather than a log line
//! from a task that retries forever. The device lives on a thread of its own
//! for its whole life, which is what non-`Send` platform capture objects
//! require, and only frames cross.
//!
//! A source runs while any clone of it exists, including the clone a
//! [`LocalBroadcast`](crate::LocalBroadcast) holds, and any number of
//! broadcasts and previews read it at once.

use std::{fmt, path::Path, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::rpicam::RpicamConfig;
pub use self::sender::FrameSender;
use self::sender::{Demand, DemandGuard, PcmFanout};
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

/// How many PCM frames a slow broadcast may fall behind a source before it
/// loses the oldest.
///
/// Sources deliver 10 to 60 ms per frame, so this is between one and six
/// seconds of audio: far more than a broadcast that keeps up ever holds, and a
/// bound on what one that stalls can make the source keep.
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
/// Samples are interleaved 32-bit floats in `-1.0..=1.0`, which is what every
/// encoder takes without a conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// The channels and their order.
    pub layout: audio::Layout,
}

/// What keeps a source's producer running, dropped with the last handle.
enum Driver {
    /// A thread that watches the stop token.
    Thread,
    /// A task on a thread of its own, for capture objects that cannot move.
    Local {
        /// Stops the task when dropped.
        _task: crate::local_task::LocalTask,
    },
    /// Nothing: the application pushes.
    Pushed,
}

impl fmt::Debug for Driver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Thread => "Thread",
            Self::Local { .. } => "Local",
            Self::Pushed => "Pushed",
        })
    }
}

/// The shared half of a [`VideoSource`].
#[derive(Debug)]
struct VideoInner {
    /// What kind of source this is, for logs and status.
    kind: String,
    format: VideoFormat,
    frames: FrameReader,
    demand: Demand,
    /// Cancelled when the last handle goes, which stops the producer.
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
/// Capture runs while any clone exists, including the clone a broadcast holds;
/// encoding is demand-driven. Cheap to clone.
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

    /// Opens a camera, display or window, as `config.source` names, and returns
    /// once it produced a frame.
    ///
    /// The config is upstream's, which already names every backend and device.
    /// A device that opens and then produces nothing within half a minute
    /// fails, since a screen capture may wait that long on a permission dialog
    /// but a camera never does.
    ///
    /// Cancellation safe: dropping the future stops the thread and releases
    /// the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Device`] if the device will not open or produces no
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
    /// A sweeping bar, a frame counter, a clock and a marker that flashes in
    /// step with [`AudioSource::test_pattern`]'s beep, drawn on a thread of its
    /// own at exactly `rate`. See the `generator` module docs for what each
    /// element makes visible.
    pub fn test_pattern(size: video::Size, rate: video::Rate) -> Self {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("test-pattern".into())
            .spawn(move || generator::run_pattern(size, rate, slot, token));
        if let Err(err) = spawned {
            // The slot closes with the thread that never started, so the
            // source reads as ended rather than as silent.
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

    /// Returns a source fed by the returned sender, for frames the application
    /// makes.
    ///
    /// The source ends when every sender is dropped. `format` describes what
    /// the sender will push; the broadcast encodes at the size of the frames
    /// that actually arrive.
    pub fn push(format: VideoFormat) -> (FrameSender<video::Frame>, Self) {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let source = Self::new("push", format, reader, stop.clone(), Driver::Pushed);
        let sender = FrameSender::new(Arc::new(slot), stop, source.inner.demand.clone());
        (sender, source)
    }

    /// Runs `run` on a dedicated thread, which feeds frames into the sender it
    /// is handed.
    ///
    /// `run` may create thread-bound platform objects, which is what this is
    /// for. The thread has a single-threaded Tokio runtime entered, so `run`
    /// can drive async code with `tokio::runtime::Handle::current().block_on`.
    /// The source ends once `run` has returned and every clone of its sender
    /// is dropped, and fails with the error `run` returned. `run` should
    /// return once [`FrameSender::push`] reports [`Closed`](crate::Closed).
    ///
    /// # Errors
    ///
    /// Fails if the thread cannot be started.
    pub fn spawn<F>(name: &str, format: VideoFormat, run: F) -> Result<Self, Error>
    where
        F: FnOnce(FrameSender<video::Frame>) -> Result<(), Error> + Send + 'static,
    {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let stop = CancellationToken::new();
        let demand = Demand::default();
        let sender = FrameSender::new(Arc::new(slot.clone()), stop.clone(), demand.clone());
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
        Ok(Self {
            inner: Arc::new(VideoInner {
                kind: name.to_string(),
                format,
                frames: reader,
                demand,
                stop,
                _driver: Driver::Thread,
            }),
        })
    }

    /// Starts the Raspberry Pi camera through `rpicam-vid`, for raw pictures.
    ///
    /// The raw geometry is rounded up to one libcamera leaves tightly packed,
    /// so [`format`](Self::format) may be a few columns wider than asked for.
    /// For the camera's own hardware H.264, see
    /// [`EncodedVideoSource::rpicam`].
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
    /// Every call returns a handle onto the same stream: reading it costs no
    /// encode and does not count as demand.
    pub fn frames(&self) -> VideoFrames {
        self.inner.frames.frames()
    }

    /// Returns what the source actually opened at, which may differ from what
    /// the config asked for.
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

/// Pre-encoded H.264, bypassing the encoders.
///
/// The stream describes itself: the catalog rendition is derived from its
/// first SPS, so nothing here has to describe an encode it did not perform.
/// Not `Clone`, because a byte stream has one reader.
pub struct EncodedVideoSource {
    pub(crate) bytes: n0_future::boxed::BoxStream<bytes::Bytes>,
    /// Keeps whatever produces the bytes alive, such as a subprocess.
    pub(crate) _guard: Option<Box<dyn std::any::Any + Send>>,
}

impl fmt::Debug for EncodedVideoSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodedVideoSource").finish_non_exhaustive()
    }
}

impl EncodedVideoSource {
    /// Wraps an Annex-B H.264 byte stream.
    ///
    /// The stream may split anywhere: access units are found by their start
    /// codes. It should repeat its parameter sets before every keyframe, so a
    /// subscriber that joins late can start decoding.
    pub fn annex_b(bytes: impl n0_future::Stream<Item = bytes::Bytes> + Send + 'static) -> Self {
        Self {
            bytes: Box::pin(bytes),
            _guard: None,
        }
    }

    /// Starts the Raspberry Pi camera through `rpicam-vid`, for the H.264 its
    /// hardware encoder produces.
    ///
    /// The cheapest thing a Pi Zero can publish: no raw pipe and no second
    /// encode. The subprocess is killed when the source is dropped.
    ///
    /// Cancellation safe: the subprocess starts without waiting, so the future
    /// resolves at its first poll.
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

/// Which microphone, and which output's signal to cancel from it.
#[cfg(feature = "capture")]
#[derive(Debug, Clone, Default)]
pub struct MicrophoneConfig {
    /// The device and its capture settings.
    pub capture: audio::capture::Config,
    /// The output whose signal is removed from the microphone, or `None` for
    /// no echo cancellation.
    ///
    /// Without it, a handset on speakerphone publishes its own output back to
    /// the peer. Needs the `aec` feature: [`AudioSource::microphone`] refuses
    /// the config without it. The canceller is built when a broadcast starts
    /// publishing the microphone, after the publication it replaces has let go
    /// of its own, since an output feeds one canceller at a time.
    pub echo_reference: Option<AudioOutput>,
}

#[cfg(feature = "capture")]
impl MicrophoneConfig {
    /// Returns the config with a specific microphone, by the id
    /// `audio::capture::devices` reports.
    #[must_use]
    pub fn with_device(mut self, device: impl Into<String>) -> Self {
        self.capture.source = audio::capture::Source::Microphone(Some(device.into()));
        self
    }

    /// Checks what can be checked before a publication starts: that echo
    /// cancellation, if asked for, is compiled in.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if echo cancellation is asked for in a
    /// build without it.
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
    /// # Errors
    ///
    /// Fails if echo cancellation is asked for in a build without it, or if
    /// the output already feeds another canceller.
    pub(crate) fn resolve(&self) -> Result<audio::capture::Config, Error> {
        #[cfg_attr(not(feature = "aec"), allow(unused_mut, reason = "only aec attaches"))]
        let mut capture = self.capture.clone();
        if let Some(output) = &self.echo_reference {
            #[cfg(feature = "aec")]
            {
                capture.aec = output.canceller()?;
            }
            #[cfg(not(feature = "aec"))]
            {
                let _ = output;
                return Err(Error::invalid(
                    "echo cancellation needs the `aec` feature, which this build was \
                     compiled without",
                ));
            }
        }
        Ok(capture)
    }
}

/// What an audio source produces.
#[derive(Debug)]
pub(crate) enum AudioKind {
    /// A microphone, opened by the publication that encodes it, which also
    /// builds its echo canceller.
    #[cfg(feature = "capture")]
    Microphone(MicrophoneConfig),
    /// PCM from a file, a generator or the application, fanned out to every
    /// broadcast that reads it.
    Pcm {
        format: AudioFormat,
        fanout: PcmFanout,
    },
}

/// The shared half of an [`AudioSource`].
#[derive(Debug)]
struct AudioInner {
    kind_name: &'static str,
    kind: AudioKind,
    /// Held while a broadcast publishes this source, for
    /// [`FrameSender::demand`].
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
/// Runs while any clone exists. Cheap to clone; every broadcast it feeds reads
/// all of its samples.
#[derive(Debug, Clone)]
pub struct AudioSource {
    inner: Arc<AudioInner>,
}

impl AudioSource {
    fn pcm(
        kind_name: &'static str,
        format: AudioFormat,
        fanout: PcmFanout,
        stop: CancellationToken,
        driver: Driver,
    ) -> Self {
        Self::pcm_with_demand(kind_name, format, fanout, stop, driver, Demand::default())
    }

    fn pcm_with_demand(
        kind_name: &'static str,
        format: AudioFormat,
        fanout: PcmFanout,
        stop: CancellationToken,
        driver: Driver,
        demand: Demand,
    ) -> Self {
        Self {
            inner: Arc::new(AudioInner {
                kind_name,
                kind: AudioKind::Pcm { format, fanout },
                demand,
                stop,
                _driver: driver,
            }),
        }
    }

    /// Registers a broadcast publishing this source, for
    /// [`FrameSender::demand`].
    pub(crate) fn want(&self) -> DemandGuard {
        self.inner.demand.acquire()
    }

    /// Opens a microphone.
    ///
    /// Checks that the device exists and that echo cancellation, if asked
    /// for, is compiled in. The device itself opens when a broadcast first has
    /// a listener for it, together with the echo canceller, and a failure then
    /// shows in the broadcast's [`PublishStatus`](crate::PublishStatus):
    /// moq-audio opens a microphone only inside the publication that encodes
    /// it. The same source set on two broadcasts is therefore two captures of
    /// the device.
    ///
    /// Cancellation safe: nothing is open until a broadcast wants it, so
    /// dropping the future only abandons the device query.
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
        Self {
            inner: Arc::new(AudioInner {
                kind_name: "microphone",
                kind: AudioKind::Microphone(config),
                demand: Demand::default(),
                stop: CancellationToken::new(),
                _driver: Driver::Pushed,
            }),
        }
    }

    /// Decodes a file in real time, restarting at the beginning when
    /// `looping`.
    ///
    /// WAV and MP3 are readable. Opens and validates the codec before
    /// returning.
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
        let stop = CancellationToken::new();
        // Stops the decode thread if this future is dropped after it started:
        // a looping file would otherwise decode for the rest of the process.
        let abandoned = stop.clone().drop_guard();
        let format = {
            let fanout = fanout.clone();
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || file::spawn(path, looping, fanout, stop))
                .await
                .map_err(|err| Error::device_msg(format!("the file reader failed: {err}")))??
        };
        abandoned.disarm();
        Ok(Self::pcm("file", format, fanout, stop, Driver::Thread))
    }

    /// Returns a steady sine tone at `hz`, at 48 kHz in `layout`.
    pub fn tone(hz: f32, layout: audio::Layout) -> Self {
        Self::generated("tone", f64::from(hz), layout, generator::Gate::Continuous)
    }

    /// Returns the beeping tone that goes with
    /// [`VideoSource::test_pattern`].
    ///
    /// Beeps for a tenth of a second every second, on the timeline the
    /// pattern's marker flashes on, so whether the flash and the beep land
    /// together is something a viewer sees and hears.
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
        let stop = CancellationToken::new();
        let spawned = {
            let fanout = fanout.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || generator::run_tone(hz, format, gate, fanout, stop))
        };
        if let Err(err) = spawned {
            warn!(error = %err, "the tone thread did not start");
        }
        Self::pcm(name, format, fanout, stop, Driver::Thread)
    }

    /// Returns a source fed by the returned sender, for PCM the application
    /// makes.
    ///
    /// Frames carry interleaved 32-bit float samples in `format`. A broadcast
    /// that falls more than a few seconds behind loses the oldest frames and
    /// counts them in its stats. The sender's demand is true while a broadcast
    /// publishes the source.
    pub fn push(format: AudioFormat) -> (FrameSender<audio::Frame>, Self) {
        let (fanout, _) = tokio::sync::broadcast::channel(PCM_BUFFER);
        let stop = CancellationToken::new();
        let demand = Demand::default();
        let sender = FrameSender::new(Arc::new(fanout.clone()), stop.clone(), demand.clone());
        (
            sender,
            Self::pcm_with_demand("push", format, fanout, stop, Driver::Pushed, demand),
        )
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
pub(crate) const FIRST_FRAME_PATIENCE: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
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
        let mut demand = sender.demand();
        use n0_watcher::Watcher as _;
        assert!(!demand.get());
        let wanted = source.want();
        assert!(demand.get());
        drop(wanted);
        assert!(!demand.get());
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

    /// Echo cancellation used to be something nothing attached. The
    /// microphone config builds its canceller from the output it is given,
    /// and this fails if it does not. It needs an output device, so it is run
    /// by hand; `publish::tests` covers the publication asking for the
    /// canceller without one.
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
