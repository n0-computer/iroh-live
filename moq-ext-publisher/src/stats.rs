//! Snapshots of what a broadcast is sending.
//!
//! [`PublishStats`] is a plain value. A UI can read it with
//! [`LocalBroadcast::stats`](crate::LocalBroadcast::stats) as often as it
//! draws. Every figure has exactly one writer. Each rendition's encoder writes
//! its own entry, and the task that reads the source writes the source frame
//! rate. The frame and byte counters come from the catalog's own counters
//! ([`hang::stats::Rendition`]), read when the snapshot is taken.

use std::{collections::BTreeMap, time::Duration};

use moq_ext_media_core::stats::Cell;
use moq_net::bandwidth::Rate;

/// What a [`LocalBroadcast`](crate::LocalBroadcast) is sending.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PublishStats {
    /// Frames per second arriving from the video source, shared by every rendition.
    pub source_fps: Option<f32>,
    /// Each video rendition, by name.
    pub renditions: BTreeMap<String, EncodeStats>,
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
    /// Frames published on the rendition's track since the broadcast started.
    pub frames: u64,
    /// Media bytes published on the rendition's track since the broadcast started.
    pub bytes: u64,
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

/// The writers of one rendition's entry.
#[derive(Debug, Clone, Default)]
pub(crate) struct RenditionWriter {
    /// The encoder's own figures.
    pub(crate) stats: Cell<EncodeStats>,
    /// The name of the track the rendition writes now, its key in the catalog's counters.
    ///
    /// A frame that changes the decoder identity moves the rendition to a new track.
    pub(crate) track: Cell<String>,
}

/// The writers behind [`PublishStats`], one per figure.
#[derive(Debug, Clone, Default)]
pub(crate) struct PublishRecorder {
    source_fps: Cell<Option<f32>>,
    renditions: Cell<BTreeMap<String, RenditionWriter>>,
    audio: Cell<Option<Cell<AudioEncodeStats>>>,
}

impl PublishRecorder {
    /// Returns the snapshot, with the counters of each rendition's track from `sent`.
    pub(crate) fn snapshot(&self, sent: &BTreeMap<String, hang::stats::Rendition>) -> PublishStats {
        let rendition = |writer: RenditionWriter| {
            let mut stats = writer.stats.get();
            if let Some(sent) = sent.get(&writer.track.get()) {
                stats.frames = sent.frames;
                stats.bytes = sent.bytes;
            }
            stats
        };
        PublishStats {
            source_fps: self.source_fps.get(),
            renditions: self
                .renditions
                .get()
                .into_iter()
                .map(|(name, writer)| (name, rendition(writer)))
                .collect(),
            audio: self.audio.get().map(|cell| cell.get()),
        }
    }

    /// Returns the writer of the source frame rate.
    pub(crate) fn source_fps(&self) -> Cell<Option<f32>> {
        self.source_fps.clone()
    }

    /// Starts an entry for rendition `name`, written to track `track`, and returns its writer.
    pub(crate) fn rendition(&self, name: &str, track: &str) -> RenditionWriter {
        let writer = RenditionWriter::default();
        writer.track.update(|name| *name = track.to_string());
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

    fn sent(frames: u64) -> hang::stats::Rendition {
        let mut sent = hang::stats::Rendition::default();
        sent.frames = frames;
        sent
    }

    /// Each rendition reads the counters of its own track, so two renditions never mix.
    #[test]
    fn two_renditions_keep_separate_entries() {
        let recorder = PublishRecorder::default();
        let high = recorder.rendition("high", "high");
        let _low = recorder.rendition("low", "low");
        let mut counters = BTreeMap::from([
            ("high".to_string(), sent(30)),
            ("low".to_string(), sent(15)),
        ]);
        let snapshot = recorder.snapshot(&counters);
        assert_eq!(snapshot.renditions["high"].frames, 30);
        assert_eq!(snapshot.renditions["low"].frames, 15);
        // A rendition moved to a new track reads that track's counters.
        high.track.update(|track| *track = "high.1".to_string());
        counters.insert("high.1".to_string(), sent(2));
        assert_eq!(recorder.snapshot(&counters).renditions["high"].frames, 2);
        recorder.clear_video();
        assert!(recorder.snapshot(&counters).renditions.is_empty());
    }
}
