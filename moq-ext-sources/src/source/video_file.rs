//! Decoding the video of a media file as a source.
//!
//! symphonia reads the container and moq-video decodes the H.264 track. The
//! decode thread hands over each picture when its timestamp comes due on the
//! wall clock, so the file plays like a camera. Looping reopens the file and
//! continues the timeline where the last pass ended, so timestamps only rise.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use bytes::Bytes;
use hang::catalog::{H264, VideoConfig};
use moq_video::decode::{Config, Decoder};
use symphonia::core::{
    codecs::{
        CodecParameters,
        video::{
            VideoCodecParameters, well_known::extra_data::VIDEO_EXTRA_DATA_ID_AVC_DECODER_CONFIG,
        },
    },
    formats::{FormatReader, Track},
    units::TimeBase,
};
use tracing::debug;

use super::{
    FileEnd,
    file::{file_error, open_reader},
};
use crate::{Error, video};

/// The frame rate of a file whose first sample declares no duration.
const FALLBACK_FPS: u32 = 30;

/// How far playback can fall behind the wall clock before it skips ahead.
///
/// A suspended host or a stalled disk causes this. Without the skip, the file
/// would play fast until it caught up.
const MAX_BEHIND: Duration = Duration::from_secs(1);

/// The picture size and frame rate a file declares, read before decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    /// The coded picture size.
    pub(crate) size: video::Size,
    /// The rate of the first sample's duration.
    pub(crate) rate: video::FrameRate,
}

/// Reads the size and rate of the H.264 track in `path`.
///
/// # Errors
///
/// Fails if the file cannot be read or holds no H.264 track.
pub(crate) fn probe(path: &Path) -> Result<Probe, Error> {
    let (mut reader, track) = open_track(path)?;
    let config = catalog(path, &track)?;
    let size = match (config.coded_width, config.coded_height) {
        (Some(width), Some(height)) => video::Size::new(width, height),
        _ => return Err(file_error(path, "the video declares no picture size")),
    };
    let time_base = time_base(path, &track)?;
    let mut rate = None;
    while let Some(packet) = reader
        .next_packet()
        .map_err(|source| file_error(path, source))?
    {
        if packet.track_id == track.id {
            rate = rate_of(time_base, packet.dur.get());
            break;
        }
    }
    let rate = rate
        .unwrap_or_else(|| video::FrameRate::new(FALLBACK_FPS, 1).expect("30 fps is a valid rate"));
    Ok(Probe { size, rate })
}

/// Decodes `path` and hands each picture, at `size`, to `push` when it is due.
///
/// Calls `opened` once the file and its decoder open, or with the error if
/// they do not, and then returns. Restarts the file at its end for
/// [`FileEnd::Restart`]. Returns when the file ends, or when `push` returns
/// `false` because the source closed.
///
/// # Errors
///
/// Fails if the file stops reading or the decoder fails after the open.
pub(crate) fn play(
    path: &Path,
    at_end: FileEnd,
    size: video::Size,
    opened: impl FnOnce(Result<(), Error>),
    mut push: impl FnMut(video::Frame) -> bool,
) -> Result<(), Error> {
    let (mut reader, mut track, mut decoder) = match open_decoder(path, size) {
        Ok(open) => {
            opened(Ok(()));
            open
        }
        Err(err) => {
            opened(Err(err));
            return Ok(());
        }
    };
    let mut pacer = Pacer::new(Instant::now());
    let mut timeline = Timeline::default();
    // Scales a picture, waits until it is due and hands it over.
    let mut show = |frame: video::Frame| -> Result<bool, Error> {
        let at = Duration::from_micros(frame.timestamp.as_micros().unwrap_or(0) as u64);
        let frame = if frame.size() == size {
            frame
        } else {
            frame
                .resize(size, &Default::default())
                .map_err(Error::decoder)?
        };
        std::thread::sleep(pacer.wait(at, Instant::now()));
        Ok(push(frame))
    };
    loop {
        let length_size = usize::from(avcc(path, &track)?[4] & 0x3) + 1;
        let time_base = time_base(path, &track)?;
        while let Some(packet) = reader
            .next_packet()
            .map_err(|source| file_error(path, source))?
        {
            if packet.track_id != track.id {
                continue;
            }
            let pts = to_duration(time_base, packet.pts.get().max(0) as u64);
            let length = to_duration(time_base, packet.dur.get());
            let at = timeline.at(pts, length);
            let timestamp = moq_net::Timestamp::from_micros(at.as_micros() as u64)
                .expect("a file plays for far less than the timestamp range");
            let keyframe = is_keyframe(&packet.data, length_size);
            decoder
                .decode(&Bytes::from(packet.data), timestamp, keyframe)
                .map_err(Error::decoder)?;
            while let Some(frame) = decoder.receive().map_err(Error::decoder)? {
                if !show(frame)? {
                    return Ok(());
                }
            }
        }
        decoder.flush().map_err(Error::decoder)?;
        while let Some(frame) = decoder.receive().map_err(Error::decoder)? {
            if !show(frame)? {
                return Ok(());
            }
        }
        if at_end == FileEnd::Stop {
            debug!(path = %path.display(), "video file ended");
            return Ok(());
        }
        if !timeline.next_pass() {
            return Err(file_error(path, "the video has no samples"));
        }
        debug!(path = %path.display(), "video file looping");
        (reader, track) = open_track(path)?;
    }
}

