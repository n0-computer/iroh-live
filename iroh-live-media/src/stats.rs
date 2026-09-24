//! Snapshots of what a broadcast and a player are doing.
//!
//! [`PublishStats`] and [`PlaybackStats`] are plain values, read with
//! `stats()` as often as a UI draws. Every figure in them has exactly one
//! writer: each rendition's encoder writes its own entry, the source's frame
//! rate is written by the one task that reads the source, and so on. The shared
//! counters every encoder used to write into, where the smoothed value sat
//! somewhere among the rungs and the labels named whichever wrote last, are
//! gone.
//!
//! Rates are counted over a window rather than derived from the gap between two
//! events: one late frame in a 30 fps stream reads as 50 by the gap and as 30
//! by the count, and averaging reciprocal gaps reads high besides.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{Bitrate, NetworkSample, video};

/// What a [`LocalBroadcast`](crate::LocalBroadcast) is sending.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PublishStats {
    /// Frames per second arriving from the video source, which every
    /// rendition of a ladder shares.
    pub source_fps: Option<f32>,
    /// Each video rendition, by name.
    pub renditions: BTreeMap<String, EncodeStats>,
    /// The audio publication, if there is one.
    pub audio: Option<AudioEncodeStats>,
}

/// One video rendition's encoder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EncodeStats {
    /// The encoder backend that opened, such as `openh264` or `vaapi`.
    pub encoder: Option<String>,
    /// The encoded picture size.
    pub size: Option<video::Size>,
    /// Frames encoded per second, over the last second.
    pub fps: Option<f32>,
    /// Bits published per second, over the last second.
    pub bitrate: Option<Bitrate>,
    /// How long one encode takes, smoothed.
    pub encode_time: Option<Duration>,
    /// Frames encoded since the rendition started.
    pub frames: u64,
    /// Bytes published since the rendition started.
    pub bytes: u64,
}

/// The audio publication.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioEncodeStats {
    /// The codec, such as `opus`.
    pub codec: Option<String>,
    /// PCM frames written to the encoder.
    pub frames: u64,
    /// PCM frames lost because the broadcast fell behind its source.
    pub dropped: u64,
}

/// What a [`Player`](crate::Player) is playing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackStats {
    /// The video decoder, while video plays.
    pub video: Option<VideoPlaybackStats>,
    /// The audio decoder, while audio plays.
    pub audio: Option<AudioPlaybackStats>,
    /// How far behind its arrival a picture is shown: the jitter allowance plus
    /// whatever audio is queued at the speaker.
    pub latency: Duration,
    /// The last reading of the link, if the broadcast has network signals.
    pub network: Option<NetworkSample>,
}

/// The video decoder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoPlaybackStats {
    /// The rendition on screen.
    pub rendition: String,
    /// The decoder backend running, such as `vaapi`.
    pub decoder: String,
    /// The decoded picture size.
    pub size: Option<video::Size>,
    /// Frames shown per second, over the last second.
    pub fps: Option<u32>,
    /// How long one transport read and decode take together, smoothed.
    pub decode_time: Option<Duration>,
    /// Frames shown since the player started.
    pub frames: u64,
    /// Access units the decoder refused and skipped.
    pub skipped: u64,
}

/// The audio decoder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioPlaybackStats {
    /// The rendition playing.
    pub rendition: String,
    /// How much audio is queued ahead of the speaker.
    pub buffered: Duration,
    /// The most recent peak level on a linear `0.0..=1.0` scale, for a meter.
    pub peak: f32,
    /// Frames played since the player started.
    pub frames: u64,
}

/// Which medium a [`FrameTiming`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// A decoded picture.
    Video,
    /// A decoded block of samples.
    Audio,
}

/// When one frame left its decoder and when it was presented, for a timeline
/// view of playback.
///
/// Read with [`Player::timeline`](crate::Player::timeline). The gap between
/// the two instants is what the playout clock held the frame for, and pictures
/// and audio with the same timestamp should be presented together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    /// The medium.
    pub kind: MediaKind,
    /// The frame's presentation timestamp on the broadcast clock.
    pub pts: Duration,
    /// When the decoder handed the frame over.
    pub decoded: Instant,
    /// When the frame was presented: a picture handed to the player's frames,
    /// or audio reaching the speaker, which is when it was written plus what
    /// was already queued ahead of it.
    pub presented: Instant,
}

