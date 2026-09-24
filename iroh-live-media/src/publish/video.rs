//! The video publish task: one source, one encoder per rendition.
//!
//! Every rendition needs the same picture at the same instant and a camera can
//! only be opened once, so each rendition's encoder reads the source through a
//! latest-wins handle of its own: a rendition that falls behind drops frames
//! instead of stalling the ones that have not.
//!
//! Encoders are demand-gated the way upstream gates its devices: a rendition
//! encodes only while someone watches it. The source itself is not, because a
//! preview reads the same frames and a publisher expects to see itself before
//! anyone tunes in.

use std::{sync::Arc, time::Instant};

use n0_future::{StreamExt, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use super::{
    FINISH_PATIENCE, SlotTask,
    encoding::{VideoEncoding, VideoRendition},
    status::{RenditionState, Reporter, SlotState},
};
use crate::{
    EncodedVideoSource, VideoSource,
    catalog::CatalogProducer,
    error::Error,
    frames::VideoFrames,
    stats::{Cell, EncodeStats, PublishRecorder, RateMeter, Smoothed},
    video::{self, encode},
};

/// The name of the one rendition a pre-encoded source publishes.
pub(super) const ENCODED_RENDITION: &str = "video";

/// Everything a video publish task needs, moved into it whole.
pub(super) struct Job {
    pub producer: moq_net::broadcast::Producer,
    pub catalog: CatalogProducer,
    pub clock: moq_mux::Clock,
    /// Held while this task owns track names.
    pub tracks: Arc<tokio::sync::Mutex<()>>,
    pub stats: PublishRecorder,
    pub reporter: Reporter,
    /// The task this one replaces, finished before its track names are taken.
    pub predecessor: Option<SlotTask>,
}

/// Maps a source's own timestamps onto the broadcast clock.
///
/// Anchored at the first frame the broadcast reads, and shared by every
/// rendition, so two rungs of one ladder carry the same timestamp for the same
/// picture and a subscriber switching between them sees no jump.
#[derive(Debug, Clone, Copy)]
struct Rebase {
    /// Broadcast micros minus source micros.
    delta: i128,
}

impl Rebase {
    fn anchor(clock: moq_mux::Clock, first: moq_net::Timestamp) -> Self {
        Self {
            delta: clock.now().as_micros() as i128 - first.as_micros() as i128,
        }
    }

    fn map(self, timestamp: moq_net::Timestamp) -> moq_net::Timestamp {
        let micros = (timestamp.as_micros() as i128 + self.delta).max(0) as u64;
        moq_net::Timestamp::from_micros(micros).unwrap_or(timestamp)
    }
}

/// Finishes the predecessor, then waits for the track names, or returns `None`
/// once stopped.
async fn take_over(
    predecessor: Option<SlotTask>,
    tracks: Arc<tokio::sync::Mutex<()>>,
    stop: &CancellationToken,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    if let Some(predecessor) = predecessor {
        tokio::select! {
            () = predecessor.finish(FINISH_PATIENCE) => {}
            () = stop.cancelled() => return None,
        }
    }
    tokio::select! {
        guard = tracks.lock_owned() => Some(guard),
        () = stop.cancelled() => None,
    }
}

/// Encodes a raw source into every rendition of `encoding`.
pub(super) async fn run_raw(
    job: Job,
    source: VideoSource,
    encoding: VideoEncoding,
    stop: CancellationToken,
) {
    let Job {
        producer,
        catalog,
        clock,
        tracks,
        stats,
        reporter,
        predecessor,
    } = job;
    let Some(_tracks) = take_over(predecessor, tracks, &stop).await else {
        return;
    };

    let mut frames = source.frames();
    let first = tokio::select! {
        first = frames.next() => first,
        () = stop.cancelled() => return,
    };
    let Some(first) = first else {
        let failure = source.failure().unwrap_or_else(|| {
            Arc::new(Error::device_msg(
                "the video source ended before its first frame",
            ))
        });
        warn!(error = %failure, "video source produced nothing");
        reporter.slot(SlotState::Failed(failure));
        return;
    };
    let size = first.size();
    let color = first.surface.color();
    let rate = source.format().rate;
    let rebase = Rebase::anchor(clock, first.timestamp);
    drop(first);

    let mut encoders = JoinSet::new();
    let mut last_failure = None;
    for rendition in &encoding.renditions {
        let mut config = rendition.encode_config(size, rate, color, encoding.prefer_hardware);
        let published = match probe(&mut config).await {
            Ok(published) => published,
            Err(err) => {
                let err = Arc::new(err);
                warn!(rendition = %rendition.name, error = %err, "no encoder for this rendition");
                reporter.rendition(&rendition.name, RenditionState::Failed(err.clone()));
                last_failure = Some(err);
                continue;
            }
        };
        let track = match producer.create_track(
            rendition.name.as_str(),
            Some(catalog.track_info(hang::catalog::PRIORITY.video)),
        ) {
            Ok(track) => track,
            Err(err) => {
                let err = Arc::new(Error::transport(err));
                reporter.rendition(&rendition.name, RenditionState::Failed(err.clone()));
                last_failure = Some(err);
                continue;
            }
        };
        let producer = match encode::Producer::with_track(track, catalog.clone(), published) {
            Ok(producer) => producer,
            Err(err) => {
                let err = Arc::new(Error::catalog(err));
                reporter.rendition(&rendition.name, RenditionState::Failed(err.clone()));
                last_failure = Some(err);
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
            rebase,
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

    // The source's own frame rate is written here, the one place that reads
    // every frame, rather than by each encoder.
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
            Some(joined) = encoders.join_next() => report(joined),
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

/// Logs an encoder that panicked; one that failed reported itself.
fn report(joined: Result<(), n0_future::task::JoinError>) {
    if let Err(err) = joined {
        warn!(error = %err, "rendition encoder panicked");
    }
}

/// The gap between frames a rendition keeps, when it runs slower than its
/// source.
fn frame_interval(rendition: &VideoRendition, source: video::Rate) -> Option<std::time::Duration> {
    let rate = rendition.rate?;
    (rate.as_f64() < source.as_f64())
        .then(|| std::time::Duration::from_secs_f64(1.0 / rate.as_f64()))
}

/// Probes the encoder `config` asks for, falling back to software once where
/// the choice was left open.
///
/// A backend named explicitly is not replaced: naming one says to fail rather
/// than fall back, so a broken driver shows up as an encoder that will not open.
async fn probe(config: &mut encode::Config) -> Result<hang::catalog::VideoConfig, Error> {
    match config.probe().await {
        Ok(published) => Ok(published),
        Err(err) if falls_back(&config.kind) => {
            warn!(error = %err, "the hardware encoder would not open, falling back to software");
            config.kind = encode::Kind::Software;
            config.probe().await.map_err(encode_error)
        }
        Err(err) => Err(encode_error(err)),
    }
}

/// Whether a failure of `kind` falls back to software.
fn falls_back(kind: &encode::Kind) -> bool {
    matches!(kind, encode::Kind::Auto | encode::Kind::Hardware)
}

/// An encoder failure, as the crate reports it.
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
    producer: encode::Producer<crate::catalog::IrohLiveExt>,
    config: encode::Config,
    frames: VideoFrames,
    source: VideoSource,
    rebase: Rebase,
    /// The gap between frames kept, for a rendition slower than its source.
    interval: Option<std::time::Duration>,
    reporter: Reporter,
    stats: Cell<EncodeStats>,
    stop: CancellationToken,
}

impl Encoder {
    /// Encodes for as long as someone watches, until the source ends or the
    /// slot stops, and reports a failure in the rendition's state.
    async fn run(mut self) {
        if let Err(err) = self.encode().await {
            warn!(error = %err, "rendition encoder failed");
            // Aborted rather than dropped, so a subscriber sees the cause
            // rather than a bare reset.
            let cause = moq_net::Error::Transport(err.to_string());
            self.reporter
                .rendition(&self.name, RenditionState::Failed(Arc::new(err)));
            self.producer.abort(cause);
        }
    }

    async fn encode(&mut self) -> Result<(), Error> {
        let demand = self.producer.demand();
        let target = self.config.size();
        self.stats.update(|stats| stats.size = Some(target));
        let mut fell_back = false;

        loop {
            // Idle until someone subscribes, the source ends, or the slot
            // stops. The track and its catalog entry are advertised already.
            tokio::select! {
                used = demand.used() => {
                    if let Err(err) = used {
                        debug!(error = %err, "rendition no longer watched");
                        break;
                    }
                }
                () = self.frames.closed() => {
                    self.producer.finish().map_err(Error::transport)?;
                    return Ok(());
                }
                () = self.stop.cancelled() => break,
            }

            // Counted as demand on the source for as long as this encodes, so
            // an application camera can idle while nobody watches.
            let _wanted = self.source.want();
            let mut encoder = match encode::Sink::open(&self.config).await {
                Ok(encoder) => encoder,
                Err(err) if !fell_back && falls_back(&self.config.kind) => {
                    warn!(error = %err, "the hardware encoder would not open, falling back to software");
                    fell_back = true;
                    self.config.kind = encode::Kind::Software;
                    encode::Sink::open(&self.config)
                        .await
                        .map_err(encode_error)?
                }
                Err(err) => return Err(encode_error(err)),
            };
            self.encoding(encoder.name());

            let mut due: Option<moq_net::Timestamp> = None;
            let mut meter = RateMeter::default();
            let mut timing = Smoothed::default();
            loop {
                let frame = tokio::select! {
                    // The last viewer left: mark the gap so the next timestamp
                    // does not stretch this frame across it.
                    _ = demand.unused() => {
                        self.producer.discontinuity().map_err(Error::transport)?;
                        self.reporter.rendition(&self.name, RenditionState::Idle);
                        break;
                    }
                    frame = self.frames.next() => frame,
                    () = self.stop.cancelled() => None,
                };
                // The source ended, or the slot stopped: drain the encoder,
                // publish the tail, and close the track so subscribers see a
                // clean end rather than a reset.
                let Some(frame) = frame else {
                    let mut tail = encoder.finish().await.map_err(Error::encoder)?;
                    self.restamp(&mut tail);
                    self.producer.publish(&tail).map_err(Error::transport)?;
                    self.producer.finish().map_err(Error::transport)?;
                    return Ok(());
                };
                if let Some(interval) = self.interval {
                    if due.is_some_and(|due| frame.timestamp < due) {
                        continue;
                    }
                    let next = frame.timestamp.as_micros() as u64 + interval.as_micros() as u64;
                    due = moq_net::Timestamp::from_micros(next).ok();
                }
                let frame = match frame.size() == target {
                    true => frame,
                    false => Arc::new(
                        frame
                            .resize(target, &Default::default())
                            .map_err(Error::encoder)?,
                    ),
                };

                let started = Instant::now();
                let encoded = match encoder.encode(frame).await {
                    Ok(encoded) => encoded,
                    Err(err) if !fell_back && falls_back(&self.config.kind) => {
                        warn!(error = %err, "the hardware encoder failed, falling back to software");
                        fell_back = true;
                        self.config.kind = encode::Kind::Software;
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
                    stats.frames += 1;
                    stats.bytes += bytes as u64;
                    stats.encode_time = Some(smoothed);
                    if let Some((fps, bytes_per_second)) = rates {
                        stats.fps = Some(fps as f32);
                        stats.bitrate =
                            Some(crate::Bitrate::from_bps((bytes_per_second * 8.0) as u64));
                    }
                });
                self.producer.publish(&encoded).map_err(Error::transport)?;
            }
        }

        self.producer.finish().map_err(Error::transport)?;
        Ok(())
    }

    /// Records that `encoder` is now the one running.
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
        for packet in encoded {
            packet.timestamp = self.rebase.map(packet.timestamp);
        }
    }
}

/// Publishes a pre-encoded Annex-B stream as the one rendition.
///
/// `Split` cuts the byte stream into access units and `Import` publishes them,
/// filling the catalog rendition in from the first SPS it sees, so this path
/// needs no description from the caller.
pub(super) async fn run_encoded(job: Job, source: EncodedVideoSource, stop: CancellationToken) {
    let Job {
        producer,
        catalog,
        tracks,
        stats,
        reporter,
        predecessor,
        ..
    } = job;
    let Some(_tracks) = take_over(predecessor, tracks, &stop).await else {
        return;
    };
    let EncodedVideoSource { mut bytes, _guard } = source;
    let result = async {
        let track = producer
            .create_track(
                ENCODED_RENDITION,
                Some(catalog.track_info(hang::catalog::PRIORITY.video)),
            )
            .map_err(Error::transport)?;
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
                let rates = meter.tick(frame.payload.len() as u64);
                entry.update(|stats| {
                    stats.frames += 1;
                    stats.bytes += frame.payload.len() as u64;
                    if let Some((fps, bytes)) = rates {
                        stats.fps = Some(fps as f32);
                        stats.bitrate = Some(crate::Bitrate::from_bps((bytes * 8.0) as u64));
                    }
                });
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
        // A source that ends without a single access unit never described
        // itself: that is a failed camera rather than a stream that ran its
        // course.
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
        let half = VideoRendition::new("half").with_rate(fps(15));
        assert_eq!(
            frame_interval(&half, fps(30)),
            Some(std::time::Duration::from_secs_f64(1.0 / 15.0))
        );
        assert_eq!(frame_interval(&VideoRendition::new("full"), fps(30)), None);
    }
}
