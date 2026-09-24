//! The player's audio task: one rendition into the player's output.

use std::sync::Arc;

use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::Watcher as _;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use super::{Controls, PlaybackRecorder, PlayoutClock, StatusCell};
use crate::{
    AudioOutput, RemoteBroadcast, SlotState,
    error::Error,
    output::{OutputControl, SinkInput},
    stats::AudioPlaybackStats,
};

/// The audio task's inputs.
pub(crate) struct Inputs {
    pub broadcast: RemoteBroadcast,
    pub output: AudioOutput,
    pub controls: Arc<Controls>,
    pub status: StatusCell,
    pub clock: PlayoutClock,
    pub stats: PlaybackRecorder,
    pub shutdown: CancellationToken,
}

/// Plays the broadcast's audio until the player is dropped.
///
/// Reopens on a new catalog or a new route to the broadcast, so audio follows
/// a publisher that restarts its microphone and a subscription that changes
/// path.
pub(crate) async fn run(inputs: Inputs) {
    let Inputs {
        broadcast,
        output,
        controls,
        status,
        clock,
        stats,
        shutdown,
    } = inputs;
    let mut catalog = broadcast.catalog();
    let mut epoch = broadcast.epoch();
    let mut volume = controls.volume.subscribe();

    loop {
        // What to play: the first audio rendition, over the current route.
        let opened = {
            let known = catalog.peek().clone();
            let consumer = epoch.peek().consumer.clone();
            known.zip(consumer).and_then(|(known, consumer)| {
                let info = known.audio().first()?.clone();
                let config = known.hang_audio(&info.name)?.clone();
                Some((info.name, config, consumer))
            })
        };
        let mut reader = match opened {
            Some((name, config, consumer)) => {
                status.update(|status| status.audio = SlotState::Starting);
                let max_age = controls.latency.borrow().max;
                match open(&name, &config, &consumer, max_age, &output).await {
                    Ok((decoder, sink, control)) => {
                        control.set_volume(*volume.borrow());
                        info!(rendition = %name, "audio playing");
                        status.update(|status| status.audio = SlotState::Running);
                        let job = Job {
                            name: name.clone(),
                            decoder,
                            sink,
                            control: control.clone(),
                            latency: clock.register_audio(),
                            stats: stats.clone(),
                        };
                        Some((
                            AbortOnDropHandle::new(spawn(
                                job.run()
                                    .instrument(info_span!("decode", rendition = %name)),
                            )),
                            control,
                        ))
                    }
                    Err(err) => {
                        warn!(error = %err, rendition = %name, "audio failed to open");
                        status.update(|status| status.audio = SlotState::Failed(Arc::new(err)));
                        None
                    }
                }
            }
            None => None,
        };

        // Wait for the reader to end, or for a reason to reopen.
        loop {
            let reading = reader.is_some();
            tokio::select! {
                () = shutdown.cancelled() => return,
                result = async { (&mut reader.as_mut().expect("guarded").0).await }, if reading => {
                    reader = None;
                    stats.audio.update(|audio| *audio = None);
                    match result {
                        Ok(Ok(())) => status.update(|status| status.audio = SlotState::Ended),
                        Ok(Err(err)) => {
                            warn!(error = %err, "audio stopped");
                            status.update(|status| status.audio = SlotState::Failed(Arc::new(err)));
                        }
                        Err(err) => warn!(error = %err, "the audio task panicked"),
                    }
                }
                updated = catalog.updated() => {
                    if updated.is_err() {
                        return;
                    }
                    // A running track keeps playing across a catalog update;
                    // only one that ended or never opened is tried again.
                    if !reading {
                        break;
                    }
                }
                updated = epoch.updated() => {
                    if updated.is_err() {
                        return;
                    }
                    debug!("the broadcast moved to a new route, reopening audio");
                    break;
                }
                changed = volume.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if let Some((_, control)) = &reader {
                        control.set_volume(*volume.borrow());
                    }
                }
            }
        }
    }
}

/// Opens the decoder and the output sink for one rendition.
async fn open(
    name: &str,
    config: &hang::catalog::AudioConfig,
    consumer: &moq_net::broadcast::Consumer,
    max_age: std::time::Duration,
    output: &AudioOutput,
) -> Result<
    (
        moq_audio::decode::Consumer,
        crate::output::OutputSink,
        OutputControl,
    ),
    Error,
> {
    let mut options = moq_audio::decode::Options::new();
    options.output.format = moq_audio::Format::F32;
    options.max_age = max_age;
    // The live edge, as a player wants: the backlog a track holds is behind it.
    options.start = moq_audio::decode::Start::Latest;
    let decoder = moq_audio::decode::Consumer::new(consumer, config, name, options)
        .await
        .map_err(Error::decoder)?;
    let sink = output.sink(SinkInput {
        sample_rate: decoder.sample_rate(),
        layout: decoder.layout(),
    })?;
    let control = sink.control();
    Ok((decoder, sink, control))
}

/// One rendition being decoded into the output.
struct Job {
    name: String,
    decoder: moq_audio::decode::Consumer,
    sink: crate::output::OutputSink,
    control: OutputControl,
    /// Clears this track's contribution to the clock on every way out.
    latency: super::clock::AudioLatency,
    stats: PlaybackRecorder,
}

impl Job {
    /// Decodes until the track ends.
    ///
    /// The read is the only await, so nothing cancels it but the task's own
    /// end: the decoder is never polled again after a dropped read.
    async fn run(mut self) -> Result<(), Error> {
        self.stats.audio.update(|audio| {
            *audio = Some(AudioPlaybackStats {
                rendition: self.name.clone(),
                ..AudioPlaybackStats::default()
            });
        });
        while let Some(frame) = self.decoder.read().await.map_err(Error::decoder)? {
            // The video clock steers off how much audio is still buffered
            // ahead of the speaker, which is the only latency either side can
            // actually measure.
            let buffered = self.sink.buffered();
            self.latency.set(buffered);
            self.sink.write(&frame.data)?;
            let peak = self.control.peak();
            self.stats.audio.update(|audio| {
                if let Some(audio) = audio.as_mut() {
                    audio.frames += 1;
                    audio.buffered = buffered;
                    audio.peak = peak;
                }
            });
        }
        debug!("audio track ended");
        Ok(())
    }
}
