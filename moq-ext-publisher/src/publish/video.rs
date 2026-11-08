//! The video publish task: one source, one encoder per rendition.
//!
//! A camera opens only once, and every rendition needs the same picture. Each
//! rendition's encoder reads the source through its own latest-wins handle, so
//! a rendition that falls behind drops frames without stalling the others.
//!
//! A rendition encodes only while someone watches it. The source runs even
//! when nobody watches, because a preview reads the same frames and a
//! publisher expects to see itself before anyone tunes in.
//!
//! A task that `set_video` replaces hands its rendition tracks to the
//! successor, which encodes the new source into the renditions of the same
//! name. A subscriber keeps its track and sees the new source from the next
//! keyframe on. Renditions the new ladder lacks finish.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use n0_future::{StreamExt, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use super::{
    Job, Timeline,
    encoding::{VideoEncoding, VideoRendition},
    status::{RenditionState, Reporter},
};
use moq_video::encode;

use moq_ext_media_core::{
    ByPtr, Error, SlotState,
    stats::{Cell, RateMeter, Smoothed},
};
use moq_ext_sources::{EncodedVideoSource, VideoSource, frames::VideoFrames};

use crate::stats::EncodeStats;

/// The name of the one rendition a pre-encoded source publishes.
pub(super) const ENCODED_RENDITION: &str = "video";

/// How many frames an encoder may hold before their timestamps are forgotten.
///
/// A realtime encoder returns a frame's packets with that frame or a few
/// frames later, so this is far more than one ever holds.
const MAX_PENDING: usize = 64;

/// A rendition's track, left open by a replaced video task for its successor.
pub(super) struct HandedOver {
    producer: encode::Producer,
    /// The last timestamp published on the track.
    last: Option<moq_net::Timestamp>,
}

/// The tracks a replaced video task left open, by rendition name.
pub(super) type Handovers = Arc<Mutex<HashMap<String, HandedOver>>>;

/// Finishes every track left in `handovers`.
///
/// For a task that stops without a successor, so subscribers see the end.
pub(super) fn finish_handovers(handovers: &Handovers) {
    finish_tracks(std::mem::take(&mut *handovers.lock().expect("poisoned")));
}

/// Finishes handed over tracks that nothing takes over.
fn finish_tracks(tracks: HashMap<String, HandedOver>) {
    for (rendition, mut handed) in tracks {
        debug!(%rendition, "finishing a track nobody took over");
        if let Err(err) = handed.producer.finish() {
            debug!(%rendition, error = %err, "the track did not finish cleanly");
        }
    }
}

/// Waits until a subscriber wants the track, or fails once it closes.
async fn wanted(demand: &mut moq_net::track::Demand) -> Result<(), moq_net::Error> {
    while demand.wanted_changed().await?.is_none() {}
    Ok(())
}

