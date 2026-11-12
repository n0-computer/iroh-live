//! Snapshots of what a broadcast is sending.
//!
//! [`PublishStats`] is a plain value. A UI can read it with
//! [`LocalBroadcast::stats`](crate::LocalBroadcast::stats) as often as it
//! draws. Every figure has exactly one writer. Each rendition's encoder writes
//! its own entry, and the task that reads the source writes the source frame
//! rate. What each track has sent is the catalog's own count
//! ([`hang::stats::Rendition`]), read when the snapshot is taken.

use std::{collections::BTreeMap, time::Duration};

use moq_ext_sources::stats::Cell;
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
    /// The track the rendition writes now, its key in [`PublishStats::sent`].
    ///
    /// A keyframe with another decoder identity, such as a new level after a
    /// switch to a larger source, moves the rendition to a newly named track.
    pub track: String,
}

impl EncodeStats {
    /// Stores the rates of a `RateMeter`, which holds frames and bytes per second.
    pub(crate) fn record(&mut self, rates: Option<(f64, f64)>) {
        if let Some((fps, bytes_per_second)) = rates {
            self.fps = Some(fps as f32);
            self.bitrate = Some(Rate::from_bps((bytes_per_second * 8.0) as u64));
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
    source_fps: Cell<Option<f32>>,
    renditions: Cell<BTreeMap<String, Cell<EncodeStats>>>,
    audio: Cell<Option<Cell<AudioEncodeStats>>>,
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
    pub(crate) fn source_fps(&self) -> Cell<Option<f32>> {
        self.source_fps.clone()
    }

    /// Starts an entry for rendition `name`, written to track `track`, and returns its writer.
    pub(crate) fn rendition(&self, name: &str, track: &str) -> Cell<EncodeStats> {
        let writer = Cell::<EncodeStats>::default();
        writer.update(|stats| stats.track = track.to_string());
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
    pub(crate) fn audio(&self) -> Cell<AudioEncodeStats> {
        let cell = Cell::default();
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
        let high = recorder.rendition("high", "high");
        let _low = recorder.rendition("low", "low");
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
}