/// Opens `path` and a decoder for its video that scales toward `size`.
fn open_decoder(
    path: &Path,
    size: video::Size,
) -> Result<(Box<dyn FormatReader>, Track, Decoder), Error> {
    let (reader, track) = open_track(path)?;
    let mut config = Config::new();
    config.output = video::Output::Cpu;
    config.scale_hint = Some(size);
    let decoder = Decoder::new(&catalog(path, &track)?, &config).map_err(Error::decoder)?;
    Ok((reader, track, decoder))
}

/// Opens `path` and returns its container reader and first video track.
fn open_track(path: &Path) -> Result<(Box<dyn FormatReader>, Track), Error> {
    let reader = open_reader(path)?;
    let track = reader
        .tracks()
        .iter()
        .find(|track| video_params(track).is_some())
        .cloned()
        .ok_or_else(|| file_error(path, "the file holds no video track"))?;
    Ok((reader, track))
}

/// Returns the video codec parameters of `track`, if it is a video track.
fn video_params(track: &Track) -> Option<&VideoCodecParameters> {
    match track.codec_params.as_ref()? {
        CodecParameters::Video(params) => Some(params),
        _ => None,
    }
}

/// Returns the time base of `track`, which its timestamps count in.
fn time_base(path: &Path, track: &Track) -> Result<TimeBase, Error> {
    track
        .time_base
        .ok_or_else(|| file_error(path, "the video has no time base"))
}

/// Returns `units` of `time_base` as a duration.
fn to_duration(time_base: TimeBase, units: u64) -> Duration {
    let nanos = u128::from(units) * u128::from(time_base.numer.get()) * 1_000_000_000
        / u128::from(time_base.denom.get());
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Returns the frame rate of samples that last `units` of `time_base` each.
///
/// Returns `None` for a sample without a duration.
fn rate_of(time_base: TimeBase, units: u64) -> Option<video::FrameRate> {
    let interval = u64::from(time_base.numer.get()).checked_mul(units)?;
    video::FrameRate::new(time_base.denom.get(), u32::try_from(interval).ok()?).ok()
}

/// Returns the H.264 configuration record (avcC) of `track`.
fn avcc<'a>(path: &Path, track: &'a Track) -> Result<&'a [u8], Error> {
    video_params(track)
        .into_iter()
        .flat_map(|params| &params.extra_data)
        .find(|extra| extra.id == VIDEO_EXTRA_DATA_ID_AVC_DECODER_CONFIG)
        .map(|extra| &*extra.data)
        .filter(|avcc| avcc.len() >= 5)
        .ok_or_else(|| file_error(path, "the video is not H.264"))
}

/// Returns the catalog entry of the H.264 track, which opens the decoder.
fn catalog(path: &Path, track: &Track) -> Result<VideoConfig, Error> {
    let avcc = avcc(path, track)?;
    let mut config = VideoConfig::new(H264 {
        inline: false,
        profile: avcc[1],
        constraints: avcc[2],
        level: avcc[3],
    });
    config.description = Some(Bytes::copy_from_slice(avcc));
    if let Some(params) = video_params(track) {
        config.coded_width = params.width.map(u32::from);
        config.coded_height = params.height.map(u32::from);
    }
    Ok(config)
}

