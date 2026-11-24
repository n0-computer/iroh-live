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
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use hang::catalog::{AAC, AudioConfig, H264, VideoConfig};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use self::mp4::{Codec, Sample, Track};
use super::{
    AudioFormat, AudioKind, AudioSource, Driver, FileEnd, PCM_BUFFER, VideoFormat, VideoSource,
    file::file_error, sender::PcmFanout,
};
use crate::{EncodedAudio, EncodedFrame, EncodedVideo, Error, audio, video};

mod mp4;

/// The frame rate of a video track whose samples have no durations.
const FALLBACK_FPS: f64 = 30.0;

/// How far playback can fall behind the wall clock before it skips ahead.
///
/// A suspended host or a stalled disk makes playback late. Without the skip,
/// the file would play fast until it caught up.
const MAX_BEHIND: Duration = Duration::from_secs(1);

/// The longest a decode thread sleeps before it checks that its source is still wanted.
const WAIT_SLICE: Duration = Duration::from_millis(50);

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
    /// The earliest presentation time of any track, in microseconds, which plays at zero.
    base: i64,
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
    /// The longest time between two keyframes, which a new viewer of a
    /// passthrough may wait for a picture.
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
/// The default keeps the file's size and lets moq-video pick the decoder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoFileConfig {
    /// The size to scale the pictures to, or `None` for the file's size.
    pub size: Option<video::Size>,
    /// The decoder backend to open.
    pub decoder: video::decode::Kind,
}

impl VideoFileConfig {
    /// Returns this config with `size` as the size to scale the pictures to.
    #[must_use]
    pub fn with_size(mut self, size: video::Size) -> Self {
        self.size = Some(size);
        self
    }

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
    /// The file plays once; [`with_end`](Self::with_end) makes it loop.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Device`] if the file cannot be read, is not an MP4
    /// file, or holds neither audio nor video.
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
    /// its decode time comes. Passthrough publishes one rendition at the
    /// file's size and bitrate, with keyframes only where the file has them,
    /// and nothing can ask for another. It costs no CPU and keeps the
    /// original quality.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if the file has no video, or video
    /// in another codec than H.264.
    pub fn encoded_video(&self) -> Result<EncodedVideo, Error> {
        let track = self.track(true, "H.264")?;
        let Codec::H264 { avcc } = &track.codec else {
            unreachable!("track checks the codec")
        };
        Ok(EncodedVideo::Avc1 {
            avcc: avcc.clone(),
            frames: Box::pin(self.paced(true)),
        })
    }

    /// Returns the file's AAC audio as it is, paced to the wall clock.
    ///
    /// For `LocalBroadcast::set_encoded_audio`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if the file has no audio, or audio
    /// in another codec than AAC.
    pub fn encoded_audio(&self) -> Result<EncodedAudio, Error> {
        let track = self.track(false, "AAC")?;
        let Codec::Aac { config } = &track.codec else {
            unreachable!("track checks the codec")
        };
        Ok(EncodedAudio::Aac {
            config: config.clone(),
            frames: Box::pin(self.paced(false)),
        })
    }

