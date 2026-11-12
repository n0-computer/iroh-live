//! The player's audio task: one rendition into the player's output.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(feature = "playback")]
use moq_audio::playback;
use moq_ext_sources::{AudioFormat, ByPtr, Error, SlotState, send_if_changed};
use moq_net::track::Delay;
use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::sync::{broadcast, watch};
use tracing::{Instrument, debug, info, info_span, warn};

use super::{Controls, PlaybackRecorder, PlayerStatus, PlayoutClock, clock::AudioLatency};
use crate::{
    RemoteBroadcast,
    clean_close::CleanClose as _,
    stats::{AudioPlaybackStats, FrameTiming, MediaKind},
};

/// How long after audio ended or failed the catalog is read again.
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// How many decoded frames an [`AudioFrames`] reader may fall behind before it skips.
///
/// One second of 20 ms Opus frames.
pub(crate) const AUDIO_FRAMES_BACKLOG: usize = 50;

/// One block of decoded audio, from [`Player::audio_frames`](crate::Player::audio_frames).
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// The presentation timestamp of the first sample, on the publisher's broadcast clock.
    pub timestamp: moq_net::Timestamp,
    /// The sample rate and the channel layout of [`samples`](Self::samples).
    pub format: AudioFormat,
    /// The samples, interleaved, from -1.0 to 1.0.
    pub samples: Arc<[f32]>,
}

impl AudioFrame {
    /// Returns how long the frame plays.
    #[must_use]
    pub fn duration(&self) -> Duration {
        play_length(self.format, self.samples.len())
    }
}

/// Returns how long `samples` interleaved samples of `format` play.
fn play_length(format: AudioFormat, samples: usize) -> Duration {
    let per_channel = samples / format.layout.channels().max(1) as usize;
    Duration::from_secs_f64(per_channel as f64 / f64::from(format.sample_rate.max(1)))
}

/// The decoded audio of a player, in the order it decodes.
///
/// Returned by [`Player::audio_frames`](crate::Player::audio_frames). Each
/// handle reads every frame decoded after it was created. A handle that falls
/// more than a second behind loses the oldest frames, so the decoder never
/// waits for a reader.
#[derive(Debug)]
pub struct AudioFrames {
    rx: broadcast::Receiver<AudioFrame>,
}

impl AudioFrames {
    /// Creates a handle that reads what `tx` sends from now on.
    pub(crate) fn new(tx: &broadcast::Sender<AudioFrame>) -> Self {
        Self { rx: tx.subscribe() }
    }