/// Waits until no subscriber wants the track, or it closes.
///
/// Returns `false` if the track closed.
async fn unwanted(demand: &mut moq_net::track::Demand) -> bool {
    loop {
        match demand.wanted_changed().await {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

/// Encodes a raw source into every rendition of `encoding`.
pub(super) async fn run_raw(
    mut job: Job,
    source: VideoSource,
    encoding: VideoEncoding,
    stop: CancellationToken,
) {
    let Some(tracks) = job.take_over(&stop).await else {
        return;
    };
    let Job {
        producer,
        catalog,
        clock,
        timestamps,
        handovers,
        handing_over,
        stats,
        reporter,
        ..
    } = job;
    // Every encoder task holds a share of the track names. Encoder tasks
    // aborted with this one are dropped after it returns, and a replacement
    // must not create its tracks before then.
    let tracks = Arc::new(tracks);
    // The predecessor has finished, so its renditions leave the stats.
    stats.clear_video();

    // Tracks take their size from the source's format. A source that idles
    // until watched, such as a phone camera, produces no frame before a track
    // exists. A frame that is already there refines the size, and a later
    // frame of another size is scaled to the advertised one.
    let mut frames = source.frames();
    let format = source.format();
    let (size, color) = match frames.current() {
        Some(frame) => (frame.size(), frame.surface.color()),
        None => (format.size, None),
    };
    let rate = format.rate;
    // The first rendition to encode anchors it. It is shared so every
    // rendition carries the same timestamp for the same picture.
    let timeline = Timeline::new(clock).recording(timestamps);

    let mut encoders = JoinSet::new();
    let mut last_failure = None;
    let mut fail = |rendition: &str, err: Error| {
        let err = Arc::new(err);
        warn!(%rendition, error = %format!("{err:#}"), "rendition cannot encode");
        reporter.rendition(rendition, RenditionState::Failed(err.clone().into()));
        last_failure = Some(err);
    };
    // Every rendition is probed before any is advertised, so the whole ladder
    // reaches the catalog at once. A viewer that read the catalog between two
    // probes would miss the renditions still being probed.
    let mut probed = Vec::with_capacity(encoding.renditions.len());
    for rendition in &encoding.renditions {
        let mut config = rendition.encode_config(size, rate, color, encoding.prefer_hardware);
        match probe(&mut config).await {
            Ok(published) => probed.push((rendition, config, published)),
            Err(err) => fail(&rendition.name, err),
        }
    }
    // A track the predecessor left goes on with the new source, and its
    // catalog entry follows the parameter sets of the next keyframe. The
    // tracks of renditions this ladder does not publish finish first, so no
    // name is taken twice.
    let (mut inherited, left): (HashMap<_, _>, HashMap<_, _>) =
        std::mem::take(&mut *handovers.lock().expect("poisoned"))
            .into_iter()
            .partition(|(name, _)| probed.iter().any(|(rendition, ..)| rendition.name == *name));
    finish_tracks(left);
    for (rendition, config, published) in probed {
        let (producer, last) = match inherited.remove(&rendition.name) {
            Some(HandedOver { producer, last }) => {
                info!(rendition = %rendition.name, "taking over the rendition's track");
                (producer, last)
            }
            None => {
                let reserved = catalog.reserve();
                let track_info = reserved.track_info(hang::catalog::PRIORITY.video);
                let publisher = producer
                    .create_track(rendition.name.as_str(), Some(track_info))
                    .map_err(Error::broadcast)
                    .and_then(|track| {
                        encode::Producer::with_track(track, reserved, published)
                            .map_err(Error::catalog)
                    });
                match publisher {
                    Ok(producer) => (producer, None),
                    Err(err) => {
                        fail(&rendition.name, err);
                        continue;
                    }
                }
            }
        };
        info!(rendition = %rendition.name, size = %config.size(), %rate, "publishing video rendition");
        let encoder = Encoder {
            name: rendition.name.clone(),
            producer,
            config,
            frames: source.frames(),
            source: source.clone(),
            timeline: timeline.clone().after(last),
            pending: VecDeque::new(),
            handovers: handovers.clone(),
            handing_over: handing_over.clone(),
            _tracks: tracks.clone(),
            interval: frame_interval(rendition, rate),
            reporter: reporter.clone(),
            stats: stats.rendition(&rendition.name),
            stop: stop.clone(),
        };
        encoders.spawn(
            encoder
                .run()
                .instrument(info_span!("rendition", name = %rendition.name)),
        );
    }
    if encoders.is_empty() {
        let failure = last_failure
            .unwrap_or_else(|| Arc::new(Error::invalid("no rendition could be encoded")));
        reporter.slot(SlotState::Failed(failure.into()));
        return;
    }
    reporter.slot(SlotState::Running);
    let spawned = encoders.len();
    let mut failed = 0;

    // The source frame rate is measured here, the one place that reads every
    // frame.
    let source_fps = stats.source_fps();
    let mut meter = RateMeter::default();
    loop {
        tokio::select! {
            frame = frames.next() => match frame {
                Some(_) => {
                    if let Some((fps, _)) = meter.tick(0) {
                        source_fps.update(|value| *value = Some(fps as f32));
                    }
                }
                None => break,
            },
            Some(joined) = encoders.join_next() => {
                if let Some(failure) = encoder_failure(joined) {
                    failed += 1;
                    last_failure = Some(failure);
                }
                // Every encoder failed while the source runs on. Nothing is
                // published, and the slot must say so.
                if failed == spawned {
                    let failure = last_failure.clone().expect("counted above");
                    warn!(error = %format!("{failure:#}"), "no rendition can encode");
                    reporter.slot(SlotState::Failed(failure.into()));
                    return;
                }
            }
            () = stop.cancelled() => break,
        }
    }
    // The encoders see the same end or the same stop, and finish their tracks.
    while let Some(joined) = encoders.join_next().await {
        encoder_failure(joined);
    }
    if stop.is_cancelled() {
        return;
    }
    match source.failure() {
        Some(failure) => {
            warn!(error = %format!("{failure:#}"), "video source failed");
            reporter.slot(SlotState::Failed(failure.into()));
        }
        None => {
            debug!("video source ended");
            reporter.slot(SlotState::Ended);
        }
    }
}

/// Returns why an encoder stopped, if it failed or panicked.
///
/// A failed encoder has already reported itself in its rendition's state.
fn encoder_failure(
    joined: Result<Option<Arc<Error>>, n0_future::task::JoinError>,
) -> Option<Arc<Error>> {
    match joined {
        Ok(failure) => failure,
        Err(err) => {
            warn!(error = %err, "rendition encoder panicked");
            Some(Arc::new(Error::encoder_msg(format!(
                "the encoder task panicked: {err}"
            ))))
        }
    }
}

/// Returns the gap between kept frames for a rendition slower than its source.
fn frame_interval(rendition: &VideoRendition, source: moq_video::FrameRate) -> Option<Duration> {
    let rate = rendition.rate?;
    (rate.as_f64() < source.as_f64()).then(|| Duration::from_secs_f64(1.0 / rate.as_f64()))
}

/// Probes the encoder `config` asks for, falling back to software once if allowed.
///
/// An explicitly named backend is never replaced, so a broken driver shows up
/// as an encoder that does not open.
async fn probe(config: &mut encode::Config) -> Result<hang::catalog::VideoConfig, Error> {
    match config.probe().await {
        Ok(published) => Ok(published),
        Err(err) if fall_back(config, &err) => config.probe().await.map_err(encode_error),
        Err(err) => Err(encode_error(err)),
    }
}

/// Returns whether an encoder of `kind` falls back to software.
fn allows_fallback(kind: &encode::Kind) -> bool {
    matches!(kind, encode::Kind::Auto | encode::Kind::Hardware)
}

/// Switches `config` to the software encoder if its backend allows a fallback.
///
/// Returns whether it switched.
fn fall_back(config: &mut encode::Config, err: &moq_video::Error) -> bool {
    if !allows_fallback(&config.kind) {
        return false;
    }
    warn!(error = %err, "the hardware encoder failed, falling back to software");
    config.kind = encode::Kind::Software;
    true
}

/// Converts an encoder failure into the crate's error.
fn encode_error(err: moq_video::Error) -> Error {
    match err {
        moq_video::Error::NoEncoder(tried) => n0_error::e!(Error::NoEncoder { codec: tried }),
        moq_video::Error::UnknownEncoder { codec, .. } => n0_error::e!(Error::NoEncoder {
            codec: format!("{codec:?}")
        }),
        other => Error::encoder(other),
    }
}

/// One rendition's encoder.
struct Encoder {
    name: String,
    producer: encode::Producer,
    config: encode::Config,
    frames: VideoFrames,
    source: VideoSource,
    /// Maps source timestamps onto the broadcast clock, shared with the other renditions.
    timeline: Timeline,
    /// The source and broadcast timestamps of frames in the encoder, oldest first.
    pending: VecDeque<(moq_net::Timestamp, moq_net::Timestamp)>,
    /// Where the track goes when a successor takes it over.
    handovers: Handovers,
    /// Set when a successor takes the slot's tracks over.
    handing_over: Arc<AtomicBool>,
    /// The slot's hold on its track names.
    _tracks: Arc<tokio::sync::OwnedMutexGuard<()>>,
    /// The gap between kept frames, for a rendition slower than its source.
    interval: Option<Duration>,
    reporter: Reporter,
    stats: Cell<EncodeStats>,
    stop: CancellationToken,
}

/// How an encoder stopped without failing.
enum Stopped {
    /// The track finished or closed.
    Done,
    /// The slot stopped for a successor, which takes the track over.
    HandOver,
}

/// What an encoder waits for next.
enum Next {
    /// A frame from the source.
    Frame(Arc<moq_video::Frame>),
    /// The source ended.
    Ended,
    /// The slot stopped.
    Stopped,
}

impl Encoder {
    /// Encodes while someone watches, until the source ends or the slot stops.
    ///
    /// Reports a failure in the rendition's state and returns it for the slot
    /// to count. A slot that stops for a successor leaves the track open in
    /// the handovers.
    async fn run(mut self) -> Option<Arc<Error>> {
        let err = match self.encode().await {
            Ok(Stopped::Done) => return None,
            Ok(Stopped::HandOver) => {
                debug!(last = ?self.timeline.last(), "handing the track over");
                let handed = HandedOver {
                    producer: self.producer,
                    last: self.timeline.last(),
                };
                self.handovers
                    .lock()
                    .expect("poisoned")
                    .insert(self.name, handed);
                return None;
            }
            Err(err) => err,
        };
        warn!(error = %format!("{err:#}"), "rendition encoder failed");
        // Aborting the track shows a subscriber the cause instead of a bare
        // reset.
        let cause = moq_net::Error::Transport(format!("{err:#}"));
        let err = Arc::new(err);
        self.reporter
            .rendition(&self.name, RenditionState::Failed(err.clone().into()));
        self.producer.abort(cause);
        Some(err)
    }

    /// Leaves the track for a successor if the slot stops for one, and finishes it otherwise.
    fn stopped(&mut self) -> Result<Stopped, Error> {
        if self.stop.is_cancelled() && self.handing_over.load(Ordering::Acquire) {
            return Ok(Stopped::HandOver);
        }
        self.producer.finish().map_err(Error::broadcast)?;
        Ok(Stopped::Done)
    }

    /// Encodes frames for as long as someone watches, one demand cycle at a time.
    ///
    /// Returns how the encoder stopped without failing.
    async fn encode(&mut self) -> Result<Stopped, Error> {
        let target = self.config.size();
        self.stats.update(|stats| stats.size = Some(target));

        loop {
            let mut demand = self.producer.demand();
            // Idle until someone subscribes, the source ends, or the slot
            // stops. The track and its catalog entry are already advertised.
            tokio::select! {
                used = wanted(&mut demand) => {
                    // The track was closed or aborted under the encoder, so
                    // there is nothing left to finish.
                    if let Err(err) = used {
                        debug!(error = %err, "the rendition's track closed");
                        return Ok(Stopped::Done);
                    }
                }
                () = self.frames.closed() => return self.stopped(),
                () = self.stop.cancelled() => return self.stopped(),
            }

            // Counts as demand on the source while this encodes, so an
            // application camera can idle while nobody watches.
            let _wanted = self.source.want();
            let mut encoder = match encode::Sink::open(&self.config).await {
                Ok(encoder) => encoder,
                Err(err) if fall_back(&mut self.config, &err) => encode::Sink::open(&self.config)
                    .await
                    .map_err(encode_error)?,
                Err(err) => return Err(encode_error(err)),
            };
            self.encoding(encoder.name());

            let mut due: Option<moq_net::Timestamp> = None;
            let mut meter = RateMeter::default();
            let mut timing = Smoothed::default();
            loop {
                let next = tokio::select! {
                    // The last viewer left. Mark the gap so the next timestamp
                    // does not stretch this frame across it.
                    open = unwanted(&mut demand) => {
                        if !open {
                            debug!("the rendition's track closed while it encoded");
                            return Ok(Stopped::Done);
                        }
                        self.producer.discontinuity().map_err(Error::broadcast)?;
                        self.reporter.rendition(&self.name, RenditionState::Idle);
                        break;
                    }
                    frame = self.frames.next() => frame.map_or(Next::Ended, Next::Frame),
                    () = self.stop.cancelled() => Next::Stopped,
                };
                // The source ended or the slot stopped. Drain the encoder, then
                // finish the track so subscribers see a clean end, or leave it
                // open for a successor.
                let frame = match next {
                    Next::Frame(frame) => frame,
                    Next::Ended | Next::Stopped => {
                        let mut tail = encoder.finish().await.map_err(Error::encoder)?;
                        self.restamp(&mut tail);
                        self.producer.publish(&tail).map_err(Error::broadcast)?;
                        return self.stopped();
                    }
                };
                let mapped = self.timeline.map(frame.timestamp);
                if self.timeline.broke() {
                    // A backend that cannot cut keys at its own interval.
                    if let Err(err) = encoder.cut().await {
                        debug!(error = %err, "no keyframe at the source's break");
                    }
                }
                if let Some(interval) = self.interval {
                    if due.is_some_and(|due| mapped < due) {
                        continue;
                    }
                    let next = mapped.as_micros() as u64 + interval.as_micros() as u64;
                    due = moq_net::Timestamp::from_micros(next).ok();
                }
                // A frame of another size than the track advertises is scaled
                // to it, for example after a phone camera turns to portrait.
                let frame = if frame.size() == target {
                    frame
                } else {
                    Arc::new(
                        frame
                            .resize(target, &Default::default())
                            .map_err(Error::encoder)?,
                    )
                };

                // The encoder labels the stream with one color space and
                // refuses frames in another. A source that idled until watched
                // had no frame to take it from when the track opened, so adopt
                // the first frame's and reopen.
                let labelled = self
                    .config
                    .color
                    .unwrap_or_else(|| moq_video::Color::infer(target));
                if let Some(color) = frame.surface.color()
                    && color != labelled
                {
                    debug!(
                        ?color,
                        ?labelled,
                        "reopening the encoder in the frames' color space"
                    );
                    self.config.color = Some(color);
                    encoder = encode::Sink::open(&self.config)
                        .await
                        .map_err(encode_error)?;
                    self.encoding(encoder.name());
                }

                if self.pending.len() == MAX_PENDING {
                    self.pending.pop_front();
                }
                self.pending.push_back((frame.timestamp, mapped));
                let started = Instant::now();
                let mut encoded = match encoder.encode(frame).await {
                    Ok(encoded) => encoded,
                    Err(err) if fall_back(&mut self.config, &err) => {
                        encoder = encode::Sink::open(&self.config)
                            .await
                            .map_err(encode_error)?;
                        self.encoding(encoder.name());
                        continue;
                    }
                    Err(err) => return Err(encode_error(err)),
                };
                let took = started.elapsed();
                self.restamp(&mut encoded);
                let bytes: usize = encoded.iter().map(|packet| packet.payload.len()).sum();
                let rates = meter.tick(bytes as u64);
                let smoothed = timing.record(took);
                self.stats.update(|stats| {
                    stats.record(bytes as u64, rates);
                    stats.encode_time = Some(smoothed);
                });
                self.producer.publish(&encoded).map_err(Error::broadcast)?;
                // A keyframe with another decoder identity, such as a new
                // level after a switch to a larger source, moves the rendition
                // to a newly named track and finishes the old one. Its
                // viewers are the new track's.
                demand = self.producer.demand();
            }
        }
    }

    /// Records which encoder is running.
    fn encoding(&self, encoder: &str) {
        debug!(encoder, "rendition encoding");
        self.reporter.rendition(
            &self.name,
            RenditionState::Encoding {
                encoder: encoder.to_string(),
            },
        );
        let encoder = encoder.to_string();
        self.stats.update(|stats| stats.encoder = Some(encoder));
    }

    /// Moves encoded timestamps onto the broadcast clock.
    ///
    /// An encoder stamps a packet with the source timestamp of its frame, so
    /// each packet takes what its frame was mapped to. A packet of a frame
    /// that is no longer pending, which a realtime encoder never returns, is
    /// mapped on its own.
    fn restamp(&mut self, encoded: &mut [encode::Encoded]) {
        for packet in encoded {
            let pending = self
                .pending
                .iter()
                .position(|(source, _)| *source == packet.timestamp);
            packet.timestamp = match pending.and_then(|index| self.pending.remove(index)) {
                Some((_, mapped)) => mapped,
                None => self.timeline.map(packet.timestamp),
            };
        }
    }
}

/// Publishes a pre-encoded Annex-B stream as the one rendition.
///
/// `Split` cuts the byte stream into access units. `Import` publishes them and
/// fills in the catalog rendition from the first SPS, so the caller does not
/// describe the stream.
pub(super) async fn run_encoded(mut job: Job, source: EncodedVideoSource, stop: CancellationToken) {
    let Some(_tracks) = job.take_over(&stop).await else {
        return;
    };
    // The stream describes itself, so it starts its own track.
    finish_handovers(&job.handovers);
    let Job {
        producer,
        catalog,
        stats,
        reporter,
        ..
    } = job;
    stats.clear_video();
    let (mut bytes, _guard) = source.into_parts();
    let result = async {
        let track = producer
            .create_track(
                ENCODED_RENDITION,
                Some(catalog.track_info(hang::catalog::PRIORITY.video)),
            )
            .map_err(Error::broadcast)?;
        let mut import =
            moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default())
                .map_err(Error::catalog)?;
        let mut split = moq_mux::codec::h264::Split::new();
        let entry = stats.rendition(ENCODED_RENDITION);
        entry.update(|stats| stats.encoder = Some("pre-encoded".to_string()));
        info!("publishing pre-encoded video");

        let mut units = 0u64;
        let mut meter = RateMeter::default();
        loop {
            let chunk = tokio::select! {
                chunk = bytes.next() => chunk,
                () = stop.cancelled() => None,
            };
            let Some(chunk) = chunk else { break };
            let frames = split.decode(&chunk, None).map_err(Error::decoder)?;
            if units == 0 && !frames.is_empty() {
                reporter.slot(SlotState::Running);
                reporter.rendition(
                    ENCODED_RENDITION,
                    RenditionState::Encoding {
                        encoder: "pre-encoded".to_string(),
                    },
                );
            }
            for frame in &frames {
                let bytes = frame.payload.len() as u64;
                let rates = meter.tick(bytes);
                entry.update(|stats| stats.record(bytes, rates));
            }
            units += frames.len() as u64;
            import.decode(frames).map_err(Error::decoder)?;
        }
        // The splitter holds the final access unit until the next start code,
        // so the end of the stream has to flush it explicitly.
        let tail = split.flush(None).map_err(Error::decoder)?;
        units += tail.len() as u64;
        import.decode(tail).map_err(Error::decoder)?;
        import.finish().map_err(Error::catalog)?;
        Ok::<_, Error>(units)
    }
    .await;

    if stop.is_cancelled() {
        return;
    }
    match result {
        // A source that ends before any access unit never described itself.
        // That is a failed camera, not a finished stream.
        Ok(0) => reporter.slot(SlotState::Failed(ByPtr::new(Error::device_msg(
            "the pre-encoded source ended before its first access unit",
        )))),
        Ok(units) => {
            debug!(units, "pre-encoded source ended");
            reporter.slot(SlotState::Ended);
        }
        Err(err) => {
            warn!(error = %format!("{err:#}"), "pre-encoded video failed");
            reporter.slot(SlotState::Failed(ByPtr::new(err)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::{TIMESTAMP_HISTORY, TimestampLog};

    fn at(micros: u64) -> moq_net::Timestamp {
        moq_net::Timestamp::from_micros(micros).expect("in range")
    }

    #[test]
    fn a_timeline_keeps_the_source_cadence() {
        let mut timeline = Timeline::new(moq_mux::Clock::new());
        let first = timeline.map(at(1_000_000));
        let later = timeline.map(at(1_033_333));
        assert_eq!(later.as_micros() - first.as_micros(), 33_333);
    }

    /// A camera whose timestamps start at zero after a pattern on a later clock.
    #[test]
    fn a_timeline_that_goes_back_anchors_again_after_the_last_frame() {
        let mut timeline = Timeline::new(moq_mux::Clock::new());
        let pattern = timeline.map(at(80_000_000));
        let camera = timeline.map(at(20_000));
        assert!(camera > pattern, "{camera:?} is not after {pattern:?}");
        assert!(camera.as_micros() - pattern.as_micros() < 1_000_000);
        let next = timeline.map(at(53_333));
        assert_eq!(next.as_micros() - camera.as_micros(), 33_333);
    }

    #[test]
    fn a_timeline_that_jumps_ahead_anchors_at_the_clock() {
        let mut timeline = Timeline::new(moq_mux::Clock::new());
        let first = timeline.map(at(20_000));
        let jumped = timeline.map(at(3_600_000_000));
        assert!(jumped.as_micros() - first.as_micros() < 1_000_000);
    }

    /// Every rendition carries the same timestamp for the same picture.
    #[test]
    fn renditions_share_the_offset() {
        let mut first = Timeline::new(moq_mux::Clock::new());
        let mut second = first.clone();
        assert_eq!(first.map(at(20_000)), second.map(at(20_000)));
        assert_eq!(first.map(at(90_000_000)), second.map(at(90_000_000)));
    }

    /// A successor's first frame lands after what its predecessor published on the track.
    #[test]
    fn a_taken_over_track_goes_on_after_its_last_timestamp() {
        let clock = moq_mux::Clock::new();
        let ahead = at(clock.now().as_micros() as u64 + 500_000);
        let mut timeline = Timeline::new(clock).after(Some(ahead));
        assert!(timeline.map(at(0)) > ahead);
    }

    /// A recording timeline finds the source timestamp of each frame it mapped, across a break.
    #[test]
    fn a_recording_timeline_maps_back_to_the_source() {
        let log = TimestampLog::default();
        let mut timeline = Timeline::new(moq_mux::Clock::new()).recording(log.clone());
        let pattern = timeline.map(at(80_000_000));
        let camera = timeline.map(at(20_000));
        assert_eq!(log.source(pattern), Some(at(80_000_000)));
        assert_eq!(log.source(camera), Some(at(20_000)));
        assert_eq!(log.source(at(1)), None, "nothing was published at 1 us");
    }

    #[test]
    fn the_timestamp_log_forgets_the_oldest_frames() {
        let log = TimestampLog::default();
        let count = TIMESTAMP_HISTORY as u64 + 10;
        for micros in 1..=count {
            log.record(at(micros), at(micros + 7));
        }
        assert_eq!(log.source(at(1)), None);
        assert_eq!(log.source(at(count)), Some(at(count + 7)));
    }

    #[test]
    fn only_an_open_choice_allows_fallback() {
        assert!(allows_fallback(&encode::Kind::Auto));
        assert!(allows_fallback(&encode::Kind::Hardware));
        assert!(!allows_fallback(&encode::Kind::Software));
        assert!(!allows_fallback(&encode::Kind::Named("vaapi".into())));
    }

    #[test]
    fn a_slower_rendition_keeps_every_other_frame() {
        let fps = |n| moq_video::FrameRate::new(n, 1).expect("valid");
        let half = VideoRendition {
            rate: Some(fps(15)),
            ..VideoRendition::new("half")
        };
        assert_eq!(
            frame_interval(&half, fps(30)),
            Some(Duration::from_secs_f64(1.0 / 15.0))
        );
        assert_eq!(frame_interval(&VideoRendition::new("full"), fps(30)), None);
    }
}