/// Returns whether the H.264 sample `data` holds an IDR slice.
///
/// The sample is a list of NAL units, each after its length in `length_size`
/// bytes.
fn is_keyframe(mut data: &[u8], length_size: usize) -> bool {
    while data.len() > length_size {
        let (length, rest) = data.split_at(length_size);
        let length = length
            .iter()
            .fold(0usize, |sum, byte| sum << 8 | usize::from(*byte));
        if rest.first().is_some_and(|header| header & 0x1f == 5) {
            return true;
        }
        data = rest.get(length..).unwrap_or_default();
    }
    false
}

/// The wall clock a file plays against.
#[derive(Debug)]
struct Pacer {
    started: Instant,
}

impl Pacer {
    /// Returns a clock on which media time zero is due at `now`.
    fn new(now: Instant) -> Self {
        Self { started: now }
    }

    /// Returns how long to wait at `now` until media time `at` is due.
    ///
    /// Playback more than [`MAX_BEHIND`] late moves the clock so that `at` is
    /// due now, and continues from there.
    fn wait(&mut self, at: Duration, now: Instant) -> Duration {
        let due = self.started + at;
        if let Some(ahead) = due.checked_duration_since(now) {
            return ahead;
        }
        if now - due > MAX_BEHIND {
            debug!(behind = ?(now - due), "file playback skips ahead");
            if let Some(started) = now.checked_sub(at) {
                self.started = started;
            }
        }
        Duration::ZERO
    }
}

/// The media time of a file that may play several times over.
///
/// Each pass starts where the last one ended, so timestamps only rise.
#[derive(Debug, Default)]
struct Timeline {
    /// Where the current pass starts.
    offset: Duration,
    /// Where the current pass ends so far.
    end: Duration,
    /// How many samples the current pass had.
    samples: usize,
}

impl Timeline {
    /// Returns the media time of a sample at `pts` in the current pass that lasts `length`.
    fn at(&mut self, pts: Duration, length: Duration) -> Duration {
        self.end = self.end.max(pts + length);
        self.samples += 1;
        self.offset + pts
    }

    /// Starts the next pass where the current one ended.
    ///
    /// Returns `false` if the current pass had no samples, since looping it
    /// would spin.
    fn next_pass(&mut self) -> bool {
        if self.samples == 0 {
            return false;
        }
        self.offset += self.end;
        self.end = Duration::ZERO;
        self.samples = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Returns the path of the test clip: 64x48 H.264 at 10 fps and 16 kHz mono AAC, one second each.
    ///
    /// Made with `ffmpeg -f lavfi -i testsrc2=size=64x48:rate=10:duration=1
    /// -f lavfi -i sine=frequency=440:sample_rate=16000:duration=1 -c:v
    /// libx264 -profile:v baseline -crf 40 -g 10 -pix_fmt yuv420p -c:a aac
    /// -b:a 16k -ac 1 -movflags +faststart clip.mp4`.
    fn clip() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/source/test_data/clip.mp4")
    }

    #[test]
    fn an_idr_slice_makes_a_keyframe() {
        // An SPS (7), then an IDR slice (5), each after a 4-byte length.
        let idr = [0, 0, 0, 2, 0x67, 0xaa, 0, 0, 0, 2, 0x65, 0xbb];
        assert!(is_keyframe(&idr, 4));
        // A non-IDR slice (1).
        let delta = [0, 0, 0, 2, 0x41, 0xbb];
        assert!(!is_keyframe(&delta, 4));
        // A cut sample ends the scan.
        assert!(!is_keyframe(&[0, 0, 0, 9, 0x41], 4));
    }

