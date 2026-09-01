//! The audio decode task: one track into the shared playback engine.
//!
//! `moq_audio::playback::Engine` owns the output device and mixes up to 64
//! sinks into it, so a process watching several broadcasts opens one engine and
//! one sink per broadcast. The engine is created lazily and shared, because
//! opening a second output device would fight the first for the speaker.

use n0_error::{Result, e};
use n0_future::task::{AbortOnDropHandle, spawn};
use tracing::{Instrument, debug, error_span, info, warn};

use super::{AudioTrack, RemoteBroadcast, SubscribeError, audio_decode_config};

/// Opens `rendition` and starts playing it.
pub(super) async fn open(
    broadcast: &RemoteBroadcast,
    rendition: &str,
) -> Result<AudioTrack, SubscribeError> {
    let catalog = broadcast.catalog();
    let config = catalog.audio().get(rendition).cloned().ok_or_else(|| {
        e!(SubscribeError::NoRendition {
            name: rendition.to_string(),
        })
    })?;

    let context = broadcast.decode_context();
    let decode = audio_decode_config(&context.policy);
    let mut consumer =
        moq_audio::decode::Consumer::new(broadcast.consumer(), &config, rendition, decode).await?;

    let mut sink = crate::playback::engine()
        .await?
        .sink(moq_audio::playback::Input {
            format: moq_audio::Format::F32,
            sample_rate: consumer.sample_rate(),
            channels: consumer.channels(),
        })?;
    let control = sink.control();
    info!(
        rendition,
        sample_rate = consumer.sample_rate(),
        channels = consumer.channels(),
        "audio playing",
    );

    let task = spawn(
        async move {
            loop {
                tokio::select! {
                    _ = context.shutdown.cancelled() => {
                        debug!("audio playback cancelled");
                        return;
                    }
                    frame = consumer.read() => match frame {
                        Ok(Some(frame)) => {
                            // The video clock steers off how much audio is
                            // still buffered ahead of the speaker, which is the
                            // only latency either side can actually measure.
                            context.sync.set_audio_buffered(Some(sink.buffered()));
                            if let Err(err) = sink.write(&frame.data) {
                                warn!(error = %err, "audio sink write failed");
                                return;
                            }
                        }
                        Ok(None) => {
                            debug!("audio track ended");
                            return;
                        }
                        Err(err) => {
                            warn!(error = %err, "audio decode failed");
                            return;
                        }
                    },
                }
            }
        }
        .instrument(error_span!("audio", broadcast = %broadcast.name())),
    );

    Ok(AudioTrack {
        _broadcast: broadcast.clone(),
        rendition: rendition.to_string(),
        control,
        _task: AbortOnDropHandle::new(task),
    })
}
