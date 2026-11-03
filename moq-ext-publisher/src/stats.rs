//! Snapshots of what a broadcast is sending.
//!
//! [`PublishStats`] is a plain value. A UI can read it with
//! [`LocalBroadcast::stats`](crate::LocalBroadcast::stats) as often as it
//! draws. Every figure has exactly one writer. Each rendition's encoder writes
//! its own entry, and the task that reads the source writes the source frame
//! rate.

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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EncodeStats {
    /// The encoder backend that opened, such as `openh264` or `vaapi`.
    pub encoder: Option<String>,
    /// The encoded picture size.
    pub size: Option<moq_video::Size>,
    /// Frames encoded per second, over the last second.
    pub fps: Option<f32>,
    /// Bits published per second, over the last second.
    pub bitrate: Option<Rate>,
    /// How long one encode takes, smoothed.
    pub encode_time: Option<Duration>,
    /// Frames encoded since the rendition started.
    pub frames: u64,
    /// Bytes published since the rendition started.
    pub bytes: u64,
}

impl EncodeStats {
    /// Counts one encoded frame of `bytes` and stores the rates from a [`RateMeter`].
    ///
    /// `rates` holds frames and bytes per second.
    pub(crate) fn record(&mut self, bytes: u64, rates: Option<(f64, f64)>) {
        self.frames += 1;
        self.bytes += bytes;
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

    /// Each rendition writes its own entry, so two renditions never mix.
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
}
