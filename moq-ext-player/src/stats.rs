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
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

use moq_ext_media_core::stats::Cell;

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
    /// This is the latency's minimum, the playout clock's delay.
    pub latency: Duration,
    /// A reading of the link, if the broadcast has network signals.
    pub network: Option<NetworkSample>,
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

impl From<&PlaybackStats> for BTreeMap<String, hang::echo::Rendition> {
    /// Returns the viewer feedback of each rendition playing, keyed by rendition.
    ///
    /// This is the `renditions` part of a [`hang::echo::Snapshot`]. Frames
    /// played count as received and decoded, skipped access units count as
    /// errors, and every entry carries the playout latency. The player keeps no
    /// byte counter, so `bytes` stays zero.
    fn from(stats: &PlaybackStats) -> Self {
        let entry = |rendition: &str, frames: u64, errors: u64| {
            let mut feedback = hang::echo::Rendition::default();
            feedback.frames = frames;
            feedback.decoded = frames;
            feedback.errors = errors;
            feedback.latency = Some(stats.latency);
            (rendition.to_string(), feedback)
        };
        let video = stats
            .video
            .as_ref()
            .map(|video| entry(&video.rendition, video.frames, video.skipped));
        let audio = stats
            .audio
            .as_ref()
            .map(|audio| entry(&audio.rendition, audio.frames, 0));
        video.into_iter().chain(audio).collect()
    }
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
/// of playback, or [`Player::video_timing`](crate::Player::video_timing) for one
/// picture. From `arrived` to `decoded` is the time in the decoder, and from
/// `decoded` to `presented` the time the playout clock held the frame. Pictures
/// and audio with the same timestamp should be presented together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    /// The medium.
    pub kind: MediaKind,
    /// The frame's presentation timestamp on the broadcast clock.
    pub pts: Duration,
    /// When the frame's data was complete on this node, before decoding.
    ///
    /// Set for pictures when [`PlayerConfig::record_arrivals`] is on. It is
    /// `None` otherwise, for audio, and for a picture whose data arrived
    /// before the player watched its track.
    ///
    /// [`PlayerConfig::record_arrivals`]: crate::PlayerConfig::record_arrivals
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

/// How many frames a [`FrameLog`] keeps per medium.
///
/// That is ten seconds of 50 Hz audio, and more of 30 fps video.
const TIMELINE_LEN: usize = 512;

/// The last frames of one medium, oldest first, written by the presenting task.
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameLog(Cell<VecDeque<FrameTiming>>);

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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each medium playing becomes one feedback entry under its rendition.
    #[test]
    fn playback_maps_onto_viewer_feedback() {
        let stats = PlaybackStats {
            video: Some(VideoPlaybackStats {
                rendition: "high".to_string(),
                frames: 30,
                skipped: 2,
                ..Default::default()
            }),
            audio: Some(AudioPlaybackStats {
                rendition: "voice".to_string(),
                frames: 50,
                ..Default::default()
            }),
            latency: Duration::from_millis(150),
            network: None,
        };
        let feedback = BTreeMap::<String, hang::echo::Rendition>::from(&stats);
        assert_eq!(feedback.len(), 2);
        assert_eq!(feedback["high"].decoded, 30);
        assert_eq!(feedback["high"].errors, 2);
        assert_eq!(feedback["voice"].frames, 50);
        assert_eq!(feedback["voice"].latency, Some(Duration::from_millis(150)));
    }
}
