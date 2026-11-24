//! A media file, published as it is or decoded like a camera.
//!
//! [`MediaFile::open`] reads the sample index of an MP4 file once. From it,
//! [`encoded_video`](MediaFile::encoded_video) and
//! [`encoded_audio`](MediaFile::encoded_audio) hand the file's H.264 and AAC
//! to a broadcast without decoding, and [`video`](MediaFile::video) and
//! [`audio`](MediaFile::audio) decode them into raw sources for the normal
//! encoders. Every stream of one file paces against one clock, so audio and
//! video stay in sync and loop together.

use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use tokio::time::Instant;
use tracing::{debug, warn};

use self::mp4::{Codec, Sample, Track};
use super::{
    AudioFormat, AudioSource, FileEnd, FrameSender, VideoFormat, VideoSource, file::file_error,
    sleep_until,
};
use crate::{EncodedAudio, EncodedFrame, EncodedVideo, Error, audio, video};

mod mp4;

/// The frame rate of a video track whose samples have no durations.
const FALLBACK_FPS: u32 = 30;

/// The shortest pass over a file, one frame at [`FALLBACK_FPS`].
///
/// A file of one sample, or of samples without durations, would otherwise
/// loop a million times a second.
const MIN_PASS: Duration = Duration::from_micros(1_000_000 / FALLBACK_FPS as u64);

/// How far playback can fall behind the wall clock before the timeline moves forward.
///
/// A stalled disk makes playback late. Without the move, the file would play
/// fast until it caught up. After the move, the content resumes where it
/// stopped.
const MAX_BEHIND: Duration = Duration::from_secs(1);

/// The deepest picture reordering the decode path allows for, the size of the largest H.264 picture buffer.
const MAX_HELD: usize = 16;

/// An MP4 file, ready to publish as it is or to decode.
///
/// Opening reads the boxes that describe the file and none of its media.
/// Cheap to clone: clones share the file and its clock.
#[derive(Debug, Clone)]
pub struct MediaFile {
    shared: Arc<Shared>,
    end: FileEnd,
}

/// What every stream of a [`MediaFile`] shares.
#[derive(Debug)]
struct Shared {
    path: PathBuf,
    file: Mutex<std::fs::File>,
    video: Option<Track>,
    audio: Option<Track>,
    info: MediaInfo,
    /// The length of one pass over the file.
    pass: Duration,
    clock: Clock,
}

/// What a [`MediaFile`] holds, as [`MediaFile::info`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MediaInfo {
    /// The first video track, if any.
    pub video: Option<VideoInfo>,
    /// The first audio track, if any.
    pub audio: Option<AudioInfo>,
    /// How long one pass over the file plays.
    pub duration: Duration,
}

/// The video track of a [`MediaFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoInfo {
    /// The codec's name, such as `H.264` or `H.265`.
    pub codec: &'static str,
    /// The picture size the track declares.
    pub size: video::Size,
    /// The average frame rate over the whole track.
    pub rate: video::FrameRate,
    /// The longest time between two keyframes, which a new passthrough viewer may wait.
    pub gop: Duration,
}

/// The audio track of a [`MediaFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioInfo {
    /// The codec's name, such as `AAC` or `Opus`.
    pub codec: &'static str,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// The number of channels.
    pub channels: u32,
}

/// How [`MediaFile::video`] decodes the video.
///
/// The default lets moq-video pick the decoder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoFileConfig {
    /// The decoder backend to open.
    pub decoder: video::decode::Kind,
}

impl VideoFileConfig {
    /// Returns this config with `decoder` as the decoder backend to open.
    #[must_use]
    pub fn with_decoder(mut self, decoder: video::decode::Kind) -> Self {
        self.decoder = decoder;
        self
    }
}

