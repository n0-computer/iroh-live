//! The audio publish task: a microphone, or PCM from any other source.

use moq_ext_sources::{
    AudioFormat, AudioKind, AudioSource, ByPtr, Error, SlotState, stats::SharedCell,
};
use tokio::sync::broadcast;
#[cfg(feature = "capture")]
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[cfg(feature = "capture")]
use super::status::Reporter;
use super::{Job, SourceClock, encoding::audio_track_name, status::RenditionState};
use crate::stats::AudioEncodeStats;

/// Publishes `source` encoded as `encoding` until it ends or the slot stops.
pub(super) async fn run(
    mut job: Job,
    source: AudioSource,
    encoding: moq_audio::encode::Config,
    stop: CancellationToken,
) {
    let Some(_tracks) = job.take_over(&stop).await else {
        return;
    };
    let stats = job.stats.audio();
    let codec = encoding.codec.to_string();
    stats.update(|stats| stats.codec = Some(codec.clone()));
    let result = match source.kind() {
        #[cfg(feature = "capture")]
        // Opened after `take_over`, because an echo canceller attaches to one
        // microphone at a time, and the publication this one replaces holds
        // it until it stops.
        AudioKind::Microphone(capture) => microphone(&job, capture.clone(), &encoding, &stop).await,
        AudioKind::Pcm { format, fanout } => match fanout.upgrade() {
            Some(fanout) => {
                let frames = fanout.subscribe();
                drop(fanout);
                let _wanted = source.want();
                pcm(&job, &stats, *format, frames, &encoding, &stop).await
            }
            // The producer is already gone.
            None => Ok(()),
        },
        // Another crate in the build turned on microphones in `moq-ext-sources`
        // while this crate's `capture` is off.
        #[cfg(not(feature = "capture"))]
        #[allow(
            unreachable_patterns,
            reason = "a variant exists only with the feature"
        )]
        _ => Err(Error::invalid(
            "publishing a microphone needs the `capture` feature of moq-ext-publisher",
        )),
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
            warn!(error = format_args!("{err:#}"), "audio publish failed");
            job.reporter.slot(SlotState::Failed(ByPtr::new(err)));
        }
    }
}

/// Encodes PCM from a fan-out receiver.
async fn pcm(
    job: &Job,
    stats: &SharedCell<AudioEncodeStats>,
    format: AudioFormat,
    mut frames: broadcast::Receiver<moq_audio::Frame>,
    encoding: &moq_audio::encode::Config,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let track = audio_track_name(encoding);
    let input = moq_audio::encode::Input::new(format.sample_rate, format.layout);
    let mut options = moq_audio::encode::Options::default();
    options.track = Some(track.clone());
    options.config = encoding.clone();
    if let Some(allocator) = job.bandwidth.borrow().clone() {
        options.bandwidth = allocator;
    }
    let mut broadcast = job.producer.clone();
    let mut encoder =
        moq_audio::encode::Producer::new(&mut broadcast, job.catalog.clone(), input, &options)
            .map_err(Error::encoder)?;
    info!(track = %track, rate = format.sample_rate, "publishing audio");
    job.reporter.slot(SlotState::Running);
    job.reporter.rendition(
        &track,
        RenditionState::Encoding {
            encoder: encoding.codec.to_string(),
        },
    );

    let mut source_clock = SourceClock::new(job.clock.clone());
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
        frame.timestamp = source_clock.map(frame.timestamp, None);
        encoder.write(&frame).map_err(Error::encoder)?;
        stats.update(|stats| stats.frames += 1);
    }
    // Every packet is out. Dropping the finished track releases it.
    encoder.finish().map(drop).map_err(Error::encoder)
}

/// Captures and encodes a microphone through moq-audio's publication.
///
/// The capture producer owns the device and the publication encodes one of
/// its consumers. Their futures are not `Send` on macOS, so they run on their
/// own thread.
#[cfg(feature = "capture")]
async fn microphone(
    job: &Job,
    capture: moq_audio::capture::Config,
    encoding: &moq_audio::encode::Config,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let track = audio_track_name(encoding);
    let encode_track = track.clone();
    let encode_config = encoding.clone();
    let bandwidth = job.bandwidth.borrow().clone();

    let (handle_tx, handle) = oneshot::channel();
    let broadcast = job.producer.clone();
    let catalog = job.catalog.clone();
    let driver_stop = stop.child_token();
    let driver = moq_ext_sources::local_task::spawn(
        "audio-capture",
        driver_stop.clone(),
        move |stop| async move {
            let capture = match moq_audio::capture::Producer::open(capture).await {
                Ok(capture) => capture,
                Err(err) => {
                    let _ = handle_tx.send(Err(Error::device(err)));
                    return;
                }
            };
            let mut options = moq_audio::encode::Capture::new(capture.consume());
            options.encode.track = Some(encode_track);
            options.encode.config = encode_config;
            if let Some(allocator) = bandwidth {
                options.encode.bandwidth = allocator;
            }
            let (publication, driver) =
                match moq_audio::encode::publish_capture(broadcast, catalog, options) {
                    Ok(created) => created,
                    Err(err) => {
                        let _ = handle_tx.send(Err(Error::device(err)));
                        return;
                    }
                };
            if handle_tx.send(Ok(publication)).is_err() {
                return;
            }
            // A capture failure also reaches the publication, which reports it
            // as `Status::Failed`, so the capture future only logs it.
            let capture = async move {
                if let Err(err) = capture.run().await {
                    debug!(error = %err, "microphone capture stopped");
                }
                std::future::pending::<()>().await;
            };
            let run = driver.run();
            tokio::pin!(capture, run);
            let result = tokio::select! {
                result = &mut run => result,
                () = &mut capture => return,
                () = stop.cancelled() => return,
            };
            if let Err(err) = result {
                warn!(error = %err, "microphone publication stopped");
            }
        },
    )?;
    let result = follow(handle, &job.reporter, &track, encoding, stop).await;
    // The capture thread holds the device and the echo canceller. A
    // publication replacing this one asks for them as soon as this returns,
    // so the thread must let go first.
    driver_stop.cancel();
    driver.joined().await;
    result
}

/// Reports the microphone's state changes until it ends or the slot stops.
#[cfg(feature = "capture")]
async fn follow(
    handle: oneshot::Receiver<Result<moq_audio::encode::Publication, Error>>,
    reporter: &Reporter,
    track: &str,
    encoding: &moq_audio::encode::Config,
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
            // The driver exited, so the track ended.
            return Ok(());
        };
        match state.status() {
            moq_audio::encode::Status::Starting => reporter.slot(SlotState::Starting),
            moq_audio::encode::Status::Waiting => {
                reporter.slot(SlotState::Running);
                reporter.rendition(track, RenditionState::Idle);
            }
            moq_audio::encode::Status::Live => {
                reporter.slot(SlotState::Running);
                reporter.rendition(
                    track,
                    RenditionState::Encoding {
                        encoder: encoding.codec.to_string(),
                    },
                );
            }
            moq_audio::encode::Status::Failed(err) => {
                // moq-audio does not reopen a failed capture, so the
                // publication ends here and the caller reports the failure.
                return Err(Error::device_msg(err.to_string()));
            }
            moq_audio::encode::Status::Ended | moq_audio::encode::Status::Cancelled => {
                return Ok(());
            }
            _ => {}
        }
    }
}