/// How many frames a [`Timeline`] keeps per medium: ten seconds of 50 Hz audio,
/// and more than that of 30 fps video.
const TIMELINE_LEN: usize = 512;

/// The last frames of one medium, oldest first, written by the task that
/// presents them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Timeline(Cell<VecDeque<FrameTiming>>);

impl Timeline {
    /// Records one frame, dropping the oldest once full.
    pub(crate) fn push(&self, timing: FrameTiming) {
        self.0.update(|frames| {
            if frames.len() >= TIMELINE_LEN {
                frames.pop_front();
            }
            frames.push_back(timing);
        });
    }

    /// Returns a copy of every frame kept, oldest first.
    pub(crate) fn snapshot(&self) -> VecDeque<FrameTiming> {
        self.0.get()
    }
}

/// A value one task writes and anyone snapshots.
#[derive(Debug, Clone, Default)]
pub(crate) struct Cell<T>(Arc<Mutex<T>>);

impl<T: Clone> Cell<T> {
    /// Changes the value in place.
    pub(crate) fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.0.lock().expect("poisoned"));
    }

    /// Returns a copy of the value.
    pub(crate) fn get(&self) -> T {
        self.0.lock().expect("poisoned").clone()
    }
}

/// The writers behind [`PublishStats`], one per figure.
#[derive(Debug, Clone, Default)]
pub(crate) struct PublishRecorder {
    source_fps: Cell<Option<f32>>,
    renditions: Cell<BTreeMap<String, Cell<EncodeStats>>>,
    audio: Cell<Option<Cell<AudioEncodeStats>>>,
}

impl PublishRecorder {
    /// Returns the snapshot.
    pub(crate) fn snapshot(&self) -> PublishStats {
        PublishStats {
            source_fps: self.source_fps.get(),
            renditions: self
                .renditions
                .get()
                .into_iter()
                .map(|(name, cell)| (name, cell.get()))
                .collect(),
            audio: self.audio.get().map(|cell| cell.get()),
        }
    }

    /// Returns the writer of the source frame rate.
    pub(crate) fn source_fps(&self) -> Cell<Option<f32>> {
        self.source_fps.clone()
    }

    /// Starts an entry for rendition `name` and returns its writer.
    pub(crate) fn rendition(&self, name: &str) -> Cell<EncodeStats> {
        let cell = Cell::default();
        self.renditions
            .update(|renditions| drop(renditions.insert(name.to_string(), cell.clone())));
        cell
    }

    /// Drops every video entry, as a video slot is cleared.
    pub(crate) fn clear_video(&self) {
        self.renditions.update(BTreeMap::clear);
        self.source_fps.update(|fps| *fps = None);
    }

    /// Starts the audio entry and returns its writer.
    pub(crate) fn audio(&self) -> Cell<AudioEncodeStats> {
        let cell = Cell::default();
        self.audio.update(|audio| *audio = Some(cell.clone()));
        cell
    }

    /// Drops the audio entry, as the audio slot is cleared.
    pub(crate) fn clear_audio(&self) {
        self.audio.update(|audio| *audio = None);
    }
}

/// How long a [`RateMeter`] counts events before it has a figure.
///
/// A second is what a frame rate is quoted in, and it is short enough that a
/// stream which stops is seen to stop.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Counts events and reports how many happened per second, once per window.
///
/// Owned by the one task that sees the events, so it needs no lock.
#[derive(Debug)]
pub(crate) struct RateMeter {
    window: Duration,
    started: Instant,
    events: u32,
    amount: u64,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::over(RATE_WINDOW)
    }
}

impl RateMeter {
    /// Creates a meter that reports once per `window`.
    pub(crate) fn over(window: Duration) -> Self {
        Self::starting(window, Instant::now())
    }

    /// A meter whose window opened at `started`.
    fn starting(window: Duration, started: Instant) -> Self {
        Self {
            window,
            started,
            events: 0,
            amount: 0,
        }
    }

