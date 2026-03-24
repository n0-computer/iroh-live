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

use super::{DecodeOpts, forward_packets};
use crate::{
    format::AudioFormat,
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
        opts: DecodeOpts,
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
        opts: DecodeOpts,
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

/// Mutable state that belongs to one running audio decode loop.
struct AudioLoopState {
    consecutive_errors: u32,
    last_push: Instant,
    silence_warned: bool,
    last_skip_gen: u64,
    started: bool,
    startup_wait_done: bool,
    last_audio_end_pts: Option<Duration>,
}

impl AudioLoopState {
    fn new(playback_policy: &crate::playout::PlaybackPolicy) -> Self {
        Self {
            consecutive_errors: 0,
            last_push: Instant::now(),
            silence_warned: false,
            last_skip_gen: 0,
            started: false,
            startup_wait_done: !playback_policy.is_audio_master(),
            last_audio_end_pts: None,
        }
    }

    fn reset_for_skip(&mut self, playback_policy: &crate::playout::PlaybackPolicy) {
        self.started = false;
        self.startup_wait_done = !playback_policy.is_audio_master();
        self.last_audio_end_pts = None;
        self.silence_warned = false;
        self.last_push = Instant::now();
    }
}

/// Runs the audio decode loop on an OS thread.
fn audio_decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<crate::format::MediaPacket>,
    mut decoder: impl AudioDecoder,
    mut sink: impl AudioSink,
    opts: DecodeOpts,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    use mpsc::error::TryRecvError;

    let DecodeOpts {
        stats,
        audio_position,
        video_started,
        playback_policy,
        skip_generation,
        ..
    } = opts;

    const INTERVAL: Duration = Duration::from_millis(10);
    const MAX_CONSECUTIVE_ERRORS: u32 = 10;
    const SILENCE_THRESHOLD: Duration = Duration::from_millis(200);
    const SILENCE_FRAME_SAMPLES: usize = 960;

    let loop_start = Instant::now();
    let mut silence_buf = [0.0f32; SILENCE_FRAME_SAMPLES];
    let mut state = AudioLoopState::new(&playback_policy);
    state.last_skip_gen = skip_generation.load(Ordering::Relaxed);
    let startup_deadline = Instant::now() + Duration::from_millis(250);
    let sink_format = sink.format()?;
    let channels = sink_format.channel_count as usize;
    let sample_rate = sink_format.sample_rate as f64;

    'main: for i in 0.. {
        if shutdown.is_cancelled() {
            debug!("stop audio decoder: cancelled");
            break;
        }

        let current_gen = skip_generation.load(Ordering::Relaxed);
        if current_gen != state.last_skip_gen {
            state.last_skip_gen = current_gen;
            info!("audio: video skip detected, draining stale packets to resync");
            let mut drained = 0u32;
            while input_rx.try_recv().is_ok() {
                drained += 1;
            }
            if drained > 0 {
                info!(drained, "audio: discarded stale packets");
            }
            while decoder.pop_samples().ok().flatten().is_some() {}
            state.reset_for_skip(&playback_policy);
            audio_position.reset();
        }

        let mut received_any = false;
        loop {
            match input_rx.try_recv() {
                Ok(packet) => {
                    let packet_pts = packet.timestamp;
                    received_any = true;
                    state.started = true;

                    if !sink.is_paused() {
                        if let Err(err) = decoder.push_packet(packet) {
                            state.consecutive_errors += 1;
                            warn!(
                                consecutive_errors = state.consecutive_errors,
                                "failed to push audio packet: {err:#}"
                            );
                            if state.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                n0_error::bail_any!(
                                    "too many consecutive audio decode errors: {err:#}"
                                );
                            }
                            continue;
                        }
                        match decoder.pop_samples() {
                            Ok(Some(samples)) => {
                                state.consecutive_errors = 0;
                                if !state.startup_wait_done {
                                    while Instant::now() < startup_deadline
                                        && !video_started.load(Ordering::Acquire)
                                        && !shutdown.is_cancelled()
                                    {
                                        thread::sleep(Duration::from_millis(5));
                                    }
                                    state.startup_wait_done = true;
                                }
                                sink.push_samples(samples)?;
                                state.last_push = Instant::now();
                                let frame_count = samples.len() / channels;
                                let chunk_duration =
                                    Duration::from_secs_f64(frame_count as f64 / sample_rate);
                                let buffered = Duration::from_secs_f64(sink.occupied_seconds());
                                audio_position.record(packet_pts, chunk_duration, buffered);
                                stats.timing.record_audio_status(buffered, None);
                                let now = Instant::now();
                                stats.timeline.push(crate::stats::FrameTimingEntry {
                                    kind: crate::stats::FrameKind::Audio,
                                    pts: packet_pts,
                                    receive_wall: now,
                                    decode_end: Some(now),
                                    render_wall: now + buffered,
                                    is_keyframe: false,
                                });
                                state.last_audio_end_pts = Some(packet_pts + chunk_duration);
                                if state.silence_warned {
                                    info!("audio packets resumed, stopping silence insertion");
                                    state.silence_warned = false;
                                }
                            }
                            Ok(None) => {
                                state.consecutive_errors = 0;
                            }
                            Err(err) => {
                                state.consecutive_errors += 1;
                                warn!(
                                    consecutive_errors = state.consecutive_errors,
                                    "failed to pop audio samples: {err:#}"
                                );
                                if state.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                    n0_error::bail_any!(
                                        "too many consecutive audio decode errors: {err:#}"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    debug!("stop audio decoder: packet channel closed");
                    break 'main;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        if !received_any
            && !sink.is_paused()
            && state.last_push.elapsed() > SILENCE_THRESHOLD
            && state.started
        {
            if !state.silence_warned {
                warn!(
                    gap_ms = state.last_push.elapsed().as_millis(),
                    "no audio packets for {}ms, inserting silence",
                    SILENCE_THRESHOLD.as_millis()
                );
                state.silence_warned = true;
            }
            silence_buf.fill(0.0);
            let chunk_duration =
                Duration::from_secs_f64((silence_buf.len() / channels) as f64 / sample_rate);
            let start_pts = state.last_audio_end_pts.unwrap_or_default();
            if sink.push_samples(&silence_buf).is_ok() {
                let buffered = Duration::from_secs_f64(sink.occupied_seconds());
                audio_position.record(start_pts, chunk_duration, buffered);
                stats.timing.record_audio_status(buffered, None);
                let now = Instant::now();
                stats.timeline.push(crate::stats::FrameTimingEntry {
                    kind: crate::stats::FrameKind::Audio,
                    pts: start_pts,
                    receive_wall: now,
                    decode_end: Some(now),
                    render_wall: now + buffered,
                    is_keyframe: false,
                });
                state.last_audio_end_pts = Some(start_pts + chunk_duration);
            }
        }

        let audio_buffered = Duration::from_secs_f64(sink.occupied_seconds());
        let audio_live_lag = audio_position.playback_pts().map(|playback_pts| {
            let latest_pts = state.last_audio_end_pts.unwrap_or(playback_pts);
            latest_pts.saturating_sub(playback_pts)
        });
        stats
            .timing
            .record_audio_status(audio_buffered, audio_live_lag);

        let sleep = (INTERVAL * i).saturating_sub(loop_start.elapsed());
        if !sleep.is_zero() {
            thread::sleep(sleep);
        }
    }
    shutdown.cancel();
    Ok(())
}
