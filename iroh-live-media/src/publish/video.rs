//! The video publish task: one source, one encoder per rendition.
//!
//! A camera opens only once, and every rendition needs the same picture. Each
//! rendition's encoder reads the source through its own latest-wins handle, so
//! a rendition that falls behind drops frames without stalling the others.
//!
//! A rendition encodes only while someone watches it. The source runs even
//! when nobody watches, because a preview reads the same frames and a
//! publisher expects to see itself before anyone tunes in.

use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

use n0_future::{StreamExt, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use super::{
    Job, Rebase,
    encoding::{VideoEncoding, VideoRendition},
    status::{RenditionState, Reporter, SlotState},
};
use crate::{
    EncodedVideoSource, VideoSource,
    error::Error,
    frames::VideoFrames,
    stats::{Cell, EncodeStats, RateMeter, Smoothed},
    video::{self, encode},
};

/// The name of the one rendition a pre-encoded source publishes.
pub(super) const ENCODED_RENDITION: &str = "video";

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
    let rebase = Arc::new(OnceLock::new());

    let mut encoders = JoinSet::new();
    let mut last_failure = None;
    let mut fail = |rendition: &str, err: Error| {
        let err = Arc::new(err);
        warn!(%rendition, error = %err, "rendition cannot encode");
        reporter.rendition(rendition, RenditionState::Failed(err.clone()));
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
    for (rendition, config, published) in probed {
        let track_info = catalog.track_info(hang::catalog::PRIORITY.video);
        let publisher = producer
            .create_track(rendition.name.as_str(), Some(track_info))
            .map_err(Error::broadcast)
            .and_then(|track| {
                encode::Producer::with_track(track, catalog.clone(), published)
                    .map_err(Error::catalog)
            });
        let producer = match publisher {
            Ok(producer) => producer,
            Err(err) => {
                fail(&rendition.name, err);
                continue;
            }
        };
        info!(rendition = %rendition.name, size = %config.size(), %rate, "publishing video rendition");
        let encoder = Encoder {
            name: rendition.name.clone(),
            producer,
            config,
            frames: source.frames(),
            source: source.clone(),
            clock,
            rebase: rebase.clone(),
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
        reporter.slot(SlotState::Failed(failure));
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
                if let Some(failure) = report(joined) {
                    failed += 1;
                    last_failure = Some(failure);
                }
                // Every encoder failed while the source runs on. Nothing is
                // published, and the slot must say so.
                if failed == spawned {
                    let failure = last_failure.clone().expect("counted above");
                    warn!(error = %failure, "no rendition can encode");
                    reporter.slot(SlotState::Failed(failure));
                    return;
                }
            }
            () = stop.cancelled() => break,
        }
    }
    // The encoders see the same end or the same stop, and finish their tracks.
    while let Some(joined) = encoders.join_next().await {
        report(joined);
    }
    if stop.is_cancelled() {
        return;
    }
    match source.failure() {
        Some(failure) => {
            warn!(error = %failure, "video source failed");
            reporter.slot(SlotState::Failed(failure));
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
fn report(joined: Result<Option<Arc<Error>>, n0_future::task::JoinError>) -> Option<Arc<Error>> {
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
fn frame_interval(rendition: &VideoRendition, source: video::Rate) -> Option<std::time::Duration> {
    let rate = rendition.rate?;
    (rate.as_f64() < source.as_f64())
        .then(|| std::time::Duration::from_secs_f64(1.0 / rate.as_f64()))
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
fn falls_back(kind: &encode::Kind) -> bool {
    matches!(kind, encode::Kind::Auto | encode::Kind::Hardware)
}

/// Switches `config` to the software encoder if its backend allows a fallback.
///
/// Returns whether it switched.
fn fall_back(config: &mut encode::Config, err: &video::Error) -> bool {
    if !falls_back(&config.kind) {
        return false;
    }
    warn!(error = %err, "the hardware encoder failed, falling back to software");
    config.kind = encode::Kind::Software;
    true
}

/// Converts an encoder failure into the crate's error.
fn encode_error(err: video::Error) -> Error {
    match err {
        video::Error::NoEncoder(tried) => n0_error::e!(Error::NoEncoder { codec: tried }),
        video::Error::UnknownEncoder { codec, .. } => n0_error::e!(Error::NoEncoder {
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
    clock: moq_mux::Clock,
    /// The broadcast's rebase, set by the first rendition to encode a frame.
    rebase: Arc<OnceLock<Rebase>>,
    /// The slot's hold on its track names.
    _tracks: Arc<tokio::sync::OwnedMutexGuard<()>>,
    /// The gap between kept frames, for a rendition slower than its source.
    interval: Option<std::time::Duration>,
    reporter: Reporter,
    stats: Cell<EncodeStats>,
    stop: CancellationToken,
}

impl Encoder {
    /// Encodes while someone watches, until the source ends or the slot stops.
    ///
    /// Reports a failure in the rendition's state and returns it for the slot
    /// to count.
    async fn run(mut self) -> Option<Arc<Error>> {
        let err = self.encode().await.err()?;
        warn!(error = %err, "rendition encoder failed");
        // Aborting the track shows a subscriber the cause instead of a bare
        // reset.
        let cause = moq_net::Error::Transport(err.to_string());
        let err = Arc::new(err);
        self.reporter
            .rendition(&self.name, RenditionState::Failed(err.clone()));
        self.producer.abort(cause);
        Some(err)
    }

    async fn encode(&mut self) -> Result<(), Error> {
        let demand = self.producer.demand();
        let target = self.config.size();
        self.stats.update(|stats| stats.size = Some(target));

        loop {
            // Idle until someone subscribes, the source ends, or the slot
            // stops. The track and its catalog entry are already advertised.
            tokio::select! {
                used = demand.used() => {
                    if let Err(err) = used {
                        debug!(error = %err, "rendition no longer watched");
                        break;
                    }
                }
                () = self.frames.closed() => {
                    self.producer.finish().map_err(Error::broadcast)?;
                    return Ok(());
                }
                () = self.stop.cancelled() => break,
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
                let frame = tokio::select! {
                    // The last viewer left. Mark the gap so the next timestamp
                    // does not stretch this frame across it.
                    _ = demand.unused() => {
                        self.producer.discontinuity().map_err(Error::broadcast)?;
                        self.reporter.rendition(&self.name, RenditionState::Idle);
                        break;
                    }
                    frame = self.frames.next() => frame,
                    () = self.stop.cancelled() => None,
                };
                // The source ended or the slot stopped. Drain the encoder and
                // finish the track so subscribers see a clean end.
                let Some(frame) = frame else {
                    let mut tail = encoder.finish().await.map_err(Error::encoder)?;
                    self.restamp(&mut tail);
                    self.producer.publish(&tail).map_err(Error::broadcast)?;
                    self.producer.finish().map_err(Error::broadcast)?;
                    return Ok(());
                };
                let clock = self.clock;
                self.rebase
                    .get_or_init(|| Rebase::anchor(clock, frame.timestamp));
                if let Some(interval) = self.interval {
                    if due.is_some_and(|due| frame.timestamp < due) {
                        continue;
                    }
                    let next = frame.timestamp.as_micros() as u64 + interval.as_micros() as u64;
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

                let started = Instant::now();
                let encoded = match encoder.encode(frame).await {
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
                let mut encoded = encoded;
                self.restamp(&mut encoded);
                let bytes: usize = encoded.iter().map(|packet| packet.payload.len()).sum();
                let rates = meter.tick(bytes as u64);
                let smoothed = timing.record(took);
                self.stats.update(|stats| {
                    stats.record(bytes as u64, rates);
                    stats.encode_time = Some(smoothed);
                });
                self.producer.publish(&encoded).map_err(Error::broadcast)?;
            }
        }

        self.producer.finish().map_err(Error::broadcast)?;
        Ok(())
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
    fn restamp(&self, encoded: &mut [encode::Encoded]) {
        // Unset only before the first frame, when there is nothing to restamp.
        let Some(rebase) = self.rebase.get() else {
            return;
        };
        for packet in encoded {
            packet.timestamp = rebase.map(packet.timestamp);
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
    let Job {
        producer,
        catalog,
        stats,
        reporter,
        ..
    } = job;
    stats.clear_video();
    let EncodedVideoSource { mut bytes, _guard } = source;
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
        Ok(0) => reporter.slot(SlotState::Failed(Arc::new(Error::device_msg(
            "the pre-encoded source ended before its first access unit",
        )))),
        Ok(units) => {
            debug!(units, "pre-encoded source ended");
            reporter.slot(SlotState::Ended);
        }
        Err(err) => {
            warn!(error = %err, "pre-encoded video failed");
            reporter.slot(SlotState::Failed(Arc::new(err)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebase_moves_every_timestamp_by_one_offset() {
        let clock = moq_mux::Clock::new();
        let first = moq_net::Timestamp::from_micros(1_000_000).expect("in range");
        let rebase = Rebase::anchor(clock, first);
        let later = moq_net::Timestamp::from_micros(1_033_333).expect("in range");
        let gap = rebase.map(later).as_micros() - rebase.map(first).as_micros();
        assert_eq!(gap, 33_333, "the cadence survives the rebase");
    }

    #[test]
    fn only_an_open_choice_falls_back() {
        assert!(falls_back(&encode::Kind::Auto));
        assert!(falls_back(&encode::Kind::Hardware));
        assert!(!falls_back(&encode::Kind::Software));
        assert!(!falls_back(&encode::Kind::Named("vaapi".into())));
    }

    #[test]
    fn a_slower_rendition_keeps_every_other_frame() {
        let fps = |n| video::Rate::new(n, 1).expect("valid");
        let half = VideoRendition {
            rate: Some(fps(15)),
            ..VideoRendition::new("half")
        };
        assert_eq!(
            frame_interval(&half, fps(30)),
            Some(std::time::Duration::from_secs_f64(1.0 / 15.0))
        );
        assert_eq!(frame_interval(&VideoRendition::new("full"), fps(30)), None);
    }
}