    /// Decodes the file's H.264 video in real time into a raw source, like a camera.
    ///
    /// Each picture comes when its presentation time is due, as CPU I420 at
    /// the size `config` asks for, or at the file's size. The rate is the
    /// track's average. The decoder opens before this returns.
    ///
    /// Cancellation safe: dropping the future stops the decode thread.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] for an odd or zero size, or a file
    /// without H.264 video, and with [`Error::Decoder`] if no decoder of the
    /// [`VideoFileConfig::decoder`] backend opens. A read or decode error
    /// later fails the source, as [`VideoSource::failure`] reports.
    pub async fn video(&self, config: VideoFileConfig) -> Result<VideoSource, Error> {
        if let Some(size) = config.size
            && (size.width == 0 || size.height == 0 || size.width % 2 == 1 || size.height % 2 == 1)
        {
            return Err(Error::invalid(format!(
                "a decoded file scales to even sizes only, not {}x{}",
                size.width, size.height
            )));
        }
        let track = self.track(true, "H.264")?;
        let catalog = video_catalog(track);
        let info = self.shared.info.video.as_ref().expect("a video track");
        let format = VideoFormat {
            size: config.size.unwrap_or(info.size),
            rate: info.rate,
        };
        let file = self.clone();
        // The decoder may be bound to the thread that opens it, so it opens on
        // the decode thread, which reports the result back.
        let (opened, ready) = tokio::sync::oneshot::channel();
        let source = VideoSource::spawn("video-file", format, move |sender| {
            let mut decoder_config = video::decode::Config::new();
            decoder_config.kind = config.decoder;
            decoder_config.output = video::Output::Cpu;
            // Only a size that was asked for: an odd coded size is a bad hint.
            decoder_config.scale_hint = config.size;
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
                config.size,
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
        let track = self.track(false, "AAC")?;
        let catalog = audio_catalog(track);
        let decoder = moq_audio::decode::Decoder::new(&catalog, &Default::default())
            .map_err(Error::decoder)?;
        let format = AudioFormat {
            sample_rate: decoder.sample_rate(),
            layout: decoder.layout(),
        };
        let (fanout, _) = tokio::sync::broadcast::channel(PCM_BUFFER);
        let weak = fanout.downgrade();
        let stop = CancellationToken::new();
        let file = self.clone();
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("audio-file".into())
            .spawn(move || {
                if let Err(err) = file.decode_audio(decoder, &fanout, &stopped) {
                    warn!(path = %file.path().display(), error = format_args!("{err:#}"), "audio file decode stopped");
                }
            })?;
        let kind = AudioKind::Pcm {
            format,
            fanout: weak,
        };
        Ok(AudioSource::new("audio-file", kind, stop, Driver::Thread))
    }

    /// Returns the first video or audio track, if it is in `codec`.
    fn track(&self, video: bool, codec: &str) -> Result<&Track, Error> {
        let (track, kind) = if video {
            (&self.shared.video, "video")
        } else {
            (&self.shared.audio, "audio")
        };
        let path = self.path().display();
        let track = track
            .as_ref()
            .ok_or_else(|| Error::invalid(format!("{path} has no {kind} track")))?;
        if track.codec.name() != codec {
            return Err(Error::invalid(format!(
                "{path} has {} {kind}, and only {codec} is supported",
                track.codec.name()
            )));
        }
        Ok(track)
    }

    /// Returns the samples of the video or audio track, each when its decode time is due.
    fn paced(&self, video: bool) -> impl n0_future::Stream<Item = EncodedFrame> + use<> {
        let schedule = Schedule::new(self.clone(), video);
        n0_future::stream::unfold(schedule, |mut schedule| async move {
            let next = schedule.next()?;
            let shared = &schedule.file.shared;
            let skipped = shared.clock.skipped();
            let due = shared.clock.instant(skipped + next.decode_at);
            tokio::time::sleep_until(due).await;
            shared.clock.catch_up(due);
            // A sample is a few kilobytes of a local file, so the read blocks
            // the task for no longer than a page fault would.
            let data = match shared.read(&next.sample) {
                Ok(data) => data,
                Err(err) => {
                    warn!(
                        error = format_args!("{err:#}"),
                        "file read failed, the stream ends"
                    );
                    return None;
                }
            };
            let timestamp = skipped + next.timestamp;
            let frame = EncodedFrame {
                data,
                timestamp: to_timestamp(timestamp),
                at: shared.clock.instant(timestamp).into_std(),
            };
            Some((frame, schedule))
        })
    }

    /// Decodes the video track and hands each picture to `push` when it is due.
    ///
    /// Returns when the file ends, or when `push` returns `false` or `closed`
    /// returns `true` because the source is gone.
    fn decode_video(
        &self,
        mut decoder: video::decode::Decoder,
        size: Option<video::Size>,
        mut push: impl FnMut(video::Frame) -> bool,
        closed: impl Fn() -> bool,
    ) -> Result<(), Error> {
        let clock = &self.shared.clock;
        // Scales a picture, waits until it is due and hands it over.
        let mut show = |frame: video::Frame| -> Result<bool, Error> {
            let frame = match size {
                Some(size) if frame.size() != size => frame
                    .resize(size, &Default::default())
                    .map_err(Error::decoder)?,
                _ => frame,
            };
            let at = Duration::from_micros(frame.timestamp.as_micros().unwrap_or(0) as u64);
            let due = clock.instant(at);
            if !wait_until(due, &closed) {
                return Ok(false);
            }
            clock.catch_up(due);
            Ok(push(frame))
        };
        let mut drain = |decoder: &mut video::decode::Decoder| -> Result<bool, Error> {
            while let Some(frame) = decoder.receive().map_err(Error::decoder)? {
                if !show(frame)? {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let mut pass = 0;
        for next in Schedule::new(self.clone(), true) {
            if next.pass != pass {
                // The pictures a reordering decoder still holds belong to the last pass.
                decoder.flush().map_err(Error::decoder)?;
                if !drain(&mut decoder)? {
                    return Ok(());
                }
                pass = next.pass;
                debug!(path = %self.path().display(), "video file looping");
            }
            let data = self.shared.read(&next.sample)?;
            let timestamp = to_timestamp(clock.skipped() + next.timestamp);
            decoder
                .decode(&data, timestamp, next.sample.keyframe)
                .map_err(Error::decoder)?;
            if !drain(&mut decoder)? {
                return Ok(());
            }
        }
        decoder.flush().map_err(Error::decoder)?;
        drain(&mut decoder)?;
        debug!(path = %self.path().display(), "video file ended");
        Ok(())
    }

    /// Decodes the audio track into `fanout`, each packet when it is due.
    fn decode_audio(
        &self,
        mut decoder: moq_audio::decode::Decoder,
        fanout: &PcmFanout,
        stop: &CancellationToken,
    ) -> Result<(), Error> {
        let clock = &self.shared.clock;
        let mut pass = 0;
        for next in Schedule::new(self.clone(), false) {
            if next.pass != pass {
                // Each pass starts with the encoder delay again.
                decoder.reset().map_err(Error::decoder)?;
                pass = next.pass;
            }
            let data = self.shared.read(&next.sample)?;
            let timestamp = clock.skipped() + next.timestamp;
            let due = clock.instant(timestamp);
            if !wait_until(due, &|| stop.is_cancelled()) {
                return Ok(());
            }
            clock.catch_up(due);
            // A corrupt packet is not fatal: skip it and keep the file playing.
            if let Err(err) = decoder.decode(&data) {
                debug!(error = %err, "skipping a corrupt audio packet");
                continue;
            }
            while let Some(decoded) = decoder.receive().map_err(Error::decoder)? {
                let data = Bytes::from_iter(
                    decoded
                        .samples
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes()),
                );
                // An error only means no broadcast is attached right now.
                let _ = fanout.send(audio::Frame::new(data, to_timestamp(timestamp)));
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
        let tracks = || video.iter().chain(&audio);
        let base = tracks()
            .flat_map(|track| {
                track
                    .samples
                    .iter()
                    .map(|sample| micros(sample.pts, track.timescale))
            })
            .min()
            .unwrap_or(0);
        let end = tracks()
            .flat_map(|track| {
                track
                    .samples
                    .iter()
                    .map(|sample| micros(sample.pts + i64::from(sample.duration), track.timescale))
            })
            .max()
            .unwrap_or(0);
        let pass = Duration::from_micros((end - base).max(1) as u64);
        let info = MediaInfo {
            video: video.as_ref().map(video_info),
            audio: audio.as_ref().map(|track| AudioInfo {
                codec: track.codec.name(),
                sample_rate: track.sample_rate,
                channels: track.channels,
            }),
            duration: pass,
        };
        Ok(Self {
            path,
            file: Mutex::new(file),
            video,
            audio,
            info,
            base,
            pass,
            clock: Clock::default(),
        })
    }

    /// Reads the data of `sample`.
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

/// Reports whether the file at `path` starts with the `ftyp` box of an MP4 file.
pub(super) async fn is_mp4(path: &Path) -> bool {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut head = [0; 8];
        std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut head))
            .is_ok_and(|()| &head[4..] == b"ftyp")
    })
    .await
    .unwrap_or(false)
}

/// Returns the info of a video track.
///
/// The rate is the track's average, so a long first frame of a screen
/// recording does not set it.
fn video_info(track: &Track) -> VideoInfo {
    let seconds = |units: i64| units as f64 / f64::from(track.timescale);
    let span: i64 = track
        .samples
        .iter()
        .map(|sample| i64::from(sample.duration))
        .sum();
    let rate = if span > 0 {
        video::FrameRate::from_f64(track.samples.len() as f64 / seconds(span)).ok()
    } else {
        None
    }
    .unwrap_or_else(|| video::FrameRate::from_f64(FALLBACK_FPS).expect("a valid rate"));
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
        gop: Duration::from_secs_f64(seconds(gop).max(0.0)),
    }
}

/// Returns the catalog entry that opens moq-video's decoder for an H.264 track.
fn video_catalog(track: &Track) -> VideoConfig {
    let Codec::H264 { avcc } = &track.codec else {
        unreachable!("only H.264 tracks decode")
    };
    let byte = |index: usize| avcc.get(index).copied().unwrap_or(0);
    let mut config = VideoConfig::new(H264 {
        inline: false,
        profile: byte(1),
        constraints: byte(2),
        level: byte(3),
    });
    config.description = Some(avcc.clone());
    config.coded_width = Some(track.width);
    config.coded_height = Some(track.height);
    config
}

/// Returns the catalog entry that opens moq-audio's decoder for an AAC track.
fn audio_catalog(track: &Track) -> AudioConfig {
    let Codec::Aac { config } = &track.codec else {
        unreachable!("only AAC tracks decode")
    };
    // The audio object type is the first five bits of the AudioSpecificConfig.
    let profile = config.first().map_or(2, |byte| byte >> 3);
    let mut catalog = AudioConfig::new(AAC { profile }, track.sample_rate, track.channels);
    catalog.description = Some(config.clone());
    catalog
}

/// Returns `units` of `timescale` in microseconds.
fn micros(units: i64, timescale: u32) -> i64 {
    (i128::from(units) * 1_000_000 / i128::from(timescale)) as i64
}

/// Returns a media time as a timestamp.
fn to_timestamp(at: Duration) -> moq_net::Timestamp {
    moq_net::Timestamp::from_micros(at.as_micros() as u64)
        .expect("a file plays for far less than the timestamp range")
}

/// Waits until `due`, in short slices, and returns `false` early once `closed` says so.
///
/// A variable-rate file holds a still picture for as long as it likes, and
/// the decoder must not outlive its source by that long.
fn wait_until(due: Instant, closed: &impl Fn() -> bool) -> bool {
    loop {
        if closed() {
            return false;
        }
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(WAIT_SLICE));
    }
}

/// The wall clock every stream of a file plays against.
///
/// It starts with the first sample any stream asks for, so a file opened
/// early does not rush its first seconds out.
#[derive(Debug, Default)]
struct Clock {
    /// When media time zero is due.
    epoch: OnceLock<Instant>,
    /// How far playback skipped ahead after falling behind, in microseconds.
    skipped: AtomicU64,
}

impl Clock {
    /// Returns when media time `at` is due, starting the clock if it has not started.
    fn instant(&self, at: Duration) -> Instant {
        *self.epoch.get_or_init(Instant::now) + at
    }