    /// Counts one event carrying `amount` (bytes, say), and returns events and
    /// amount per second once the window has closed.
    pub(crate) fn tick(&mut self, amount: u64) -> Option<(f64, f64)> {
        self.tick_at(amount, Instant::now())
    }

    fn tick_at(&mut self, amount: u64, now: Instant) -> Option<(f64, f64)> {
        self.events += 1;
        self.amount += amount;
        let elapsed = now.duration_since(self.started);
        if elapsed < self.window {
            return None;
        }
        let seconds = elapsed.as_secs_f64();
        let rates = (
            f64::from(self.events) / seconds,
            self.amount as f64 / seconds,
        );
        // The window restarts at the reading, so the time spent computing does
        // not accumulate across windows.
        *self = Self::starting(self.window, now);
        Some(rates)
    }
}

/// An exponentially smoothed duration, for timings that jitter per call.
#[derive(Debug, Default)]
pub(crate) struct Smoothed(Option<f64>);

impl Smoothed {
    /// Weight of a new sample.
    const ALPHA: f64 = 0.2;

    /// Folds `sample` in and returns the smoothed value.
    pub(crate) fn record(&mut self, sample: Duration) -> Duration {
        let value = sample.as_secs_f64();
        let smoothed = match self.0 {
            Some(previous) => Self::ALPHA * value + (1.0 - Self::ALPHA) * previous,
            None => value,
        };
        self.0 = Some(smoothed);
        Duration::from_secs_f64(smoothed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs forty events at offsets from `offset` and returns the first rate.
    fn drive(offset: impl Fn(u32) -> Duration) -> f64 {
        let start = Instant::now();
        let mut meter = RateMeter::starting(RATE_WINDOW, start);
        (0..40)
            .find_map(|event| meter.tick_at(0, start + offset(event)))
            .expect("the offsets span the window")
            .0
    }

    /// Counted over a window, bursty delivery reads the same as even delivery.
    /// The old reciprocal-of-one-gap measure read this stream at about 36 fps
    /// while it swung between 21 and 50.
    #[test]
    fn a_burst_reads_the_same_as_an_even_stream() {
        let bursty = drive(|event| Duration::from_millis(350) * (event / 10));
        let even = drive(|event| Duration::from_millis(35) * event);
        assert!((bursty - even).abs() < 0.5, "{bursty} against {even}");
        assert!((28.0..32.0).contains(&bursty), "{bursty}");
    }

    #[test]
    fn the_amount_is_a_rate_too() {
        let start = Instant::now();
        let mut meter = RateMeter::starting(RATE_WINDOW, start);
        for tick in 0..30u32 {
            if let Some((_, bytes)) = meter.tick_at(2_500, start + Duration::from_millis(33) * tick)
            {
                // 2500 bytes every 33 ms is about 75 kB a second.
                assert!((70_000.0..80_000.0).contains(&bytes), "{bytes}");
                return;
            }
        }
        let (_, bytes) = meter
            .tick_at(2_500, start + Duration::from_secs(1))
            .expect("a second has passed");
        assert!((70_000.0..82_000.0).contains(&bytes), "{bytes}");
    }

    /// Each rendition writes its own entry, so two rungs never mix.
    #[test]
    fn two_renditions_keep_separate_entries() {
        let recorder = PublishRecorder::default();
        let high = recorder.rendition("high");
        let low = recorder.rendition("low");
        high.update(|stats| stats.frames = 30);
        low.update(|stats| stats.frames = 15);
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.renditions["high"].frames, 30);
        assert_eq!(snapshot.renditions["low"].frames, 15);
        recorder.clear_video();
        assert!(recorder.snapshot().renditions.is_empty());
    }

    #[test]
    fn smoothing_starts_at_the_first_sample() {
        let mut smoothed = Smoothed::default();
        assert_eq!(
            smoothed.record(Duration::from_millis(10)),
            Duration::from_millis(10)
        );
        let next = smoothed.record(Duration::from_millis(20));
        assert!(next > Duration::from_millis(10) && next < Duration::from_millis(20));
    }
}
