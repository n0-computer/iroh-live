//! The audio publish task: a microphone, or PCM from any other source.

use std::sync::Arc;

use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{
    FINISH_PATIENCE, SlotTask,
    encoding::AudioEncoding,
    status::{RenditionState, Reporter, SlotState},
};
use crate::{
    AudioFormat, AudioSource, audio,
    catalog::CatalogProducer,
    error::Error,
    source::AudioKind,
    stats::{AudioEncodeStats, Cell, PublishRecorder},
};

/// Everything an audio publish task needs, moved into it whole.
pub(super) struct Job {
    pub producer: moq_net::broadcast::Producer,
    pub catalog: CatalogProducer,
    pub clock: moq_mux::Clock,
    /// Held while this task owns its track name.
    pub tracks: Arc<tokio::sync::Mutex<()>>,
    pub stats: PublishRecorder,
    pub reporter: Reporter,
    /// The task this one replaces, finished before its track name is taken.
    pub predecessor: Option<SlotTask>,
}

/// Publishes `source` encoded as `encoding` until it ends or the slot stops.
pub(super) async fn run(
    job: Job,
    source: AudioSource,
    encoding: AudioEncoding,
    stop: CancellationToken,
) {
    if let Some(predecessor) = job.predecessor {
        tokio::select! {
            () = predecessor.finish(FINISH_PATIENCE) => {}
            () = stop.cancelled() => return,
        }
    }
    let _tracks = tokio::select! {
        guard = job.tracks.clone().lock_owned() => guard,
        () = stop.cancelled() => return,
    };
    let stats = job.stats.audio();
    let codec = encoding.codec.to_string();
    stats.update(|stats| stats.codec = Some(codec.clone()));
    let result = match source.kind() {
        #[cfg(feature = "capture")]
        AudioKind::Microphone(config) => {
            // Built here rather than when the source opened: the publication
            // this one replaces has finished, and with it the canceller it
            // held, which an output hands out one at a time.
            match config.resolve() {
                Ok(capture) => {
                    microphone(
                        &job.producer,
                        &job.catalog,
                        job.clock,
                        &job.reporter,
                        capture,
                        &encoding,
                        &stop,
                    )
                    .await
                }
                Err(err) => Err(err),
            }
        }
        AudioKind::Pcm { format, fanout } => {
            let _wanted = source.want();
            pcm(
                &job.producer,
                &job.catalog,
                job.clock,
                &job.reporter,
                &stats,
                *format,
                fanout.subscribe(),
                &encoding,
                &stop,
            )
            .await
        }
    };
    if stop.is_cancelled() {
        return;
    }
    match result {
        Ok(()) => {
            debug!("audio source ended");
            job.reporter.slot(SlotState::Ended);
        }
        Err(err) => {
            warn!(error = %err, "audio publish failed");
            job.reporter.slot(SlotState::Failed(Arc::new(err)));
        }
    }
}

