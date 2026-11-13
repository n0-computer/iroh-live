//! Snapshots of what a broadcast is sending.
//!
//! [`PublishStats`] is a plain value. A UI can read it with
//! [`LocalBroadcast::stats`](crate::LocalBroadcast::stats) as often as it
//! draws. Every figure has exactly one writer. Each rendition's encoder writes
//! its own entry, and the task that reads the source writes the source frame
//! rate. What each track has sent is the catalog's own count
//! ([`hang::stats::Rendition`]), read when the snapshot is taken.

use std::{collections::BTreeMap, time::Duration};

use moq_ext_sources::stats::{Rates, SharedCell};
use moq_net::bandwidth::Rate;

/// What a [`LocalBroadcast`](crate::LocalBroadcast) is sending.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PublishStats {
    /// Frames per second arriving from the video source, shared by every rendition.
    pub source_fps: Option<f32>,
    /// Each video rendition's encoder, by rendition name.
    pub renditions: BTreeMap<String, EncodeStats>,
    /// What each track has sent, by track name, as the catalog counts it.
    ///
    /// A rendition's [`EncodeStats::track`] names its entry.
    pub sent: BTreeMap<String, hang::stats::Rendition>,
    /// The audio publication, if there is one.
    pub audio: Option<AudioEncodeStats>,
}

/// One video rendition's encoder.
///
/// The running encoder backend is in the rendition's
/// [`RenditionState`](crate::RenditionState).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EncodeStats {
    /// The encoded picture size.
    pub size: Option<moq_video::Size>,
    /// Frames encoded per second, over the last second.
    pub fps: Option<f32>,
    /// Bits published per second, over the last second.
    pub bitrate: Option<Rate>,
    /// How long one encode takes, smoothed.
    pub encode_time: Option<Duration>,
    /// The size of the last keyframe in bytes.
    pub keyframe_bytes: Option<u64>,
    /// The size of a delta frame in bytes, smoothed.
    ///
    /// `None` while every frame is a keyframe.
    pub delta_bytes: Option<u64>,
    /// The time between the last two keyframes, by their timestamps.
    ///
    /// It shows the keyframe interval the encoder keeps, which a cut for a
    /// new source or a slower frame rate can move away from the configured
    /// one.
    pub keyframe_interval: Option<Duration>,
    /// The track the rendition writes now, its key in [`PublishStats::sent`].
    ///
    /// A keyframe with another decoder identity, such as a new level after a
    /// switch to a larger source, moves the rendition to a newly named track.
    pub track: String,
}

impl EncodeStats {
    /// Stores the rates of a meter that counts frames and their bytes.
    pub(crate) fn record(&mut self, rates: Option<Rates>) {
        if let Some(rates) = rates {
            self.fps = Some(rates.events as f32);
            self.bitrate = Some(Rate::from_bps((rates.amount * 8.0) as u64));
        }
    }
}

/// The tracker of one rendition's encoded frame sizes and keyframe spacing.
///
/// It fills the frame figures of the rendition's [`EncodeStats`].
#[derive(Debug, Default)]
pub(crate) struct FrameSizes {
    /// The timestamp of the last keyframe.
    last_keyframe: Option<moq_net::Timestamp>,
    /// The smoothed delta frame size in bytes.
    delta: Option<f64>,
}

impl FrameSizes {
    /// The weight of a new delta frame in the smoothed size.
    const ALPHA: f64 = 0.2;