impl MediaFile {
    /// Opens the MP4 file at `path`, plain or fragmented, and reads its sample index.
    ///
    /// The file plays once, and [`with_end`](Self::with_end) makes it loop.
    /// The index stays in memory, about 40 bytes per sample, which is 22 MB
    /// for two hours of 30 fps video with 48 kHz AAC.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Device`] if the file cannot be read, is not an MP4
    /// file, is corrupt, or holds neither audio nor video.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let shared = tokio::task::spawn_blocking(move || Shared::open(path))
            .await
            .map_err(|err| Error::device_msg(format!("the file reader failed: {err}")))??;
        Ok(Self {
            shared: Arc::new(shared),
            end: FileEnd::Stop,
        })
    }

    /// Returns this file with `end` as what its streams do at the end of the file.
    ///
    /// With [`FileEnd::Restart`], every stream starts over after the longest
    /// track ends, and timestamps keep rising across the passes.
    #[must_use]
    pub fn with_end(mut self, end: FileEnd) -> Self {
        self.end = end;
        self
    }

    /// Returns the file's tracks, codecs and length.
    #[must_use]
    pub fn info(&self) -> &MediaInfo {
        &self.shared.info
    }

    /// Returns the path the file was opened from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// Returns the file's H.264 video as it is, paced to the wall clock.
    ///
    /// For `LocalBroadcast::set_encoded_video`. Each access unit is due when
    /// its decode time comes. Passthrough costs no CPU and keeps the original
    /// quality, but it publishes one rendition at the file's size and
    /// bitrate. Keyframes come only where the file has them, and nothing can
    /// ask for another. A stream that starts while the file already plays
    /// starts at the next keyframe. A read error ends the stream with that
    /// error.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if the file has no video, or video
    /// in another codec than H.264.
    pub fn encoded_video(&self) -> Result<EncodedVideo, Error> {
        Ok(EncodedVideo::Avc1 {
            avcc: self.codec_record(true)?,
            frames: Box::pin(self.paced(true)),
        })
    }

    /// Returns the file's AAC audio as it is, paced to the wall clock.
    ///
    /// For `LocalBroadcast::set_encoded_audio`, with the trade-offs of
    /// [`encoded_video`](Self::encoded_video).
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if the file has no audio, or audio
    /// in another codec than AAC.
    pub fn encoded_audio(&self) -> Result<EncodedAudio, Error> {
        Ok(EncodedAudio::Aac {
            config: self.codec_record(false)?,
            frames: Box::pin(self.paced(false)),
        })
    }

    /// Decodes the file's H.264 video in real time into a raw source, like a camera.
    ///
    /// Each picture comes when its presentation time is due, at the file's
    /// size and in the decoder's own memory, so a hardware decoder's pictures
    /// reach a hardware encoder without a copy. The renditions of the
    /// broadcast scale them. The rate is the track's average. The decoder
    /// opens before this returns, and decodes whether or not a broadcast
    /// wants the source.
    ///
    /// Cancellation safe: dropping the future stops the decode thread.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] for a file without H.264 video,
    /// and with [`Error::Decoder`] if no decoder of the
    /// [`VideoFileConfig::decoder`] backend opens. A read or decode error
    /// later fails the source, as [`VideoSource::failure`] reports.
    pub async fn video(&self, config: VideoFileConfig) -> Result<VideoSource, Error> {
        let catalog =
            moq_mux::codec::h264::config(&self.codec_record(true)?).map_err(Error::decoder)?;
        let info = self.shared.info.video.as_ref().expect("the file has video");
        let format = VideoFormat {
            size: info.size,
            rate: info.rate,
        };
        let file = self.clone();
        // The decoder may be bound to the thread that opens it, so it opens on
        // the decode thread, which reports the result back.
        let (opened, ready) = tokio::sync::oneshot::channel();
        let source = VideoSource::spawn("video-file", format, move |sender| {
            let mut decoder_config = video::decode::Config::new();
            decoder_config.kind = config.decoder;
            let decoder = match video::decode::Decoder::new(&catalog, &decoder_config) {
                Ok(decoder) => {
                    let _ = opened.send(Ok(()));
                    decoder
                }
                Err(err) => {
                    let _ = opened.send(Err(Error::decoder(err)));
                    return Ok(());
                }
            };
            file.decode_video(
                decoder,
                |frame| sender.push(frame).is_ok(),
                || sender.is_closed(),
            )
        })?;
        ready
            .await
            .map_err(|_| Error::device_msg("the file decoder stopped before it opened"))??;
        Ok(source)
    }

    /// Decodes the file's AAC audio in real time into a raw source, like a microphone.
    ///
    /// The samples keep the file's rate and layout and come when they are due.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if the file has no AAC audio, and
    /// with [`Error::Decoder`] if the decoder does not open.
    pub fn audio(&self) -> Result<AudioSource, Error> {
        let catalog =
            moq_mux::codec::aac::config(&self.codec_record(false)?).map_err(Error::decoder)?;
        let decoder = moq_audio::decode::Decoder::new(&catalog, &Default::default())
            .map_err(Error::decoder)?;
        let format = AudioFormat {
            sample_rate: decoder.sample_rate(),
            layout: decoder.layout(),
        };
        let file = self.clone();
        AudioSource::spawn("audio-file", format, move |sender| {
            file.decode_audio(decoder, &sender)
        })
    }

    /// Returns the avcC record of the video track, or the AudioSpecificConfig of the audio track.
    ///
    /// Fails if the track is missing or in another codec than H.264 or AAC.
    fn codec_record(&self, video: bool) -> Result<Bytes, Error> {
        let (track, kind, supported) = if video {
            (&self.shared.video, "video", "H.264")
        } else {
            (&self.shared.audio, "audio", "AAC")
        };
        let path = self.path().display();
        let track = track
            .as_ref()
            .ok_or_else(|| Error::invalid(format!("{path} has no {kind} track")))?;
        match (&track.codec, video) {
            (Codec::H264(record), true) | (Codec::Aac(record), false) => Ok(record.clone()),
            (codec, _) => Err(Error::invalid(format!(
                "{path}: its {kind} is in {}, and only {supported} is supported",
                codec.name()
            ))),
        }
    }

    /// Returns the samples of the video or audio track, each when its decode time is due.
    ///
    /// The stream ends after a read error, which it yields.
    fn paced(
        &self,
        video: bool,
    ) -> impl n0_future::Stream<Item = Result<EncodedFrame, Error>> + use<> {
        let schedule = Schedule::new(self.clone(), video);
        n0_future::stream::unfold(Some(schedule), |schedule| async move {
            let mut schedule = schedule?;
            let next = schedule.next()?;
            let frame = schedule.file.when_due(next).await;
            let more = frame.is_ok().then_some(schedule);
            Some((frame, more))
        })
    }

    /// Reads the sample of `next`, and returns it once it is due.
    async fn when_due(&self, next: Scheduled) -> Result<EncodedFrame, Error> {
        let shared = self.shared.clone();
        // The read comes before the wait, so its time falls inside it.
        let data = tokio::task::spawn_blocking(move || shared.read(&next.sample))
            .await
            .map_err(|err| Error::device_msg(format!("the file reader failed: {err}")))??;
        let clock = &self.shared.clock;
        let (skipped, due) = clock.due(next.decode_at);
        tokio::time::sleep_until(due).await;
        clock.catch_up(skipped, due);
        let timestamp = skipped + next.timestamp;
        Ok(EncodedFrame {
            data,
            timestamp: to_timestamp(timestamp)?,
            at: clock.instant(timestamp).into_std(),
        })
    }

    /// Decodes the video track and hands each picture to `push` when it is due.
    ///
    /// Returns when the file ends, or when `push` returns `false` or `closed`
    /// returns `true` because the source is gone.
    fn decode_video(
        &self,
        mut decoder: video::decode::Decoder,
        mut push: impl FnMut(video::Frame) -> bool,
        closed: impl Fn() -> bool,
    ) -> Result<(), Error> {
        let clock = &self.shared.clock;
        // A workaround for moq-video's VideoToolbox backend, which returns
        // pictures in decode order: up to `depth` pictures wait here, by
        // presentation time, and the earliest goes once more wait. It can go
        // once that backend reorders on its own.
        let depth = reorder_depth(
            self.shared
                .video
                .as_ref()
                .map_or(&[], |track| &track.samples),
        );
        let mut held = BTreeMap::new();
        // Takes the pictures the decoder readied, and hands over those whose
        // turn it is once each is due.
        let mut drain = |decoder: &mut video::decode::Decoder,
                         held: &mut BTreeMap<u64, video::Frame>,
                         flushing: bool|
         -> Result<bool, Error> {
            while let Some(frame) = decoder.receive().map_err(Error::decoder)? {
                held.insert(micros(&frame), frame);
            }
            let keep = if flushing { 0 } else { depth };
            while held.len() > keep {
                let (at, mut frame) = held.pop_first().expect("a held picture");
                let Some(at) = clock.wait(Duration::from_micros(at), &closed) else {
                    return Ok(false);
                };
                frame.timestamp = to_timestamp(at)?;
                if !push(frame) {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        for next in Schedule::new(self.clone(), true) {
            if next.restarted {
                // The pictures a reordering decoder still holds belong to the last pass.
                decoder.flush().map_err(Error::decoder)?;
                if !drain(&mut decoder, &mut held, true)? {
                    return Ok(());
                }
                debug!(path = %self.path().display(), "video file looping");
            }
            let data = self.shared.read(&next.sample)?;
            let timestamp = to_timestamp(next.timestamp)?;
            decoder
                .decode(&data, timestamp, next.sample.keyframe)
                .map_err(Error::decoder)?;
            if !drain(&mut decoder, &mut held, false)? {
                return Ok(());
            }
        }
        decoder.flush().map_err(Error::decoder)?;
        drain(&mut decoder, &mut held, true)?;
        debug!(path = %self.path().display(), "video file ended");
        Ok(())
    }

    /// Decodes the audio track into `sender`, each packet when it is due.
    fn decode_audio(
        &self,
        mut decoder: moq_audio::decode::Decoder,
        sender: &FrameSender<audio::Frame>,
    ) -> Result<(), Error> {
        let mut corrupt = 0u64;
        for next in Schedule::new(self.clone(), false) {
            if next.restarted {
                // Each pass starts with the encoder delay again.
                decoder.reset().map_err(Error::decoder)?;
            }
            let data = self.shared.read(&next.sample)?;
            let Some(timestamp) = self
                .shared
                .clock
                .wait(next.timestamp, || sender.is_closed())
            else {
                return Ok(());
            };
            // A corrupt packet is not fatal. The file keeps playing without it.
            if let Err(err) = decoder.decode(&data) {
                corrupt += 1;
                if corrupt == 1 {
                    warn!(path = %self.path().display(), error = %err, "skipping a corrupt audio packet, later ones log at debug");
                } else {
                    debug!(corrupt, error = %err, "skipping a corrupt audio packet");
                }
                continue;
            }
            while let Some(decoded) = decoder.receive().map_err(Error::decoder)? {
                let data = Bytes::from_iter(
                    decoded
                        .samples
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes()),
                );
                if sender
                    .push(audio::Frame::new(data, to_timestamp(timestamp)?))
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
        debug!(path = %self.path().display(), "audio file ended");
        Ok(())
    }
}

impl Shared {
    /// Opens `path` and reads its sample index.
    fn open(path: PathBuf) -> Result<Self, Error> {
        let mut file = std::fs::File::open(&path).map_err(|err| file_error(&path, err))?;
        let tracks = mp4::index(&mut file).map_err(|err| file_error(&path, err))?;
        let mut video = None;
        let mut audio = None;
        for track in tracks {
            let slot = if track.video { &mut video } else { &mut audio };
            if slot.is_none() && !track.samples.is_empty() {
                *slot = Some(track);
            }
        }
        if video.is_none() && audio.is_none() {
            return Err(file_error(&path, "the file has no audio or video samples"));
        }
        let end = video
            .iter()
            .chain(&audio)
            .flat_map(|track| &track.samples)
            .map(|sample| sample.pts.saturating_add(i64::from(sample.duration)))
            .max()
            .unwrap_or(0);
        let pass = Duration::from_micros(u64::try_from(end).unwrap_or(0)).max(MIN_PASS);
        let info = MediaInfo {
            video: video.as_ref().map(video_info),
            audio: audio.as_ref().map(|track| AudioInfo {
                codec: track.codec.name(),
                sample_rate: track.sample_rate,
                channels: track.channels,
            }),
            duration: pass,
        };
        debug!(path = %path.display(), ?video, ?audio, ?pass, "file opened");
        Ok(Self {
            path,
            file: Mutex::new(file),
            video,
            audio,
            info,
            pass,
            clock: Clock::default(),
        })
    }

    /// Reads the data of `sample`, which the index checked to lie within the file.
    fn read(&self, sample: &Sample) -> Result<Bytes, Error> {
        let mut data = vec![0; sample.size as usize];
        let mut file = self.file.lock().expect("poisoned");
        file.seek(SeekFrom::Start(sample.offset))
            .and_then(|_| file.read_exact(&mut data))
            .map_err(|err| {
                file_error(
                    &self.path,
                    format!("sample at byte {}: {err}", sample.offset),
                )
            })?;
        Ok(data.into())
    }
}

/// Reports whether the file at `path` starts with a box an MP4 or QuickTime file starts with.
pub(super) async fn is_mp4(path: &Path) -> bool {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut head = [0; 8];
        std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut head))
            .is_ok_and(|()| {
                matches!(
                    &head[4..],
                    b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip"
                )
            })
    })
    .await
    .unwrap_or(false)
}