/// Maps a source's own timestamps onto the broadcast clock, anchored at its
/// first frame, so PCM keeps its own contiguous cadence.
#[derive(Debug, Clone, Copy)]
struct Rebase {
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

/// Encodes PCM from a fan-out receiver.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site, all of it the job's parts"
)]
async fn pcm(
    producer: &moq_net::broadcast::Producer,
    catalog: &CatalogProducer,
    clock: moq_mux::Clock,
    reporter: &Reporter,
    stats: &Cell<AudioEncodeStats>,
    format: AudioFormat,
    mut frames: broadcast::Receiver<audio::Frame>,
    encoding: &AudioEncoding,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let input = audio::encode::Input::new(format.sample_rate, format.layout);
    let mut options = audio::encode::Options::default();
    options.track = Some(encoding.track_name());
    options.settings = encoding.settings(Some(format));
    let mut broadcast = producer.clone();
    let mut encoder =
        audio::encode::Producer::new(&mut broadcast, catalog.clone(), input, &options)
            .map_err(Error::encoder)?;
    info!(track = %encoding.track_name(), rate = format.sample_rate, "publishing audio");
    reporter.slot(SlotState::Running);
    reporter.rendition(
        &encoding.track_name(),
        RenditionState::Encoding {
            encoder: encoding.codec.to_string(),
        },
    );

    let mut rebase = None;
    loop {
        let frame = tokio::select! {
            frame = frames.recv() => frame,
            () = stop.cancelled() => break,
        };
        let mut frame = match frame {
            Ok(frame) => frame,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                debug!(missed, "the audio publish fell behind its source");
                stats.update(|stats| stats.dropped += missed);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        let rebase = *rebase.get_or_insert_with(|| Rebase::anchor(clock, frame.timestamp));
        frame.timestamp = rebase.map(frame.timestamp);
        encoder.write(&frame).map_err(Error::encoder)?;
        stats.update(|stats| stats.frames += 1);
    }
    encoder.finish().map_err(Error::encoder)
}

/// Captures and encodes a microphone through moq-audio's publication.
///
/// The publication owns the device and opens it only while someone listens.
/// Its driver future is not `Send` on macOS, so it runs on a thread of its
/// own, and its state changes come back here to be reported.
async fn microphone(
    producer: &moq_net::broadcast::Producer,
    catalog: &CatalogProducer,
    clock: moq_mux::Clock,
    reporter: &Reporter,
    capture: audio::capture::Config,
    encoding: &AudioEncoding,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let track = encoding.track_name();
    let mut options = audio::encode::PublicationOptions::default();
    options.capture = capture;
    options.encode.track = Some(track.clone());
    options.encode.settings = encoding.settings(None);
    options.clock = clock;

    let (handle_tx, handle) = oneshot::channel();
    let broadcast = producer.clone();
    let catalog = catalog.clone();
    let driver_stop = stop.child_token();
    let mut driver = crate::local_task::spawn(
        "audio-capture",
        driver_stop.clone(),
        move |stop| async move {
            let (publication, driver) =
                match audio::encode::Publication::new(broadcast, catalog, options) {
                    Ok(created) => created,
                    Err(err) => {
                        let _ = handle_tx.send(Err(Error::device(err)));
                        return;
                    }
                };
            if handle_tx.send(Ok(publication)).is_err() {
                return;
            }
            let run = driver.run();
            tokio::pin!(run);
            let result = tokio::select! {
                result = &mut run => result,
                () = stop.cancelled() => return,
            };
            if let Err(err) = result {
                warn!(error = %err, "microphone publication stopped");
            }
        },
    )?;
    let result = follow(handle, reporter, &track, encoding, stop).await;
    // The capture thread holds the echo canceller, which its output hands out
    // to one microphone at a time: a publication replacing this one asks for
    // it as soon as this returns, so the thread has let go first.
    driver_stop.cancel();
    driver.joined().await;
    result
}

/// Reports the microphone publication's state changes until it ends or the
/// slot stops.
async fn follow(
    handle: oneshot::Receiver<Result<audio::encode::Publication, Error>>,
    reporter: &Reporter,
    track: &str,
    encoding: &AudioEncoding,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let mut publication = handle
        .await
        .map_err(|_| Error::device_msg("the microphone thread stopped before it started"))??;
    info!(track = %track, "publishing microphone");

    loop {
        let state = tokio::select! {
            state = publication.changed() => state,
            () = stop.cancelled() => return Ok(()),
        };
        let Some(state) = state else {
            // The driver exited: the track ended.
            return Ok(());
        };
        match state.status() {
            audio::encode::Status::Starting => reporter.slot(SlotState::Starting),
            audio::encode::Status::Waiting | audio::encode::Status::Stopped => {
                reporter.slot(SlotState::Running);
                reporter.rendition(track, RenditionState::Idle);
            }
            audio::encode::Status::Live => {
                reporter.slot(SlotState::Running);
                reporter.rendition(
                    track,
                    RenditionState::Encoding {
                        encoder: encoding.codec.to_string(),
                    },
                );
            }
            audio::encode::Status::Failed => {
                let reason = state
                    .failure()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "the microphone failed".to_string());
                warn!(%reason, "microphone failed; moq-audio retries when it can");
                reporter.slot(SlotState::Failed(Arc::new(Error::device_msg(reason))));
            }
            audio::encode::Status::Ended => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebase_keeps_the_cadence() {
        let clock = moq_mux::Clock::new();
        let first = moq_net::Timestamp::from_micros(5_000).expect("in range");
        let rebase = Rebase::anchor(clock, first);
        let next = moq_net::Timestamp::from_micros(25_000).expect("in range");
        assert_eq!(
            rebase.map(next).as_micros() - rebase.map(first).as_micros(),
            20_000
        );
    }
}
