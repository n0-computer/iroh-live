use std::{
    future::Future,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, warn};

use super::{PipelineContext, forward_packets};
use crate::{
    format::AudioFormat,
    stats::LagTracker,
    traits::{AudioDecoder, AudioSink, AudioSinkHandle, AudioStreamFactory},
    util::spawn_thread,
};

/// Standalone audio decoder pipeline.
#[derive(derive_more::Debug)]
pub struct AudioDecoderPipeline {
    name: String,
    shutdown: CancellationToken,
    #[debug(skip)]
    handle: Box<dyn AudioSinkHandle>,
    #[debug(skip)]
    _task_handle: AbortOnDropHandle<()>,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl AudioDecoderPipeline {
    /// Creates a new audio decoder pipeline using an [`AudioStreamFactory`].
    pub async fn new<D: AudioDecoder>(
        name: String,
        source: impl crate::transport::PacketSource,
        config: &rusty_codecs::config::AudioConfig,
        audio_backend: &dyn AudioStreamFactory,
        opts: PipelineContext,
    ) -> Result<Self> {
        let target_format = AudioFormat::from_config(config);
        let sink = audio_backend.create_output(target_format).await?;
        let handle = sink.handle();
        Self::build::<D>(name, source, config, sink, handle, opts)
    }

    fn build<D: AudioDecoder>(
        name: String,
        source: impl crate::transport::PacketSource,
        config: &rusty_codecs::config::AudioConfig,
        sink: impl AudioSink,
        handle: Box<dyn AudioSinkHandle>,
        opts: PipelineContext,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        let span = info_span!("audiodec", %name);
        let output_format = sink.format()?;
        let decoder = D::new(config, output_format)?;

        let (packet_tx, packet_rx) = mpsc::channel(32);
        let thread_name = format!("adec-{}", name);
        let config = config.clone();
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                info!(?config, "decode start");
                if let Err(err) = audio_decode_loop(&shutdown, packet_rx, decoder, sink, opts) {
                    error!("decoder failed: {err:#}");
                }
                info!("decode stop");
            }
        });
        let task = tokio::spawn(forward_packets(source, packet_tx));
        Ok(Self {
            name,
            shutdown,
            handle,
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn handle(&self) -> &dyn AudioSinkHandle {
        self.handle.as_ref()
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + 'static {
        let shutdown = self.shutdown.clone();
        async move { shutdown.cancelled().await }
    }

    pub fn is_stopped(&self) -> bool {
        self.shutdown.is_cancelled()
    }
}

impl Drop for AudioDecoderPipeline {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Core audio decode loop. Runs on a dedicated OS thread.
///
/// Decodes packets as they arrive and pushes samples to the audio sink
/// immediately. The sink's internal ring buffer provides the front-buffering
/// that smooths jitter. If no packets arrive and the sink buffer runs low,
/// silence is inserted to prevent audible underruns.
fn audio_decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<crate::format::MediaPacket>,
    mut decoder: impl AudioDecoder,
    mut sink: impl AudioSink,
    opts: PipelineContext,
) -> Result<()> {
    use mpsc::error::TryRecvError;

    let stats = opts.stats;

    /// Tick interval — how often we check for packets and sink state.
    const TICK: Duration = Duration::from_millis(10);
    const MAX_CONSECUTIVE_ERRORS: u32 = 10;
    /// Insert silence when the sink buffer drops below this threshold.
    const LOW_BUFFER_THRESHOLD: Duration = Duration::from_millis(20);
    /// How many silence samples to insert per underrun (20ms at 48kHz mono).
    const SILENCE_SAMPLES: usize = 960;

    let loop_start = Instant::now();
    let sink_format = sink.format()?;
    let channels = sink_format.channel_count as usize;
    let sample_rate = sink_format.sample_rate as f64;
    let silence_buf = vec![0.0f32; SILENCE_SAMPLES];

    let mut consecutive_errors = 0u32;
    let mut started = false;
    let mut last_pts = Duration::ZERO;
    let mut silence_warned = false;

    let mut lag = LagTracker::new();

    for tick_num in 0u64.. {
        if shutdown.is_cancelled() {
            debug!("audio decode: cancelled");
            break;
        }

        // Drain all available packets.
        let mut received_any = false;
        loop {
            match input_rx.try_recv() {
                Ok(packet) => {
                    received_any = true;
                    started = true;
                    let packet_pts = packet.timestamp;
                    let received = Instant::now();

                    if sink.is_paused() {
                        continue;
                    }

                    if let Err(err) = decoder.push_packet(packet) {
                        consecutive_errors += 1;
                        warn!(consecutive_errors, "audio push_packet failed: {err:#}");
                        if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                            n0_error::bail_any!(
                                "too many consecutive audio decode errors: {err:#}"
                            );
                        }
                        continue;
                    }

                    match decoder.pop_samples() {
                        Ok(Some(samples)) => {
                            consecutive_errors = 0;
                            silence_warned = false;
                            sink.push_samples(samples)?;

                            let decoded = Instant::now();
                            let buffered = Duration::from_secs_f64(sink.occupied_seconds());
                            let frame_count = samples.len() / channels;
                            let chunk_duration =
                                Duration::from_secs_f64(frame_count as f64 / sample_rate);

                            // Timeline entry.
                            stats.timeline.push(crate::stats::FrameMeta {
                                kind: crate::stats::FrameKind::Audio,
                                pts: packet_pts,
                                received,
                                decoded: Some(decoded),
                                rendered: decoded + buffered,
                                is_keyframe: false,
                            });

                            // Lag at the speaker: wall + buffered vs PTS.
                            stats
                                .timing
                                .audio_lag_ms
                                .record(lag.record_ms(decoded + buffered, packet_pts));

                            last_pts = packet_pts + chunk_duration;
                        }
                        Ok(None) => {
                            consecutive_errors = 0;
                        }
                        Err(err) => {
                            consecutive_errors += 1;
                            warn!(consecutive_errors, "audio pop_samples failed: {err:#}");
                            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                n0_error::bail_any!(
                                    "too many consecutive audio decode errors: {err:#}"
                                );
                            }
                        }
                    }

                    last_pts = last_pts.max(packet_pts);
                }
                Err(TryRecvError::Disconnected) => {
                    debug!("audio decode: input channel closed");
                    shutdown.cancel();
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        // If the sink buffer is running low and we have no new packets,
        // insert a small silence chunk to prevent audible underruns.
        let buffered = Duration::from_secs_f64(sink.occupied_seconds());
        if started && !received_any && !sink.is_paused() && buffered < LOW_BUFFER_THRESHOLD {
            if !silence_warned {
                warn!(
                    buffered_ms = buffered.as_millis(),
                    "audio buffer low, inserting silence"
                );
                silence_warned = true;
            }
            let _ = sink.push_samples(&silence_buf);
        }

        // Record audio buffer level every tick.
        stats
            .timing
            .audio_buf_ms
            .record_ms(Duration::from_secs_f64(sink.occupied_seconds()));

        // Sleep to maintain tick cadence.
        let target = TICK * tick_num as u32;
        let elapsed = loop_start.elapsed();
        let sleep = target.saturating_sub(elapsed);
        if !sleep.is_zero() {
            thread::sleep(sleep);
        }
    }
    shutdown.cancel();
    Ok(())
}
