use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, trace, warn};

use crate::{
    format::AudioFormat,
    traits::{AudioEncoder, AudioSource, AudioStreamFactory},
    transport::PacketSink,
    util::spawn_thread,
};

/// Standalone audio encoder pipeline.
#[derive(derive_more::Debug)]
pub struct AudioEncoderPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl AudioEncoderPipeline {
    /// Creates a new audio encoder pipeline, using an [`AudioStreamFactory`].
    pub async fn new(
        audio_backend: &dyn AudioStreamFactory,
        encoder: impl AudioEncoder,
        sink: impl PacketSink,
    ) -> Result<Self> {
        let format = AudioFormat::from_config(&encoder.config());
        let source = audio_backend.create_input(format).await?;
        Ok(Self::build(source, encoder, sink))
    }

    /// Creates a new audio encoder pipeline with a pre-made [`AudioSource`].
    pub fn with_source(
        source: Box<dyn AudioSource>,
        encoder: impl AudioEncoder,
        sink: impl PacketSink,
    ) -> Result<Self> {
        let source_format = source.format();
        let enc_config = encoder.config();
        anyhow::ensure!(
            source_format.sample_rate == enc_config.sample_rate
                && source_format.channel_count == enc_config.channel_count,
            "audio source format mismatch: source has {source_format:?}, encoder expects sr={} ch={}",
            enc_config.sample_rate,
            enc_config.channel_count,
        );
        Ok(Self::build(source, encoder, sink))
    }

    fn build(
        mut source: Box<dyn AudioSource>,
        mut encoder: impl AudioEncoder,
        mut sink: impl PacketSink,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let name = encoder.name();
        let thread_name = format!("aenc-{:<4}", name);
        let span = info_span!("audioenc", encoder = %name);
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                let codec_name = encoder.name().to_string();
                info!(config = ?encoder.config(), "encode start");
                const INTERVAL: Duration = Duration::from_millis(20);
                let format = source.format();
                let samples_per_frame = (format.sample_rate / 1000) * INTERVAL.as_millis() as u32;
                let mut buf =
                    vec![0.0f32; samples_per_frame as usize * format.channel_count as usize];
                let start = Instant::now();
                let mut sink_closed = false;
                let sample_rate = format.sample_rate;
                'encode: for tick in 0u64.. {
                    trace!("tick");
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_samples(&mut buf) {
                        Ok(Some(_)) => {
                            if let Err(err) = encoder.push_samples(&buf) {
                                error!(buf_len = buf.len(), "encoder push_samples failed: {err:#}");
                                break;
                            }
                            loop {
                                match encoder.pop_packet() {
                                    Ok(Some(mut pkt)) => {
                                        pkt.timestamp = start.elapsed();
                                        if let Err(err) = sink.write(pkt) {
                                            debug!("sink closed: {err:#}");
                                            sink_closed = true;
                                            break 'encode;
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(err) => {
                                        error!("encoder pop_packet failed: {err:#}");
                                        break 'encode;
                                    }
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            error!("audio source failed: {err:#}");
                            break;
                        }
                    }
                    let expected_time = INTERVAL.mul_f64((tick + 1) as f64);
                    let elapsed = start.elapsed();
                    if elapsed > expected_time {
                        warn!("audio encoder too slow by {:?}", elapsed - expected_time);
                    }
                    let sleep = expected_time.saturating_sub(elapsed);
                    if sleep > Duration::ZERO {
                        thread::sleep(sleep);
                    }

                    throttled_tracing::debug_every!(Duration::from_secs(5),
                        uptime_s = format_args!("{:.0}", start.elapsed().as_secs_f64()),
                        codec = %codec_name,
                        sample_rate,
                        "aenc stats",
                    );
                }
                if !sink_closed {
                    while let Ok(Some(mut pkt)) = encoder.pop_packet() {
                        pkt.timestamp = start.elapsed();
                        if let Err(err) = sink.write(pkt) {
                            debug!("sink closed during drain: {err:#}");
                            break;
                        }
                    }
                    sink.finish().ok();
                }
                info!("encode stop");
            }
        });

        Self {
            shutdown,
            _thread_handle: thread,
        }
    }
}

impl Drop for AudioEncoderPipeline {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