/// Returns the info of a video track.
///
/// The rate is the track's average, so a long first frame of a screen
/// recording does not set it.
fn video_info(track: &Track) -> VideoInfo {
    let span: i64 = track
        .samples
        .iter()
        .map(|sample| i64::from(sample.duration))
        .sum();
    let rate = (span > 0)
        .then(|| video::FrameRate::from_f64(track.samples.len() as f64 * 1e6 / span as f64).ok())
        .flatten()
        .unwrap_or_else(|| video::FrameRate::new(FALLBACK_FPS, 1).expect("a valid rate"));
    // The longest run from one keyframe to the next, or to the end.
    let end = track
        .samples
        .last()
        .map_or(0, |last| last.dts + i64::from(last.duration));
    let mut gop = 0i64;
    let mut last = None;
    for sample in track.samples.iter().filter(|sample| sample.keyframe) {
        if let Some(last) = last {
            gop = gop.max(sample.dts - last);
        }
        last = Some(sample.dts);
    }
    gop = gop.max(end - last.unwrap_or(0));
    VideoInfo {
        codec: track.codec.name(),
        size: video::Size::new(track.width, track.height),
        rate,
        gop: Duration::from_micros(gop.max(0) as u64),
    }
}

/// Returns a media time as a timestamp, or an error past the timestamp range.
fn to_timestamp(at: Duration) -> Result<moq_net::Timestamp, Error> {
    u64::try_from(at.as_micros())
        .ok()
        .and_then(|micros| moq_net::Timestamp::from_micros(micros).ok())
        .ok_or_else(|| Error::device_msg(format!("a file time of {at:?} is out of range")))
}