    /// Waits for the next decoded frame.
    ///
    /// Frames decoded before the player was dropped are still returned, up to
    /// the backlog a handle keeps. Returns `None` once the player is dropped
    /// and this handle has read them. Cancellation safe.
    pub async fn next(&mut self) -> Option<AudioFrame> {
        loop {
            match self.rx.recv().await {
                Ok(frame) => return Some(frame),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    debug!(skipped, "an audio frames reader fell behind and skipped");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// The audio task's inputs.
pub(crate) struct Inputs {
    pub broadcast: RemoteBroadcast,
    /// The speaker, or `None` to decode without playing.
    #[cfg(feature = "playback")]
    pub speaker: Option<playback::Engine>,
    /// Where each decoded frame goes, for [`AudioFrames`] readers.
    pub frames: broadcast::Sender<AudioFrame>,
    pub controls: Arc<Controls>,
    pub status: watch::Sender<PlayerStatus>,
    pub clock: PlayoutClock,
    pub stats: PlaybackRecorder,
}

/// Plays the broadcast's audio until the player is dropped or the broadcast closes.
///
/// Reopens on a new catalog or a new route. Audio then follows a publisher
/// that restarts its microphone, and a subscription that changes path.
pub(crate) async fn run(inputs: Inputs) {
    let Inputs {
        broadcast,
        #[cfg(feature = "playback")]
        speaker,
        frames,
        controls,
        status,
        clock,
        stats,
    } = inputs;
    let mut catalog = broadcast.catalog();
    let mut epoch = broadcast.epoch();
    let mut volume = controls.volume.subscribe();

    loop {
        // What to play: the first audio rendition, over the current route.
        let opened = {
            let known = catalog.borrow_and_update().clone();
            let consumer = epoch.borrow_and_update().consumer.clone();
            known.zip(consumer).and_then(|(known, consumer)| {
                let (name, config) = known.audio.renditions.first_key_value()?;
                Some((name.clone(), config.clone(), consumer))
            })
        };
        let mut reader = match opened {
            Some((name, config, consumer)) => {
                send_if_changed(&status, |status| status.audio = SlotState::Starting);
                let delay = *controls.latency.borrow();
                let opened: Result<Job, Error> = async {
                    let decoder = open(&name, &config, &consumer, delay).await?;
                    let format = AudioFormat {
                        sample_rate: decoder.sample_rate(),
                        layout: decoder.layout(),
                    };
                    #[cfg(feature = "playback")]
                    let sink = speaker
                        .as_ref()
                        .map(|engine| sink(engine, format))
                        .transpose()?;
                    Ok(Job {
                        name: name.clone(),
                        format,
                        frames: frames.clone(),
                        decoder,
                        #[cfg(feature = "playback")]
                        sink,
                        latency: clock.register_audio(),
                        stats: stats.clone(),
                    })
                }
                .await;
                match opened {
                    Ok(job) => {
                        let control = job.control();
                        set_volume(control.as_ref(), *volume.borrow());
                        info!(rendition = %name, "audio playing");
                        send_if_changed(&status, |status| status.audio = SlotState::Running);
                        Some((
                            AbortOnDropHandle::new(spawn(
                                job.run()
                                    .instrument(info_span!("decode", rendition = %name)),
                            )),
                            control,
                        ))
                    }
                    // The session closed while the track opened. The epoch
                    // or the catalog changes next, or the broadcast closes,
                    // and nothing failed.
                    Err(err @ Error::Closed { .. }) => {
                        debug!(error = %err, rendition = %name, "audio closed before it opened");
                        send_if_changed(&status, |status| status.audio = SlotState::Ended);
                        None
                    }
                    Err(err) => {
                        warn!(error = format_args!("{err:#}"), rendition = %name, "audio failed to open");
                        send_if_changed(&status, |status| {
                            status.audio = SlotState::Failed(ByPtr::new(err))
                        });
                        None
                    }
                }
            }
            None => None,
        };

        // Wait for the reader to end, or for a reason to reopen. Without a
        // reader, the catalog is also read again after a pause. A publisher
        // that replaced its audio may have sent the new catalog before the old
        // track ended here, and no later update follows.
        let mut retry = std::pin::pin!(tokio::time::sleep(RETRY_AFTER));
        loop {
            let reading = reader.is_some();
            tokio::select! {
                // The task holds the broadcast, so the watches below never
                // close, and only this tells it that nothing comes back.
                () = broadcast.closed() => {
                    debug!("the broadcast closed, so the audio is over");
                    drop(reader);
                    stats.audio.update(|audio| *audio = None);
                    send_if_changed(&status, |status| {
                        if matches!(status.audio, SlotState::Running | SlotState::Starting) {
                            status.audio = SlotState::Ended;
                        }
                    });
                    return;
                }
                result = async { (&mut reader.as_mut().expect("guarded").0).await }, if reading => {
                    reader = None;
                    stats.audio.update(|audio| *audio = None);
                    retry.as_mut().reset(tokio::time::Instant::now() + RETRY_AFTER);
                    match result {
                        Ok(Ok(())) => send_if_changed(&status, |status| status.audio = SlotState::Ended),
                        Ok(Err(err)) => {
                            warn!(error = format_args!("{err:#}"), "audio stopped");
                            send_if_changed(&status, |status| status.audio = SlotState::Failed(ByPtr::new(err)));
                        }
                        Err(err) => warn!(error = %err, "the audio task panicked"),
                    }
                }
                changed = catalog.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    // A running track keeps playing across a catalog update.
                    // Only a track that ended or never opened is tried again.
                    if !reading {
                        break;
                    }
                }
                changed = epoch.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    debug!("the broadcast moved to a new route, reopening audio");
                    break;
                }
                () = &mut retry, if !reading => break,
                changed = volume.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if let Some((_, control)) = &reader {
                        set_volume(control.as_ref(), *volume.borrow());
                    }
                }
            }
        }
    }
}

/// Opens the decoder for one rendition.
async fn open(
    name: &str,
    config: &hang::catalog::AudioConfig,
    consumer: &moq_net::broadcast::Consumer,
    delay: Delay,
) -> Result<moq_audio::decode::Consumer, Error> {
    let mut options = moq_audio::decode::Options::new();
    options.output.format = moq_audio::Format::F32;
    options.delay = delay;
    // Start at the live edge. The backlog a track holds is behind it.
    options.start = moq_audio::decode::Start::Latest;
    let decoder = moq_audio::decode::Consumer::new(consumer, config, name, options)
        .await
        .map_err(|err| {
            if err.is_clean_close() {
                n0_error::e!(Error::Closed)
            } else {
                Error::decoder(err)
            }
        })?;
    Ok(decoder)
}

/// Adds a stream to the speaker's mix, taking PCM in `format`.
#[cfg(feature = "playback")]
fn sink(engine: &playback::Engine, format: AudioFormat) -> Result<playback::Sink, Error> {
    let mut input = playback::Input::default();
    input.format = moq_audio::Format::F32;
    input.sample_rate = format.sample_rate;
    input.layout = format.layout;
    engine.sink(input).map_err(Error::device)
}

/// Sets the gain of a speaker stream, where 1.0 is unattenuated.
fn set_volume(control: Option<&Control>, volume: f32) {
    #[cfg(feature = "playback")]
    if let Some(control) = control {
        control.set_volume(volume);
    }
    #[cfg(not(feature = "playback"))]
    let _ = (control, volume);
}

/// The gain and level handle of a speaker stream.
#[cfg(feature = "playback")]
type Control = playback::Control;
/// No speaker stream exists without the `playback` feature.
#[cfg(not(feature = "playback"))]
type Control = std::convert::Infallible;

/// One rendition being decoded into the output.
struct Job {
    name: String,
    /// What the decoder delivers: interleaved `f32` at its own rate and layout.
    format: AudioFormat,
    /// Where each decoded frame goes, for [`AudioFrames`] readers.
    frames: broadcast::Sender<AudioFrame>,
    decoder: moq_audio::decode::Consumer,
    /// The stream into the speaker, `None` when the audio is only decoded.
    #[cfg(feature = "playback")]
    sink: Option<playback::Sink>,
    /// Hands the clock's anchor back to video when dropped.
    latency: AudioLatency,
    stats: PlaybackRecorder,
}

impl Job {
    /// Decodes until the track ends or is closed.
    ///
    /// The read is the only await, and only the task's end cancels it. The
    /// decoder is never polled again after a dropped read.
    async fn run(mut self) -> Result<(), Error> {
        self.stats.audio.update(|audio| {
            *audio = Some(AudioPlaybackStats {
                rendition: self.name.clone(),
                ..AudioPlaybackStats::default()
            });
        });
        loop {
            let read = match self.decoder.read().await {
                Ok(Some(read)) => read,
                Ok(None) => break,
                Err(err) if err.is_clean_close() => {
                    debug!(error = %err, "audio track closed");
                    return Ok(());
                }
                Err(err) => return Err(Error::decoder(err)),
            };
            // A gap or an end carries no samples. The sink plays on from what
            // it has queued, so only decoded audio is written.
            let moq_audio::decode::Read::Frame(frame) = read else {
                continue;
            };
            let decoded = Instant::now();
            let buffered = self.buffered();
            #[cfg(feature = "playback")]
            if let Some(sink) = &mut self.sink {
                sink.write(&frame.data).map(drop).map_err(Error::device)?;
            }
            // The speaker owns the playout clock: video shows each picture
            // when the sound of its time plays.
            self.latency.set(self.end_of(&frame), self.buffered());
            self.share(&frame);
            self.stats.audio_log.push(FrameTiming {
                kind: MediaKind::Audio,
                pts: frame.timestamp.into(),
                arrived: None,
                decoded,
                presented: Instant::now() + buffered,
            });
            let peak = self.peak();
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

    /// Returns the handle that sets the speaker stream's gain, if there is one.
    fn control(&self) -> Option<Control> {
        #[cfg(feature = "playback")]
        return self.sink.as_ref().map(playback::Sink::control);
        #[cfg(not(feature = "playback"))]
        None
    }

    /// Returns how much audio is queued ahead of the speaker.
    fn buffered(&self) -> Duration {
        #[cfg(feature = "playback")]
        if let Some(sink) = &self.sink {
            return sink.buffered();
        }
        Duration::ZERO
    }

    /// Returns the speaker stream's most recent peak level, for a meter.
    fn peak(&self) -> f32 {
        #[cfg(feature = "playback")]
        if let Some(sink) = &self.sink {
            return sink.control().peak();
        }
        0.0
    }

    /// Returns the media time at the end of `frame`'s samples.
    fn end_of(&self, frame: &moq_audio::Frame) -> Duration {
        let samples = frame.data.len() / std::mem::size_of::<f32>();
        Duration::from(frame.timestamp) + play_length(self.format, samples)
    }

    /// Hands `frame` to the [`AudioFrames`] readers, if there are any.
    fn share(&self, frame: &moq_audio::Frame) {
        if self.frames.receiver_count() == 0 {
            return;
        }
        let channels = self.format.layout.channels();
        match moq_audio::Format::F32.as_interleaved_f32(&frame.data, channels) {
            Ok(samples) => {
                let _ = self.frames.send(AudioFrame {
                    timestamp: frame.timestamp,
                    format: self.format,
                    samples: samples.into(),
                });
            }
            Err(err) => debug!(error = %err, "a decoded audio frame is not whole f32 samples"),
        }
    }
}