    /// The pacer waits for a frame ahead of the clock and not for one behind it.
    #[test]
    fn the_pacer_waits_for_frames_ahead_of_the_clock() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        let ms = Duration::from_millis;
        assert_eq!(pacer.wait(ms(100), start), ms(100));
        assert_eq!(pacer.wait(ms(100), start + ms(40)), ms(60));
        assert_eq!(pacer.wait(ms(100), start + ms(300)), Duration::ZERO);
        // Less than the limit behind: the clock stays, so the file catches up.
        assert_eq!(pacer.wait(ms(400), start + ms(400)), Duration::ZERO);
        assert_eq!(pacer.wait(ms(500), start + ms(400)), ms(100));
    }

    /// After a stall longer than the limit, the pacer continues from now instead of racing to catch up.
    #[test]
    fn the_pacer_skips_ahead_after_a_stall() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        let ms = Duration::from_millis;
        assert_eq!(pacer.wait(ms(100), start + ms(5_000)), Duration::ZERO);
        // The next frame is due one frame interval after the late one.
        assert_eq!(pacer.wait(ms(200), start + ms(5_000)), ms(100));
    }

    /// The rate of a sample's duration is exact, and a sample without one has none.
    #[test]
    fn the_rate_comes_from_the_sample_duration() {
        let base = |numer: u32, denom: u32| TimeBase {
            numer: numer.try_into().unwrap(),
            denom: denom.try_into().unwrap(),
        };
        assert_eq!(
            rate_of(base(1, 10_240), 1_024),
            Some(video::FrameRate::new(10, 1).unwrap())
        );
        assert_eq!(
            rate_of(base(1, 30_000), 1_001),
            Some(video::FrameRate::new(30_000, 1_001).unwrap())
        );
        assert_eq!(rate_of(base(1, 90_000), 0), None);
        assert_eq!(
            to_duration(base(1, 90_000), 9_000),
            Duration::from_millis(100)
        );
    }

    /// Each pass of a looping file continues the timeline where the last one ended.
    #[test]
    fn the_timeline_continues_across_passes() {
        let ms = Duration::from_millis;
        let mut timeline = Timeline::default();
        let first: Vec<_> = (0..3).map(|n| timeline.at(ms(n * 100), ms(100))).collect();
        assert_eq!(first, [ms(0), ms(100), ms(200)]);
        assert!(timeline.next_pass());
        let second: Vec<_> = (0..3).map(|n| timeline.at(ms(n * 100), ms(100))).collect();
        assert_eq!(second, [ms(300), ms(400), ms(500)]);
        assert!(timeline.next_pass());
        // A pass without samples does not loop.
        assert!(!timeline.next_pass());
    }

    #[test]
    fn the_probe_reads_size_and_rate() {
        let probe = probe(&clip()).expect("the clip opens");
        assert_eq!(probe.size, video::Size::new(64, 48));
        assert_eq!(probe.rate, video::FrameRate::new(10, 1).unwrap());
    }

    /// A file without a video track fails the probe.
    #[test]
    fn a_file_without_video_fails_the_probe() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(probe(&path).is_err());
    }

    /// A looping file decodes to pictures of the size asked for, with rising timestamps across the loop.
    #[test]
    fn a_looping_file_decodes_with_rising_timestamps() {
        let size = video::Size::new(32, 24);
        let mut opened = None;
        let mut frames = Vec::new();
        play(
            &clip(),
            FileEnd::Restart,
            size,
            |result| opened = Some(result.is_ok()),
            |frame| {
                frames.push(frame);
                frames.len() < 15
            },
        )
        .expect("the clip plays");
        assert_eq!(opened, Some(true));
        assert_eq!(frames.len(), 15);
        assert!(frames.iter().all(|frame| frame.size() == size));
        let micros: Vec<u64> = frames
            .iter()
            .map(|frame| frame.timestamp.as_micros().unwrap_or(0) as u64)
            .collect();
        assert!(
            micros.windows(2).all(|pair| pair[0] < pair[1]),
            "timestamps do not rise: {micros:?}"
        );
        // The second pass starts one second in, after the ten frames of the first.
        assert_eq!(micros[10], 1_000_000, "{micros:?}");
    }

    /// A file that does not loop ends after its last picture.
    #[test]
    fn a_file_that_does_not_loop_ends() {
        let mut count = 0;
        play(
            &clip(),
            FileEnd::Stop,
            video::Size::new(64, 48),
            |_| {},
            |_| {
                count += 1;
                true
            },
        )
        .expect("the clip plays");
        assert_eq!(count, 10);
    }
}