    /// Returns how far playback has skipped ahead, which every media time adds.
    fn skipped(&self) -> Duration {
        Duration::from_micros(self.skipped.load(Ordering::Relaxed))
    }

    /// Skips ahead if a sample due at `due` comes more than [`MAX_BEHIND`] late.
    ///
    /// Moves every stream of the file, so they stay in sync.
    fn catch_up(&self, due: Instant) {
        let behind = Instant::now().saturating_duration_since(due);
        if behind > MAX_BEHIND {
            debug!(?behind, "file playback skips ahead");
            self.skipped
                .fetch_add(behind.as_micros() as u64, Ordering::Relaxed);
        }
    }
}

/// One sample with its times on the file's timeline.
#[derive(Debug, Clone, Copy)]
struct Scheduled {
    sample: Sample,
    /// Which pass over the file this is, from zero.
    pass: u64,
    /// When the sample decodes.
    decode_at: Duration,
    /// When the sample presents.
    timestamp: Duration,
}

/// The samples of one track in decode order, pass after pass for a looping file.
///
/// Every track of a file starts a pass at the same time, after the longest
/// track ended, so a shorter track pauses and the tracks stay in sync.
#[derive(Debug)]
struct Schedule {
    file: MediaFile,
    video: bool,
    index: usize,
    pass: u64,
}

impl Schedule {
    fn new(file: MediaFile, video: bool) -> Self {
        Self {
            file,
            video,
            index: 0,
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
        if self.index == track.samples.len() {
            if self.file.end == FileEnd::Stop {
                return None;
            }
            self.index = 0;
            self.pass += 1;
        }
        let sample = track.samples[self.index];
        self.index += 1;
        let at = |units: i64| {
            let offset = micros(units, track.timescale) - shared.base;
            shared.pass * self.pass as u32 + Duration::from_micros(offset.max(0) as u64)
        };
        Some(Scheduled {
            sample,
            pass: self.pass,
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
    ///   -movflags +faststart clip.mp4`.
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

    #[tokio::test]
    async fn a_plain_mp4_reports_its_tracks() {
        let file = MediaFile::open(clip("clip.mp4"))
            .await
            .expect("the clip opens");
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
    /// ffmpeg drops the audio's edit list when it fragments, so the tracks
    /// start at another offset from each other.
    #[tokio::test]
    async fn a_fragmented_mp4_matches_the_plain_one() {
        let plain = MediaFile::open(clip("clip.mp4"))
            .await
            .expect("the clip opens");
        let fragmented = MediaFile::open(clip("fragmented.mp4"))
            .await
            .expect("the fragmented clip opens");
        assert_eq!(plain.info().video, fragmented.info().video);
        assert_eq!(plain.info().audio, fragmented.info().audio);
        for video in [true, false] {
            let plain_samples: Vec<_> = Schedule::new(plain.clone(), video).collect();
            let fragmented_samples: Vec<_> = Schedule::new(fragmented.clone(), video).collect();
            assert_eq!(plain_samples.len(), fragmented_samples.len());
            let start = |samples: &[Scheduled]| samples[0].timestamp;
            let (plain_start, fragmented_start) =
                (start(&plain_samples), start(&fragmented_samples));
            for (a, b) in plain_samples.iter().zip(&fragmented_samples) {
                assert_eq!(a.timestamp - plain_start, b.timestamp - fragmented_start);
                assert_eq!(a.sample.keyframe, b.sample.keyframe);
                assert_eq!(
                    plain.shared.read(&a.sample).unwrap(),
                    fragmented.shared.read(&b.sample).unwrap()
                );
            }
        }
    }

    /// B-frames keep their presentation times, which the edit list starts at zero.
    #[tokio::test]
    async fn b_frames_keep_their_presentation_times() {
        let file = MediaFile::open(clip("bframes.mp4"))
            .await
            .expect("the clip opens");
        let pts: Vec<u64> = Schedule::new(file, true)
            .map(|next| next.timestamp.as_millis() as u64)
            .collect();
        // Decode order: I0 P3 B1 B2 P6 B4 B5 ...
        assert_eq!(&pts[..6], [0, 300, 100, 200, 600, 400]);
        let mut sorted = pts.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..10).map(|n| n * 100).collect::<Vec<_>>());
    }

    /// A looping file starts each pass one file length after the last, on every track.
    #[tokio::test]
    async fn a_looping_file_continues_its_timeline() {
        let file = MediaFile::open(clip("clip.mp4"))
            .await
            .expect("the clip opens")
            .with_end(FileEnd::Restart);
        let length = file.info().duration;
        for video in [true, false] {
            let count = Schedule::new(file.clone().with_end(FileEnd::Stop), video).count();
            let two: Vec<_> = Schedule::new(file.clone(), video).take(2 * count).collect();
            for (first, second) in two[..count].iter().zip(&two[count..]) {
                assert_eq!(second.pass, 1);
                assert_eq!(second.timestamp, first.timestamp + length);
                assert_eq!(second.decode_at, first.decode_at + length);
            }
        }
        // The audio's priming packet presents first, so the video starts after it.
        let first_video = Schedule::new(file, true).next().unwrap();
        assert_eq!(first_video.timestamp, Duration::from_millis(64));
    }

    /// Passthrough hands each access unit over when its decode time is due.
    #[tokio::test(start_paused = true)]
    async fn passthrough_paces_to_the_decode_times() {
        let file = MediaFile::open(clip("bframes.mp4"))
            .await
            .expect("the clip opens");
        let EncodedVideo::Avc1 { avcc, mut frames } = file.encoded_video().expect("H.264") else {
            panic!("not avc1");
        };
        assert_eq!(avcc[0], 1, "an avcC record starts with version 1");
        let start = Instant::now();
        let mut arrivals = Vec::new();
        while let Some(frame) = frames.next().await {
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

    #[tokio::test]
    async fn passthrough_refuses_another_codec() {
        let file = MediaFile::open(clip("bframes.mp4"))
            .await
            .expect("the clip opens");
        assert!(matches!(
            file.encoded_audio(),
            Err(Error::InvalidConfig { .. })
        ));
        let not_mp4 =
            MediaFile::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).await;
        assert!(matches!(not_mp4, Err(Error::Device { .. })));
    }

    /// Decoding refuses an odd or zero size before it opens anything.
    #[tokio::test]
    async fn decoding_refuses_odd_sizes() {
        let file = MediaFile::open(clip("clip.mp4"))
            .await
            .expect("the clip opens");
        for size in [video::Size::new(641, 360), video::Size::new(0, 48)] {
            let config = VideoFileConfig::default().with_size(size);
            assert!(matches!(
                file.video(config).await,
                Err(Error::InvalidConfig { .. })
            ));
        }
    }

    /// A looping B-frame file decodes to pictures of the asked size with rising timestamps.
    #[test]
    fn a_looping_b_frame_file_decodes_in_presentation_order() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let file = runtime
            .block_on(MediaFile::open(clip("bframes.mp4")))
            .expect("the clip opens")
            .with_end(FileEnd::Restart);
        let track = file.track(true, "H.264").unwrap();
        let mut config = video::decode::Config::new();
        config.output = video::Output::Cpu;
        let decoder = video::decode::Decoder::new(&video_catalog(track), &config).unwrap();
        let size = video::Size::new(32, 24);
        let mut frames = Vec::new();
        file.decode_video(
            decoder,
            Some(size),
            |frame| {
                frames.push(frame);
                frames.len() < 12
            },
            || false,
        )
        .expect("the clip plays");
        assert!(frames.iter().all(|frame| frame.size() == size));
        let millis: Vec<u64> = frames
            .iter()
            .map(|frame| (frame.timestamp.as_micros().unwrap() / 1000) as u64)
            .collect();
        assert_eq!(millis, (0..12).map(|n| n * 100).collect::<Vec<_>>());
    }

    /// The audio decodes to the file's rate and layout.
    #[tokio::test]
    async fn the_audio_decodes() {
        let file = MediaFile::open(clip("clip.mp4"))
            .await
            .expect("the clip opens");
        let source = file.audio().expect("the audio opens");
        let format = source.format().expect("PCM");
        assert_eq!(format.sample_rate, 16_000);
        assert_eq!(format.layout, audio::Layout::Mono);
    }
}