/// Returns how many pictures of `samples`, in decode order, can come before an earlier one.
///
/// That is the most samples that follow one sample in decode order and
/// present before it, up to [`MAX_HELD`].
fn reorder_depth(samples: &[Sample]) -> usize {
    samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            samples[index + 1..]
                .iter()
                .take(MAX_HELD)
                .filter(|later| later.pts < sample.pts)
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Returns the presentation time of `frame` in microseconds.
fn micros(frame: &video::Frame) -> u64 {
    frame
        .timestamp
        .as_micros()
        .map_or(0, |micros| micros as u64)
}

/// The wall clock every stream of a file plays against.
///
/// It starts with the first sample any stream asks for, so a file opened
/// early does not rush its first seconds out. A stream that starts later
/// joins at the clock's position.
#[derive(Debug, Default)]
struct Clock {
    /// When media time zero is due.
    epoch: OnceLock<Instant>,
    /// How far the timeline moved forward after playback fell behind, in microseconds.
    skipped: AtomicU64,
}

impl Clock {
    /// Returns when media time `at` is due, starting the clock if it has not started.
    fn instant(&self, at: Duration) -> Instant {
        *self.epoch.get_or_init(Instant::now) + at
    }

    /// Returns how far the timeline moved forward, which every media time adds.
    fn skipped(&self) -> Duration {
        Duration::from_micros(self.skipped.load(Ordering::Relaxed))
    }

    /// Returns how far the timeline moved forward, and when media time `at` is due on it.
    fn due(&self, at: Duration) -> (Duration, Instant) {
        let skipped = self.skipped();
        (skipped, self.instant(skipped + at))
    }

    /// Sleeps this thread until media time `at` is due, and returns `at` on the moved timeline.
    ///
    /// Returns `None` once `closed` reports the source gone.
    fn wait(&self, at: Duration, closed: impl Fn() -> bool) -> Option<Duration> {
        let (skipped, due) = self.due(at);
        if !sleep_until(due.into_std(), closed) {
            return None;
        }
        self.catch_up(skipped, due);
        Some(skipped + at)
    }

    /// Returns the media time that plays now, or `None` before the clock starts.
    fn position(&self) -> Option<Duration> {
        let epoch = self.epoch.get()?;
        Some(
            Instant::now()
                .saturating_duration_since(*epoch)
                .saturating_sub(self.skipped()),
        )
    }

    /// Moves the timeline forward if a sample due at `due` comes more than [`MAX_BEHIND`] late.
    ///
    /// `skipped` is what [`due`](Self::due) returned with `due`. When every stream falls behind in one stall, only the
    /// first one moves the timeline, so the stall counts once. The move holds
    /// for every stream of the file, so they stay in sync.
    fn catch_up(&self, skipped: Duration, due: Instant) {
        let behind = Instant::now().saturating_duration_since(due);
        if behind <= MAX_BEHIND {
            return;
        }
        let read = skipped.as_micros() as u64;
        let moved = read + behind.as_micros() as u64;
        if self
            .skipped
            .compare_exchange(read, moved, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            warn!(
                ?behind,
                "file playback fell behind, so its timeline moves forward"
            );
        }
    }
}

/// One sample with its times on the file's timeline.
#[derive(Debug, Clone, Copy)]
struct Scheduled {
    sample: Sample,
    /// Whether the sample starts a pass over the file after an earlier one.
    restarted: bool,
    /// When the sample decodes.
    decode_at: Duration,
    /// When the sample presents.
    timestamp: Duration,
}

/// The samples of one track in decode order, pass after pass for a looping file.
///
/// Every track of a file starts a pass at the same time, after the longest
/// track ended, so a shorter track pauses and the tracks stay in sync. The
/// first sample is the first keyframe at or after the clock's position, so a
/// stream that joins a playing file plays in sync with the others.
#[derive(Debug)]
struct Schedule {
    file: MediaFile,
    video: bool,
    /// The next sample, or `None` before the first.
    index: Option<usize>,
    pass: u64,
}

impl Schedule {
    fn new(file: MediaFile, video: bool) -> Self {
        Self {
            file,
            video,
            index: None,
            pass: 0,
        }
    }
}

impl Iterator for Schedule {
    type Item = Scheduled;

    fn next(&mut self) -> Option<Scheduled> {
        let shared = &self.file.shared;
        let track = if self.video {
            shared.video.as_ref()?
        } else {
            shared.audio.as_ref()?
        };
        let pass = shared.pass.as_micros() as u64;
        let first = self.index.is_none();
        let mut index = self.index.unwrap_or_else(|| {
            let position = shared.clock.position().unwrap_or_default().as_micros() as u64;
            self.pass = position / pass;
            let offset = (position % pass) as i64;
            track
                .samples
                .iter()
                .position(|sample| sample.keyframe && sample.pts >= offset)
                .unwrap_or(track.samples.len())
        });
        let wrapped = index == track.samples.len();
        if wrapped {
            self.pass += 1;
            index = 0;
        }
        if self.pass > 0 && self.file.end == FileEnd::Stop {
            return None;
        }
        let sample = track.samples[index];
        self.index = Some(index + 1);
        let start = self.pass.saturating_mul(pass);
        let at = |time: i64| Duration::from_micros(start.saturating_add_signed(time));
        Some(Scheduled {
            sample,
            restarted: wrapped && !first,
            decode_at: at(sample.dts),
            timestamp: at(sample.pts),
        })
    }
}

#[cfg(test)]
mod tests {
    use n0_future::StreamExt;

    use super::*;

    /// Returns the path of a test clip in `test_data`.
    ///
    /// - `clip.mp4`: 64x48 H.264 Baseline at 10 fps and 16 kHz mono AAC, one
    ///   second each, made with `ffmpeg -f lavfi -i
    ///   testsrc2=size=64x48:rate=10:duration=1 -f lavfi -i
    ///   sine=frequency=440:sample_rate=16000:duration=1 -c:v libx264 -profile:v
    ///   baseline -crf 40 -g 10 -pix_fmt yuv420p -c:a aac -b:a 16k -ac 1
    ///   -movflags +faststart clip.mp4`. The audio's edit list trims the
    ///   encoder's priming packet.
    /// - `fragmented.mp4`: the same streams, copied with `-movflags
    ///   cmaf+frag_keyframe+empty_moov+default_base_moof`.
    /// - `bframes.mp4`: the same video in High profile with two B-frames
    ///   (`-profile:v high -bf 2`) and no audio. Its edit list starts the
    ///   presentation two frames into the decode timeline.
    fn clip(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/source/test_data")
            .join(name)
    }

    /// Opens the test clip `name`.
    async fn open(name: &str) -> MediaFile {
        MediaFile::open(clip(name)).await.expect("the clip opens")
    }

    #[tokio::test]
    async fn a_plain_mp4_reports_its_tracks() {
        let file = open("clip.mp4").await;
        let info = file.info();
        let video = info.video.as_ref().expect("a video track");
        assert_eq!(video.codec, "H.264");
        assert_eq!(video.size, video::Size::new(64, 48));
        assert_eq!(video.rate, video::FrameRate::new(10, 1).unwrap());
        assert_eq!(video.gop, Duration::from_secs(1));
        let audio = info.audio.as_ref().expect("an audio track");
        assert_eq!(
            (audio.codec, audio.sample_rate, audio.channels),
            ("AAC", 16_000, 1)
        );
    }

    /// A fragmented copy of a file indexes to the same samples, spaced the same.
    ///
    /// ffmpeg drops the audio's edit list when it fragments, so the fragmented
    /// audio keeps the priming packet that the plain file trims. The tracks
    /// are compared from their ends.
    #[tokio::test]
    async fn a_fragmented_mp4_matches_the_plain_one() {
        let plain = open("clip.mp4").await;
        let fragmented = open("fragmented.mp4").await;
        assert_eq!(plain.info().video, fragmented.info().video);
        assert_eq!(plain.info().audio, fragmented.info().audio);
        for video in [true, false] {
            let plain_samples: Vec<_> = Schedule::new(plain.clone(), video).collect();
            let fragmented_samples: Vec<_> = Schedule::new(fragmented.clone(), video).collect();
            let primed = usize::from(!video);
            assert_eq!(plain_samples.len() + primed, fragmented_samples.len());
            let end = |samples: &[Scheduled]| samples.last().unwrap().timestamp;
            let (plain_end, fragmented_end) = (end(&plain_samples), end(&fragmented_samples));
            for (a, b) in plain_samples
                .iter()
                .rev()
                .zip(fragmented_samples.iter().rev())
            {
                assert_eq!(plain_end - a.timestamp, fragmented_end - b.timestamp);
                assert_eq!(a.sample.keyframe, b.sample.keyframe);
                assert_eq!(
                    plain.shared.read(&a.sample).unwrap(),
                    fragmented.shared.read(&b.sample).unwrap()
                );
            }
        }
    }

    /// A looping file starts each pass one file length after the last, on every track.
    #[tokio::test]
    async fn a_looping_file_continues_its_timeline() {
        let file = open("clip.mp4").await.with_end(FileEnd::Restart);
        let length = file.info().duration;
        for video in [true, false] {
            let count = Schedule::new(file.clone().with_end(FileEnd::Stop), video).count();
            let two_passes: Vec<_> = Schedule::new(file.clone(), video).take(2 * count).collect();
            // The edit list trims the audio's priming, so both tracks start at zero.
            assert_eq!(two_passes[0].timestamp, Duration::ZERO);
            let restarts: Vec<_> = two_passes.iter().map(|next| next.restarted).collect();
            assert_eq!(
                restarts.iter().position(|&restarted| restarted),
                Some(count)
            );
            assert_eq!(restarts.iter().filter(|&&restarted| restarted).count(), 1);
            for (first, second) in two_passes[..count].iter().zip(&two_passes[count..]) {
                assert_eq!(second.timestamp, first.timestamp + length);
                assert_eq!(second.decode_at, first.decode_at + length);
            }
        }
    }

    /// Passthrough hands each access unit over when its decode time is due.
    #[tokio::test(start_paused = true)]
    async fn passthrough_paces_to_the_decode_times() {
        let file = open("bframes.mp4").await;
        let EncodedVideo::Avc1 { avcc, mut frames } = file.encoded_video().expect("H.264") else {
            panic!("not avc1");
        };
        assert_eq!(avcc[0], 1, "an avcC record starts with version 1");
        let start = Instant::now();
        let mut arrivals = Vec::new();
        while let Some(frame) = frames.next().await {
            let frame = frame.expect("the clip reads");
            arrivals.push((Instant::now() - start).as_millis() as u64);
            assert_eq!(
                frame.at,
                (start + Duration::from_micros(frame.timestamp.as_micros().unwrap() as u64))
                    .into_std()
            );
        }
        // The first two samples decode before the presentation starts.
        assert_eq!(arrivals, [0, 0, 0, 100, 200, 300, 400, 500, 600, 700]);
    }

    /// A stream that starts while the file plays joins at the clock's position, and the others go on.
    #[tokio::test(start_paused = true)]
    async fn a_late_stream_joins_without_stalling_the_others() {
        let file = open("clip.mp4").await.with_end(FileEnd::Restart);
        let EncodedVideo::Avc1 { mut frames, .. } = file.encoded_video().expect("H.264") else {
            panic!("not avc1");
        };
        let start = Instant::now();
        let video = tokio::spawn(async move {
            let mut arrivals = Vec::new();
            while arrivals.len() < 50 {
                frames
                    .next()
                    .await
                    .expect("a loop")
                    .expect("the clip reads");
                arrivals.push(Instant::now() - start);
            }
            arrivals
        });
        tokio::time::sleep(Duration::from_secs(3)).await;
        let EncodedAudio::Aac { mut frames, .. } = file.encoded_audio().expect("AAC");
        let first = frames
            .next()
            .await
            .expect("a frame")
            .expect("the clip reads");
        let now = Instant::now() - start;
        let presents = Duration::from_micros(first.timestamp.as_micros().unwrap() as u64);
        assert!(
            presents + Duration::from_millis(70) >= now
                && presents <= now + Duration::from_millis(70),
            "the audio joins at {presents:?}, at {now:?}"
        );
        let arrivals = video.await.unwrap();
        // A pass ends with the longer audio, so the video pauses a little at each loop.
        let pause = file.info().duration - Duration::from_millis(900);
        let longest = arrivals.windows(2).map(|pair| pair[1] - pair[0]).max();
        assert_eq!(longest, Some(pause), "{arrivals:?}");
        assert_eq!(file.shared.clock.skipped(), Duration::ZERO);
    }

    /// A stall that every stream sees moves the timeline once.
    #[tokio::test(start_paused = true)]
    async fn a_shared_stall_counts_once() {
        let clock = Clock::default();
        let (skipped, due) = clock.due(Duration::ZERO);
        tokio::time::advance(Duration::from_secs(3)).await;
        for _stream in 0..2 {
            clock.catch_up(skipped, due);
        }
        assert_eq!(clock.skipped(), Duration::from_secs(3));
        assert_eq!(clock.position(), Some(Duration::ZERO));
    }

    /// A backend that returns decode order needs as many pictures held as the stream reorders.
    #[tokio::test]
    async fn the_reorder_depth_follows_the_b_frames() {
        let file = open("bframes.mp4").await;
        assert_eq!(
            reorder_depth(&file.shared.video.as_ref().unwrap().samples),
            2
        );
        let plain = open("clip.mp4").await;
        assert_eq!(
            reorder_depth(&plain.shared.video.as_ref().unwrap().samples),
            0
        );
    }

    #[tokio::test]
    async fn passthrough_refuses_another_codec() {
        let file = open("bframes.mp4").await;
        assert!(matches!(
            file.encoded_audio(),
            Err(Error::InvalidConfig { .. })
        ));
        let not_mp4 =
            MediaFile::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).await;
        assert!(matches!(not_mp4, Err(Error::Device { .. })));
    }

    /// A looping B-frame file decodes to pictures with rising timestamps.
    ///
    /// The software decoder keeps the test independent of the platform's
    /// backend.
    #[test]
    fn a_looping_b_frame_file_decodes_in_presentation_order() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let file = runtime
            .block_on(MediaFile::open(clip("bframes.mp4")))
            .expect("the clip opens")
            .with_end(FileEnd::Restart);
        let catalog = moq_mux::codec::h264::config(&file.codec_record(true).unwrap()).unwrap();
        let mut config = video::decode::Config::new();
        config.kind = video::decode::Kind::Software;
        let decoder = video::decode::Decoder::new(&catalog, &config).unwrap();
        let mut frames = Vec::new();
        file.decode_video(
            decoder,
            |frame| {
                frames.push(frame);
                frames.len() < 12
            },
            || false,
        )
        .expect("the clip plays");
        let millis: Vec<u64> = frames.iter().map(|frame| micros(frame) / 1000).collect();
        assert_eq!(millis, (0..12).map(|n| n * 100).collect::<Vec<_>>());
    }

    /// `AudioSource::file` takes an MP4 file and decodes its AAC at the file's rate and layout.
    #[tokio::test]
    async fn an_mp4_audio_file_decodes() {
        let source = AudioSource::file(clip("clip.mp4"), FileEnd::Stop)
            .await
            .expect("the audio opens");
        let format = source.format().expect("PCM");
        assert_eq!(format.sample_rate, 16_000);
        assert_eq!(format.layout, audio::Layout::Mono);
    }
}
