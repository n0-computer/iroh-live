//! Snapshots of what a player is doing.
//!
//! [`PlaybackStats`] is a plain value. A UI can read it with
//! `Player::stats()` as often as it draws. Every figure has exactly one
//! writer: the video task writes the video figures, and the audio task writes
//! the audio figures.
//!
//! Rates are counted over a window instead of derived from the gap between two
//! events. One late frame in a 30 fps stream reads as 50 by the gap and as 30
//! by the count.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use moq_ext_sources::stats::SharedCell;

use crate::NetworkSample;

/// What a [`Player`](crate::Player) is playing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackStats {
    /// The video decoder, while video plays.
    pub video: Option<VideoPlaybackStats>,
    /// The audio decoder, while audio plays.
    pub audio: Option<AudioPlaybackStats>,
    /// How far behind the live edge playback runs.
    ///
    /// This is the playout clock's delay: the latency's minimum, raised by the
    /// catalog `delay` and `jitter` and by the jitter the audio decoder measures.
    pub latency: Duration,
    /// A reading of the link, if the broadcast has network signals.
    pub network: Option<NetworkSample>,
}

impl PlaybackStats {
    /// Returns a one-line summary for a status bar or a log.
    ///
    /// It names the rendition on screen with its size and frame rate, then
    /// the round trip and the playout latency. A figure not known yet shows
    /// as `-`.
    #[must_use]
    pub fn summary(&self) -> String {
        let video = match &self.video {
            Some(video) => {
                let size = video
                    .size
                    .map_or_else(|| "-".to_string(), |size| size.to_string());
                let fps = video
                    .fps
                    .map_or_else(|| "-".to_string(), |fps| fps.to_string());
                format!("{} {size} {fps}fps", video.rendition)
            }
            None => "no video".to_string(),
        };
        let rtt = self
            .network
            .and_then(|network| network.rtt)
            .map_or_else(|| "-".to_string(), |rtt| rtt.as_millis().to_string());
        format!("{video} | rtt:{rtt}ms lat:{}ms", self.latency.as_millis())
    }
}

/// The video decoder.
///
/// The running decoder backend is in [`PlayerStatus::decoder`](crate::PlayerStatus::decoder).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoPlaybackStats {
    /// The rendition on screen.
    pub rendition: String,
    /// The decoded picture size.
    pub size: Option<moq_video::Size>,
    /// Frames shown per second, over the last second.
    ///
    /// `None` before the first second of pictures, and from at most two
    /// seconds after the last picture shown.
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
    ///
    /// With a speaker, this is the speaker stream's peak after the volume.
    /// Without one, it is the peak of the audio [`rms`](Self::rms) spans.
    /// It reads `0.0` once [`rms`](Self::rms) reads `None`.
    pub peak: f32,
    /// The RMS level of the last 100 ms of decoded audio, on a linear `0.0..=1.0` scale.
    ///
    /// Measured before the speaker's volume, with or without a speaker, so a
    /// player in `AudioMode::Decode` can drive a level meter or a speaking
    /// indicator. `None` before the first frame, and after 300 ms without one.
    pub rms: Option<f32>,
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

/// When one frame arrived, left its decoder, and was presented.
///
/// Read with [`Player::timeline`](crate::Player::timeline) for a timeline view
/// of playback, [`Player::video_timing`](crate::Player::video_timing) for one
/// picture, [`Player::take_latest_video`](crate::Player::take_latest_video)
/// with the newest picture, or [`VideoBacklog::take`](crate::VideoBacklog::take)
/// with every picture. From `arrived` to `decoded` is the time in the
/// decoder, and from `decoded` to `presented` the time the playout clock held
/// the frame. Pictures and audio with the same timestamp should be presented
/// together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    /// The medium.
    pub kind: MediaKind,
    /// The frame's presentation timestamp on the broadcast clock.
    ///
    /// This is the frame's `timestamp` as a [`Duration`], so timings sort and
    /// subtract. [`Player::video_timing`](crate::Player::video_timing) takes
    /// the frame's `timestamp` itself.
    pub pts: Duration,
    /// When the frame's data was complete on this node, before decoding.
    ///
    /// Set for pictures, from the decoder's
    /// [`arrival`](moq_video::decode::Consumer::arrival). It is `None` for
    /// audio, and for a picture the decoder could not match to one access
    /// unit.
    pub arrived: Option<Instant>,
    /// When the decoder handed the frame over.
    pub decoded: Instant,
    /// When the frame was presented.
    ///
    /// A picture is presented when it is handed to the player's frames. Audio
    /// is presented when it reaches the speaker: when it was written plus the
    /// audio already queued ahead of it.
    pub presented: Instant,
}

impl FrameTiming {
    /// Returns how long the frame spent in the decoder, from `arrived` to `decoded`.
    ///
    /// `None` when the arrival is not known.
    #[must_use]
    pub fn decode_time(&self) -> Option<Duration> {
        self.arrived
            .map(|arrived| self.decoded.saturating_duration_since(arrived))
    }

    /// Returns how long the playout clock held the frame, from `decoded` to `presented`.
    #[must_use]
    pub fn queued(&self) -> Duration {
        self.presented.saturating_duration_since(self.decoded)
    }
}

/// How many frames a [`FrameLog`] keeps per medium.
///
/// That is ten seconds of 50 Hz audio, and more of 30 fps video.
const TIMELINE_LEN: usize = 512;

/// The last frames of one medium, oldest first, written by the presenting task.
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameLog(SharedCell<VecDeque<FrameTiming>>);

impl FrameLog {
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

    /// Returns when the newest frame kept left its decoder.
    pub(crate) fn last_decoded(&self) -> Option<Instant> {
        let mut decoded = None;
        self.0
            .update(|frames| decoded = frames.back().map(|timing| timing.decoded));
        decoded
    }

    /// Returns the newest frame kept at `pts`, without copying the log.
    pub(crate) fn find(&self, pts: Duration) -> Option<FrameTiming> {
        let mut found = None;
        self.0.update(|frames| {
            found = frames
                .iter()
                .rev()
                .find(|timing| timing.pts == pts)
                .copied();
        });
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helpers split a frame's time into the decoder's part and the clock's part.
    #[test]
    fn a_timing_splits_into_decode_time_and_queued() {
        let arrived = Instant::now();
        let mut timing = FrameTiming {
            kind: MediaKind::Video,
            pts: Duration::ZERO,
            arrived: Some(arrived),
            decoded: arrived + Duration::from_millis(5),
            presented: arrived + Duration::from_millis(45),
        };
        assert_eq!(timing.decode_time(), Some(Duration::from_millis(5)));
        assert_eq!(timing.queued(), Duration::from_millis(40));
        timing.arrived = None;
        assert_eq!(timing.decode_time(), None);
    }

    /// The summary names what it knows and marks the rest.
    #[test]
    fn the_summary_marks_figures_not_known_yet() {
        let mut stats = PlaybackStats {
            latency: Duration::from_millis(150),
            ..Default::default()
        };
        assert_eq!(stats.summary(), "no video | rtt:-ms lat:150ms");
        stats.video = Some(VideoPlaybackStats {
            rendition: "high".to_string(),
            size: Some(moq_video::Size::new(1280, 720)),
            fps: Some(30),
            ..Default::default()
        });
        assert_eq!(stats.summary(), "high 1280x720 30fps | rtt:-ms lat:150ms");
    }
}
