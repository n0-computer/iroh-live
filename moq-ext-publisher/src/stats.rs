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
    ///
    /// `None` before the first second of frames, once the source ends, and
    /// within two seconds of the last frame of a source that stalls.
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
    ///
    /// `None` before the first second of frames, once the encoder stops, and
    /// within two seconds of the last frame of an encoder that stalls.
    pub fps: Option<f32>,
    /// Bits published per second, over the last second.
    ///
    /// `None` whenever [`fps`](Self::fps) is.
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

/// One rendition's figures as its encoder writes them.
///
/// The rates are kept apart, so a snapshot can drop them once they are stale.
#[derive(Debug, Clone, Default)]
pub(crate) struct RenditionEntry {
    /// Every figure but the rates.
    pub(crate) stats: EncodeStats,
    /// The last rates of the meter that counts frames and their bytes.
    rates: Option<Rates>,
}

impl RenditionEntry {
    /// Stores the rates of a meter that counts frames and their bytes, if it reported.
    pub(crate) fn record(&mut self, rates: Option<Rates>) {
        if rates.is_some() {
            self.rates = rates;
        }
    }

    /// Forgets the rates, when the encoder stops.
    pub(crate) fn stopped(&mut self) {
        self.rates = None;
    }

    /// Returns the figures, with rates only while they are current.
    fn snapshot(&self) -> EncodeStats {
        let rates = self.rates.filter(Rates::is_current);
        EncodeStats {
            fps: rates.map(|rates| rates.events as f32),
            bitrate: rates.map(|rates| Rate::from_bps((rates.amount * 8.0) as u64)),
            ..self.stats.clone()
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
                let micros =
                    crate::publish::micros(timestamp).saturating_sub(crate::publish::micros(last));
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
    source_fps: SharedCell<Option<Rates>>,
    renditions: SharedCell<BTreeMap<String, SharedCell<RenditionEntry>>>,
    audio: SharedCell<Option<SharedCell<AudioEncodeStats>>>,
}

impl PublishRecorder {
    /// Returns the snapshot, with `sent` from the catalog.
    pub(crate) fn snapshot(&self, sent: BTreeMap<String, hang::stats::Rendition>) -> PublishStats {
        PublishStats {
            source_fps: self
                .source_fps
                .get()
                .filter(Rates::is_current)
                .map(|rates| rates.events as f32),
            renditions: self
                .renditions
                .get()
                .into_iter()
                .map(|(name, writer)| (name, writer.get().snapshot()))
                .collect(),
            sent,
            audio: self.audio.get().map(|cell| cell.get()),
        }
    }

    /// Returns the writer of the source frame rate.
    ///
    /// A snapshot reads the rate as `None` once it is stale.
    pub(crate) fn source_fps(&self) -> SharedCell<Option<Rates>> {
        self.source_fps.clone()
    }

    /// Starts an entry for rendition `name` and returns its writer.
    ///
    /// The entry names the track `name` until the encoder reports another.
    pub(crate) fn rendition(&self, name: &str) -> SharedCell<RenditionEntry> {
        let writer = SharedCell::<RenditionEntry>::default();
        writer.update(|entry| entry.stats.track = name.to_string());
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
    use moq_ext_sources::stats::RateMeter;

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
        high.update(|entry| entry.stats.track = "high.1".to_string());
        assert_eq!(
            recorder.snapshot(BTreeMap::new()).renditions["high"].track,
            "high.1"
        );
        recorder.clear_video();
        assert!(recorder.snapshot(BTreeMap::new()).renditions.is_empty());
    }

    /// Counts `frames` at 20 per second, writing the rates as the encoder and the source reader do.
    async fn run_at_20fps(
        recorder: &PublishRecorder,
        entry: &SharedCell<RenditionEntry>,
        frames: u32,
    ) {
        let source_fps = recorder.source_fps();
        let mut source = RateMeter::default();
        let mut encoded = RateMeter::default();
        for _ in 0..frames {
            tokio::time::advance(Duration::from_millis(50)).await;
            if let Some(rates) = source.tick(0) {
                source_fps.update(|value| *value = Some(rates));
            }
            let rates = encoded.tick(1000);
            entry.update(|entry| entry.record(rates));
        }
    }

    /// The rates read right while frames arrive, and `None` once they stop or the source ends.
    #[tokio::test(start_paused = true)]
    async fn rates_go_away_once_frames_stop() {
        let recorder = PublishRecorder::default();
        let entry = recorder.rendition("high");
        run_at_20fps(&recorder, &entry, 30).await;
        let running = recorder.snapshot(BTreeMap::new());
        let fps = running.source_fps.expect("a running source has a rate");
        assert!((19.0..21.0).contains(&fps), "{fps}");
        let rendition = &running.renditions["high"];
        let fps = rendition.fps.expect("a running encoder has a rate");
        assert!((19.0..21.0).contains(&fps), "{fps}");
        let bitrate = rendition.bitrate.expect("a bitrate").as_bps();
        assert!((152_000..168_000).contains(&bitrate), "{bitrate}");

        // The source stalls.
        tokio::time::advance(Duration::from_secs(2)).await;
        let stalled = recorder.snapshot(BTreeMap::new());
        assert_eq!(stalled.source_fps, None);
        assert_eq!(stalled.renditions["high"].fps, None);
        assert_eq!(stalled.renditions["high"].bitrate, None);

        // It runs again, then ends.
        run_at_20fps(&recorder, &entry, 30).await;
        assert!(recorder.snapshot(BTreeMap::new()).source_fps.is_some());
        recorder.source_fps().update(|value| *value = None);
        entry.update(RenditionEntry::stopped);
        let ended = recorder.snapshot(BTreeMap::new());
        assert_eq!(ended.source_fps, None);
        assert_eq!(ended.renditions["high"].fps, None);
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