    /// Records an encoded frame of `bytes` at `timestamp` in `stats`.
    pub(crate) fn record(
        &mut self,
        stats: &mut EncodeStats,
        bytes: usize,
        keyframe: bool,
        timestamp: moq_net::Timestamp,
    ) {
        let bytes = bytes as u64;
        if keyframe {
            stats.keyframe_bytes = Some(bytes);
            if let Some(last) = self.last_keyframe.replace(timestamp) {
                let micros = timestamp.as_micros().saturating_sub(last.as_micros());
                stats.keyframe_interval = Some(Duration::from_micros(micros as u64));
            }
        } else {
            let delta = match self.delta {
                Some(delta) => Self::ALPHA * bytes as f64 + (1.0 - Self::ALPHA) * delta,
                None => bytes as f64,
            };
            self.delta = Some(delta);
            stats.delta_bytes = Some(delta.round() as u64);
        }
    }
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

/// The writers behind [`PublishStats`], one per figure.
#[derive(Debug, Clone, Default)]
pub(crate) struct PublishRecorder {
    source_fps: SharedCell<Option<f32>>,
    renditions: SharedCell<BTreeMap<String, SharedCell<EncodeStats>>>,
    audio: SharedCell<Option<SharedCell<AudioEncodeStats>>>,
}

impl PublishRecorder {
    /// Returns the snapshot, with `sent` from the catalog.
    pub(crate) fn snapshot(&self, sent: BTreeMap<String, hang::stats::Rendition>) -> PublishStats {
        PublishStats {
            source_fps: self.source_fps.get(),
            renditions: self
                .renditions
                .get()
                .into_iter()
                .map(|(name, writer)| (name, writer.get()))
                .collect(),
            sent,
            audio: self.audio.get().map(|cell| cell.get()),
        }
    }

    /// Returns the writer of the source frame rate.
    pub(crate) fn source_fps(&self) -> SharedCell<Option<f32>> {
        self.source_fps.clone()
    }

    /// Starts an entry for rendition `name` and returns its writer.
    ///
    /// The entry names the track `name` until the encoder reports another.
    pub(crate) fn rendition(&self, name: &str) -> SharedCell<EncodeStats> {
        let writer = SharedCell::<EncodeStats>::default();
        writer.update(|stats| stats.track = name.to_string());
        self.renditions
            .update(|renditions| drop(renditions.insert(name.to_string(), writer.clone())));
        writer
    }

    /// Drops every video entry when the video slot is cleared.
    pub(crate) fn clear_video(&self) {
        self.renditions.update(BTreeMap::clear);
        self.source_fps.update(|fps| *fps = None);
    }

    /// Starts the audio entry and returns its writer.
    pub(crate) fn audio(&self) -> SharedCell<AudioEncodeStats> {
        let cell = SharedCell::default();
        self.audio.update(|audio| *audio = Some(cell.clone()));
        cell
    }

    /// Drops the audio entry when the audio slot is cleared.
    pub(crate) fn clear_audio(&self) {
        self.audio.update(|audio| *audio = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each rendition names the track it writes, and a cleared video drops the entries.
    #[test]
    fn a_rendition_names_its_track() {
        let recorder = PublishRecorder::default();
        let high = recorder.rendition("high");
        let _low = recorder.rendition("low");
        let snapshot = recorder.snapshot(BTreeMap::new());
        assert_eq!(snapshot.renditions["high"].track, "high");
        assert_eq!(snapshot.renditions["low"].track, "low");
        // A rendition moved to a new track names that track.
        high.update(|stats| stats.track = "high.1".to_string());
        assert_eq!(
            recorder.snapshot(BTreeMap::new()).renditions["high"].track,
            "high.1"
        );
        recorder.clear_video();
        assert!(recorder.snapshot(BTreeMap::new()).renditions.is_empty());
    }

    /// Keyframes and delta frames are sized apart, and keyframes are spaced by their timestamps.
    #[test]
    fn frame_sizes_follow_keyframes_and_deltas() {
        let at = |millis: u64| moq_net::Timestamp::from_micros(millis * 1000).expect("in range");
        let mut sizes = FrameSizes::default();
        let mut stats = EncodeStats::default();
        sizes.record(&mut stats, 9000, true, at(0));
        assert_eq!(stats.keyframe_bytes, Some(9000));
        assert_eq!(stats.delta_bytes, None);
        assert_eq!(
            stats.keyframe_interval, None,
            "one keyframe has no interval"
        );
        sizes.record(&mut stats, 1000, false, at(100));
        sizes.record(&mut stats, 2000, false, at(200));
        assert_eq!(
            stats.delta_bytes,
            Some(1200),
            "smoothed: 0.2 * 2000 + 0.8 * 1000"
        );
        sizes.record(&mut stats, 8000, true, at(500));
        assert_eq!(stats.keyframe_bytes, Some(8000));
        assert_eq!(stats.keyframe_interval, Some(Duration::from_millis(500)));
    }
}
