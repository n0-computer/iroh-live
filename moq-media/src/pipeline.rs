use std::{
    future::Future,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use rusty_codecs::config::{AudioConfig, VideoConfig};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, trace, warn};

use crate::{
    format::{AudioFormat, DecodeConfig, MediaPacket, VideoFrame},
    playout::{PlayoutBuffer, PlayoutClock, RecvResult},
    traits::{
        AudioDecoder, AudioEncoder, AudioSink, AudioSinkHandle, AudioSource, AudioStreamFactory,
        PreEncodedVideoSource, VideoDecoder, VideoEncoder, VideoSource,
    },
    transport::{PacketSink, PacketSource},
    util::spawn_thread,
};

/// Forwards packets from an async [`PacketSource`] into an mpsc channel.
pub(crate) async fn forward_packets(
    mut source: impl PacketSource,
    sender: mpsc::Sender<MediaPacket>,
) {
    loop {
        match source.read().await {
            Ok(Some(packet)) => {
                if sender.send(packet).await.is_err() {
                    debug!("forward_packets: decoder channel closed");
                    break;
                }
            }
            Ok(None) => {
                debug!("forward_packets: source ended");
                break;
            }
            Err(err) => {
                error!("forward_packets: failed to read from source: {err:#}");
                break;
            }
        }
    }
}

/// Standalone video decoder pipeline.
///
/// Reads encoded packets from any [`PacketSource`], decodes on an OS thread,
/// and outputs [`VideoFrame`]s via an mpsc channel. Works without MoQ
/// networking — e.g., with a [`PipeSource`](crate::transport::PipeSource)
/// for local encode→decode pipelines.
#[derive(Debug)]
pub struct VideoDecoderPipeline {
    pub frames: VideoDecoderFrames,
    pub handle: VideoDecoderHandle,
}

/// Receiving end of decoded frames from a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderFrames {
    rx: mpsc::Receiver<VideoFrame>,
}

impl VideoDecoderFrames {
    /// Returns the most recent decoded frame, draining any older buffered frames.
    pub fn current_frame(&mut self) -> Option<VideoFrame> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        latest
    }

    /// Receives the next decoded frame, blocking until one is available.
    pub fn recv_blocking(&mut self) -> Option<VideoFrame> {
        self.rx.blocking_recv()
    }

    /// Consumes this and returns the underlying receiver.
    pub fn into_rx(self) -> mpsc::Receiver<VideoFrame> {
        self.rx
    }
}

/// Control handle for a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderHandle {
    rendition: String,
    decoder_name: String,
    pub(crate) viewport: n0_watcher::Watchable<(u32, u32)>,
    _guard: PipelineGuard,
}

impl VideoDecoderHandle {
    pub fn set_viewport(&self, w: u32, h: u32) {
        let _ = self.viewport.set((w, h));
    }

    pub fn rendition(&self) -> &str {
        &self.rendition
    }

    pub fn decoder_name(&self) -> &str {
        &self.decoder_name
    }
}

/// Keeps pipeline resources alive; dropping cancels everything.
#[derive(derive_more::Debug)]
struct PipelineGuard {
    #[debug(skip)]
    _shutdown_token_guard: tokio_util::sync::DropGuard,
    #[debug(skip)]
    _task_handle: Option<AbortOnDropHandle<()>>,
    #[debug(skip)]
    _thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl VideoDecoderPipeline {
    /// Creates a new decoder pipeline from any packet source.
    ///
    /// Dropping the pipeline cancels the decode thread. The pipeline also
    /// shuts down automatically when the packet source closes.
    pub fn new<D: VideoDecoder>(
        name: String,
        source: impl PacketSource,
        config: &VideoConfig,
        decode_config: &DecodeConfig,
    ) -> Result<Self> {
        Self::with_clock::<D>(name, source, config, decode_config, None)
    }

    /// Creates a new decoder pipeline with a shared [`PlayoutClock`] for
    /// playout timing and A/V sync.
    pub fn with_clock<D: VideoDecoder>(
        name: String,
        source: impl PacketSource,
        config: &VideoConfig,
        decode_config: &DecodeConfig,
        clock: Option<PlayoutClock>,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let viewport = n0_watcher::Watchable::new((1u32, 1u32));
        let viewport_watcher = viewport.watch();

        let decoder = D::new(config, decode_config)?;
        let decoder_name = decoder.name().to_string();
        let span = info_span!("videodec", %name, decoder = %decoder_name);

        let thread_name = format!("vdec-{name}");
        let decoder_name_for_handle = decoder_name;
        let framerate = config.framerate.unwrap_or(30.0);
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                info!("decode start");
                if let Err(err) = decode_loop(
                    &shutdown,
                    packet_rx,
                    frame_tx,
                    viewport_watcher,
                    decoder,
                    clock,
                    framerate,
                ) {
                    error!("decoder failed: {err:#}");
                }
                info!("decode stop");
                shutdown.cancel();
            }
        });

        let task = tokio::spawn(forward_packets(source, packet_tx));

        let guard = PipelineGuard {
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: Some(AbortOnDropHandle::new(task)),
            _thread_handle: Some(thread),
        };

        Ok(Self {
            frames: VideoDecoderFrames { rx: frame_rx },
            handle: VideoDecoderHandle {
                rendition: name,
                decoder_name: decoder_name_for_handle,
                viewport,
                _guard: guard,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Video Encoder Pipeline
// ---------------------------------------------------------------------------

/// Standalone video encoder pipeline.
///
/// Captures frames from a [`VideoSource`], encodes them on an OS thread,
/// and sends encoded packets to any [`PacketSink`]. Works without MoQ
/// networking — e.g., paired with a [`VideoDecoderPipeline`] via
/// [`media_pipe`](crate::transport::media_pipe) for local encode→decode loops.
#[derive(derive_more::Debug)]
pub struct VideoEncoderPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl VideoEncoderPipeline {
    /// Creates a new encoder pipeline.
    ///
    /// Spawns an OS thread that captures frames from `source`, encodes them
    /// with `encoder`, and writes the resulting packets to `sink`.
    pub fn new(
        mut source: impl VideoSource,
        mut encoder: impl VideoEncoder,
        mut sink: impl PacketSink,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let thread_name = format!("venc-{:<4}-{:<4}", source.name(), encoder.name());
        let span = info_span!("videoenc", source = %source.name(), encoder = %encoder.name());
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                if let Err(err) = source.start() {
                    error!("video source failed to start: {err:#}");
                    return;
                }
                let mut first = true;
                let format = source.format();
                let enc_config = encoder.config();
                info!(src_format = ?format, "encode start");
                debug!(dst_config = ?enc_config);
                let framerate = enc_config.framerate.unwrap_or(30.0);
                let interval = Duration::from_secs_f64(1. / framerate);
                let mut sink_closed = false;
                'encode: loop {
                    let start = Instant::now();
                    if shutdown.is_cancelled() {
                        break;
                    }
                    let frame = match source.pop_frame() {
                        Ok(frame) => frame,
                        Err(err) => {
                            error!("video source failed to produce frame: {err:#}");
                            break;
                        }
                    };
                    if let Some(frame) = frame {
                        // Frames are passed to the encoder at source resolution.
                        // The encoder handles any needed scaling internally
                        // (VAAPI: GPU-side VPP, software: CPU scaler).
                        if let Err(err) = encoder.push_frame(frame) {
                            error!("encoder push_frame failed: {err:#}");
                            break;
                        };
                        loop {
                            match encoder.pop_packet() {
                                Ok(Some(pkt)) => {
                                    if first && !pkt.is_keyframe {
                                        debug!("ignoring frame: waiting for first keyframe");
                                        continue;
                                    }
                                    first = false;
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
                    thread::sleep(interval.saturating_sub(start.elapsed()));
                }
                if !sink_closed {
                    sink.finish().ok();
                }
                if let Err(err) = source.stop() {
                    warn!("video source failed to stop: {err:#}");
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

impl Drop for VideoEncoderPipeline {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// ---------------------------------------------------------------------------
// Pre-Encoded Video Pipeline – passthrough (no encoder)
// ---------------------------------------------------------------------------

/// Pipeline for pre-encoded video sources that produce compressed packets
/// directly, bypassing the encoder stage entirely.
///
/// Used when the capture device or an external tool already outputs encoded
/// video (e.g. `rpicam-vid --codec h264` on Raspberry Pi, hardware RTSP
/// cameras, or file demuxers).
#[derive(derive_more::Debug)]
pub struct PreEncodedVideoPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl PreEncodedVideoPipeline {
    /// Starts the passthrough pipeline.
    ///
    /// Spawns an OS thread that reads encoded frames from `source` and writes
    /// them to `sink`. No encoding or transcoding happens.
    pub fn new(mut source: impl PreEncodedVideoSource, mut sink: impl PacketSink) -> Self {
        let shutdown = CancellationToken::new();
        let thread_name = format!("vpre-{}", source.name());
        let span = info_span!("videopre", source = %source.name());
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                if let Err(err) = source.start() {
                    error!("pre-encoded source failed to start: {err:#}");
                    return;
                }
                info!(config = ?source.config(), "pre-encoded pipeline started");
                let mut first = true;
                let mut sink_closed = false;
                loop {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_packet() {
                        Ok(Some(pkt)) => {
                            if first && !pkt.is_keyframe {
                                debug!("ignoring frame: waiting for first keyframe");
                                continue;
                            }
                            first = false;
                            if let Err(err) = sink.write(pkt) {
                                debug!("sink closed: {err:#}");
                                sink_closed = true;
                                break;
                            }
                        }
                        Ok(None) => {
                            // No frame ready — brief yield to avoid busy-spin.
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(err) => {
                            error!("pre-encoded source error: {err:#}");
                            break;
                        }
                    }
                }
                if !sink_closed {
                    sink.finish().ok();
                }
                if let Err(err) = source.stop() {
                    warn!("pre-encoded source failed to stop: {err:#}");
                }
                info!("pre-encoded pipeline stopped");
            }
        });

        Self {
            shutdown,
            _thread_handle: thread,
        }
    }
}

impl Drop for PreEncodedVideoPipeline {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// ---------------------------------------------------------------------------
// Video Decoder Pipeline – decode loop
// ---------------------------------------------------------------------------

/// The core decode loop, running on an OS thread.
///
/// Reads `MediaPacket`s from the channel, feeds them to the decoder,
/// drains decoded frames into the [`PlayoutBuffer`], and releases them
/// to the output channel at PTS-correct intervals.
///
/// The playout buffer smooths bursty DPB output from hardware decoders.
/// In [`PlayoutMode::Reliable`] (buffer=0), frames are released immediately
/// after decode — equivalent to the original direct-send path.
///
/// Uses `blocking_recv()` when the buffer is empty (zero CPU overhead),
/// and timed waits only when frames are buffered and awaiting playout.
fn decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<MediaPacket>,
    output_tx: mpsc::Sender<VideoFrame>,
    mut viewport_watcher: n0_watcher::Direct<(u32, u32)>,
    mut decoder: impl VideoDecoder,
    clock: Option<PlayoutClock>,
    framerate: f64,
) -> Result<()> {
    let mut waiting_for_keyframe = false;
    let mut last_send = Instant::now();
    let mut frames_pushed = 0u64;
    let mut frames_popped = 0u64;
    let mut frames_skipped = 0u64;
    // Track wall-clock vs PTS progression to estimate hidden latency.
    // If wall_elapsed grows faster than pts_elapsed, we're falling behind.
    // stream_start_wall is set when first_pts is set (first decoded frame),
    // and both are reset together during skip recovery.
    let mut stream_start_wall: Option<Instant> = None;
    let mut first_pts: Option<Duration> = None;
    let mut latest_pts = Duration::ZERO;
    let mut skip_active = false;

    // When no clock is provided (local pipelines), send frames directly.
    let Some(clock) = clock else {
        return decode_loop_direct(
            shutdown,
            &mut input_rx,
            &output_tx,
            &mut viewport_watcher,
            &mut decoder,
        );
    };

    // Set buffer duration from the decoder's burst size. Hardware decoders
    // (e.g. VAAPI) flush multiple frames at once from their DPB — the
    // playout buffer must be large enough to smooth these bursts.
    let burst = decoder.burst_size();
    if burst > 0 && framerate > 0.0 {
        let frame_interval = Duration::from_secs_f64(1.0 / framerate);
        let buffer = frame_interval * burst as u32;
        info!(
            burst_size = burst,
            framerate,
            buffer_ms = buffer.as_millis(),
            "playout buffer sized from decoder burst"
        );
        clock.set_buffer(buffer);
    }

    let mut playout = PlayoutBuffer::new(clock.clone());

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        // Receive next packet. When the playout buffer has frames waiting,
        // use a timed wait so we can release them on time. When empty,
        // use blocking_recv() for zero-overhead waiting.
        let packet = match playout.next_playout_wait() {
            Some(wait) if !wait.is_zero() => {
                // Buffer has frames not yet ready. Wait for the shorter of
                // the next playout deadline or a new packet arriving.
                match crate::playout::recv_timeout(&mut input_rx, wait) {
                    RecvResult::Value(pkt) => Some(pkt),
                    RecvResult::Timeout => None,
                    RecvResult::Disconnected => {
                        drain_playout_buffer(shutdown, &mut playout, &output_tx);
                        break;
                    }
                }
            }
            Some(_zero) => {
                // Frames are ready right now — don't block, just try to get a packet.
                match input_rx.try_recv() {
                    Ok(pkt) => Some(pkt),
                    Err(mpsc::error::TryRecvError::Empty) => None,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        drain_playout_buffer(shutdown, &mut playout, &output_tx);
                        break;
                    }
                }
            }
            None => {
                // Buffer is empty — block efficiently until a packet arrives.
                match input_rx.blocking_recv() {
                    Some(pkt) => Some(pkt),
                    None => break,
                }
            }
        };

        // Feed packet to decoder if we got one.
        if let Some(packet) = packet {
            // Skip frames when the decoder can't keep up. If wall-clock
            // time has advanced significantly more than PTS, the decoder
            // is falling behind. Skip non-keyframe packets until the next
            // keyframe, which lets us jump forward in the stream. The
            // threshold (500ms) avoids triggering on normal jitter.
            if !waiting_for_keyframe {
                if let (Some(fp), Some(wall_start)) = (first_pts, stream_start_wall) {
                    let wall_elapsed = wall_start.elapsed();
                    let pts_elapsed = latest_pts.saturating_sub(fp);
                    let lag = wall_elapsed.saturating_sub(pts_elapsed);
                    if lag > Duration::from_millis(500) {
                        if packet.is_keyframe {
                            // Keyframe while skipping: reset ALL baselines so
                            // the lag measurement starts fresh from this point.
                            if skip_active {
                                info!(
                                    frames_skipped,
                                    lag_ms = lag.as_millis(),
                                    "decode_loop: skip complete, resuming at keyframe"
                                );
                                skip_active = false;
                                // Reset both wall-clock and PTS baselines together.
                                stream_start_wall = Some(Instant::now());
                                first_pts = Some(packet.timestamp);
                                latest_pts = packet.timestamp;
                                // Reset playout clock for clean restart.
                                playout.clear();
                                playout.reset_clock();
                                // NOTE: audio is not reset here — it runs on an
                                // independent tick loop and doesn't use the
                                // PlayoutClock (see audio_decode_loop). After a
                                // video skip, A/V can be out of sync until the
                                // next keyframe aligns them. A proper fix would
                                // require audio-side skip support.
                            }
                        } else {
                            // Skip non-keyframes to catch up.
                            if !skip_active {
                                info!(
                                    lag_ms = lag.as_millis(),
                                    "decode_loop: decoder behind, skipping to next keyframe"
                                );
                                skip_active = true;
                            }
                            frames_skipped += 1;
                            // Still drain output in case the decoder has frames ready.
                            loop {
                                match decoder.pop_frame() {
                                    Ok(Some(frame)) => {
                                        latest_pts = frame.timestamp;
                                        playout.push(frame);
                                        frames_pushed += 1;
                                    }
                                    Ok(None) => break,
                                    Err(_) => break,
                                }
                            }
                            continue;
                        }
                    } else if skip_active {
                        // Lag dropped below threshold without hitting a keyframe.
                        skip_active = false;
                    }
                }
            }

            if waiting_for_keyframe {
                if !packet.is_keyframe {
                    trace!("skipping non-keyframe packet while waiting for recovery");
                } else {
                    info!("received keyframe, resuming decode");
                    waiting_for_keyframe = false;
                    playout.clear();
                    playout.reset_clock();
                }
            }

            if !waiting_for_keyframe {
                if viewport_watcher.update() {
                    let (w, h) = viewport_watcher.peek();
                    decoder.set_viewport(*w, *h);
                }

                let t = Instant::now();
                if let Err(err) = decoder.push_packet(packet) {
                    warn!("failed to push video packet, waiting for next keyframe: {err:#}");
                    waiting_for_keyframe = true;
                } else {
                    let push_elapsed = t.elapsed();
                    if push_elapsed > Duration::from_millis(10) {
                        debug!(t=?push_elapsed, "decode_loop: slow push_packet");
                    }

                    // Drain ALL decoded frames into the playout buffer.
                    // This frees decoder pool buffers immediately.
                    loop {
                        match decoder.pop_frame() {
                            Ok(Some(frame)) => {
                                if first_pts.is_none() {
                                    first_pts = Some(frame.timestamp);
                                    stream_start_wall = Some(Instant::now());
                                }
                                latest_pts = frame.timestamp;
                                trace!(
                                    pts_ms = frame.timestamp.as_millis(),
                                    buf_len = playout.buf_len(),
                                    "decode_loop: push frame to playout"
                                );
                                playout.push(frame);
                                frames_pushed += 1;
                            }
                            Ok(None) => break,
                            Err(err) => {
                                warn!("pop_frame error, waiting for keyframe: {err:#}");
                                waiting_for_keyframe = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Release frames whose playout time has arrived.
        while let Some(frame) = playout.pop_ready() {
            trace!(
                pts_ms = frame.timestamp.as_millis(),
                buf_len = playout.buf_len(),
                "decode_loop: pop frame from playout"
            );
            if output_tx.blocking_send(frame).is_err() {
                debug!("pipeline: frame receiver dropped");
                return Ok(());
            }
            frames_popped += 1;
            let gap = last_send.elapsed();
            if gap > Duration::from_millis(50) {
                debug!(gap=?gap, buf_len=playout.buf_len(), "decode_loop: frame gap (stutter)");
            }
            last_send = Instant::now();
        }

        // Periodic status log (~every 5s).
        {
            let (drift, reanchors) = clock.reanchor_stats();
            let jitter = clock.jitter();
            let buf = clock.buffer();
            // Wall-clock vs PTS progression: positive means we consumed
            // wall time faster than PTS advanced (falling behind / adding
            // latency). This reveals hidden delay from re-anchoring.
            let wall_vs_pts_lag_ms =
                if let (Some(fp), Some(wall_start)) = (first_pts, stream_start_wall) {
                    let wall_elapsed = wall_start.elapsed();
                    let pts_elapsed = latest_pts.saturating_sub(fp);
                    wall_elapsed.as_millis() as i64 - pts_elapsed.as_millis() as i64
                } else {
                    0
                };
            throttled_tracing::debug_every!(
                Duration::from_secs(5),
                frames_pushed,
                frames_popped,
                frames_skipped,
                buf_len = playout.buf_len(),
                buffer_ms = buf.as_millis(),
                jitter_ms = jitter.as_millis(),
                total_drift_ms = drift.as_millis(),
                reanchor_count = reanchors,
                wall_vs_pts_lag_ms,
                "decode_loop: status"
            );
        }
    }
    Ok(())
}

/// Drains remaining buffered frames before exit.
fn drain_playout_buffer(
    shutdown: &CancellationToken,
    playout: &mut PlayoutBuffer,
    output_tx: &mpsc::Sender<VideoFrame>,
) {
    while let Some(wait) = playout.next_playout_wait() {
        if shutdown.is_cancelled() {
            break;
        }
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        while let Some(frame) = playout.pop_ready() {
            if output_tx.blocking_send(frame).is_err() {
                return;
            }
        }
    }
}

/// Direct-send decode loop for local pipelines without a clock.
///
/// Identical to the pre-clock decode loop: `blocking_recv` → decode →
/// `blocking_send`. No buffering or playout timing.
fn decode_loop_direct(
    shutdown: &CancellationToken,
    input_rx: &mut mpsc::Receiver<MediaPacket>,
    output_tx: &mpsc::Sender<VideoFrame>,
    viewport_watcher: &mut n0_watcher::Direct<(u32, u32)>,
    decoder: &mut impl VideoDecoder,
) -> Result<()> {
    let mut waiting_for_keyframe = false;
    let mut last_send = Instant::now();

    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let recv_t = Instant::now();
        let Some(packet) = input_rx.blocking_recv() else {
            break;
        };
        let recv_elapsed = recv_t.elapsed();
        if recv_elapsed > Duration::from_millis(50) {
            debug!(t=?recv_elapsed, "decode_loop: slow blocking_recv (starved?)");
        }

        if waiting_for_keyframe {
            if !packet.is_keyframe {
                trace!("skipping non-keyframe packet while waiting for recovery");
                continue;
            }
            info!("received keyframe, resuming decode");
            waiting_for_keyframe = false;
        }

        if viewport_watcher.update() {
            let (w, h) = viewport_watcher.peek();
            decoder.set_viewport(*w, *h);
        }

        let t = Instant::now();
        if let Err(err) = decoder.push_packet(packet) {
            warn!("failed to push video packet, waiting for next keyframe: {err:#}");
            waiting_for_keyframe = true;
            continue;
        }
        let push_elapsed = t.elapsed();
        if push_elapsed > Duration::from_millis(10) {
            debug!(t=?push_elapsed, "decode_loop: slow push_packet");
        }

        // Drain all frames and send directly to the output channel.
        loop {
            match decoder.pop_frame() {
                Ok(Some(frame)) => {
                    if output_tx.blocking_send(frame).is_err() {
                        debug!("pipeline: frame receiver dropped");
                        return Ok(());
                    }
                    let gap = last_send.elapsed();
                    if gap > Duration::from_millis(50) {
                        debug!(gap=?gap, "decode_loop: frame gap (stutter)");
                    }
                    last_send = Instant::now();
                }
                Ok(None) => break,
                Err(err) => {
                    warn!("failed to pop video frame, waiting for next keyframe: {err:#}");
                    waiting_for_keyframe = true;
                    break;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Audio Decoder Pipeline
// ---------------------------------------------------------------------------

/// Standalone audio decoder pipeline.
///
/// Reads encoded packets from any [`PacketSource`], decodes on an OS thread,
/// and pushes decoded samples to an [`AudioSink`]. Works without MoQ
/// networking — e.g., with a [`PipeSource`](crate::transport::PipeSource)
/// for local encode→decode pipelines.
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
    /// Creates a new audio decoder pipeline, using an [`AudioStreamFactory`] to
    /// create an output stream with the format required by the decoder.
    ///
    /// Dropping the pipeline cancels the decode thread. The pipeline also
    /// shuts down automatically when the packet source closes.
    pub async fn new<D: AudioDecoder>(
        name: String,
        source: impl PacketSource,
        config: &AudioConfig,
        audio_backend: &dyn AudioStreamFactory,
    ) -> Result<Self> {
        Self::with_clock::<D>(name, source, config, audio_backend, None).await
    }

    /// Creates a new audio decoder pipeline with a shared [`PlayoutClock`]
    /// for A/V sync reporting.
    pub async fn with_clock<D: AudioDecoder>(
        name: String,
        source: impl PacketSource,
        config: &AudioConfig,
        audio_backend: &dyn AudioStreamFactory,
        clock: Option<PlayoutClock>,
    ) -> Result<Self> {
        let target_format = AudioFormat::from_config(config);
        let sink = audio_backend.create_output(target_format).await?;
        let handle = sink.handle();
        Self::build::<D>(name, source, config, sink, handle, clock)
    }

    /// Creates a new audio decoder pipeline with a pre-made [`AudioSink`].
    ///
    /// Returns an error if the sink's format does not match the audio config.
    pub fn with_sink<D: AudioDecoder>(
        name: String,
        source: impl PacketSource,
        config: &AudioConfig,
        sink: impl AudioSink,
    ) -> Result<Self> {
        let output_format = sink.format()?;
        let expected = AudioFormat::from_config(config);
        anyhow::ensure!(
            output_format.sample_rate == expected.sample_rate
                && output_format.channel_count == expected.channel_count,
            "audio sink format mismatch: sink has {output_format:?}, decoder expects {expected:?}"
        );
        let handle = sink.handle();
        Self::build::<D>(name, source, config, sink, handle, None)
    }

    fn build<D: AudioDecoder>(
        name: String,
        source: impl PacketSource,
        config: &AudioConfig,
        sink: impl AudioSink,
        handle: Box<dyn AudioSinkHandle>,
        clock: Option<PlayoutClock>,
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
                if let Err(err) = audio_decode_loop(&shutdown, packet_rx, decoder, sink, clock) {
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

    /// Returns the pipeline name (typically the rendition/track name).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the [`AudioSinkHandle`] for controlling playback (pause/resume/peaks).
    pub fn handle(&self) -> &dyn AudioSinkHandle {
        self.handle.as_ref()
    }

    /// Returns a future that completes when the pipeline shuts down.
    pub fn stopped(&self) -> impl Future<Output = ()> + 'static {
        let shutdown = self.shutdown.clone();
        async move { shutdown.cancelled().await }
    }
}

impl Drop for AudioDecoderPipeline {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Runs the audio decode loop on an OS thread.
///
/// Uses 10ms tick-based polling (`try_recv`) to ensure regular sample delivery
/// regardless of packet arrival timing. This is critical for smooth audio playback.
///
/// When a [`PlayoutClock`] is provided, reports arrival timestamps
/// so audio and video share the same time base for playout.
fn audio_decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<MediaPacket>,
    mut decoder: impl AudioDecoder,
    mut sink: impl AudioSink,
    _clock: Option<PlayoutClock>,
) -> Result<()> {
    use bytes::Buf as _;
    use mpsc::error::TryRecvError;

    const INTERVAL: Duration = Duration::from_millis(10);
    const MAX_CONSECUTIVE_ERRORS: u32 = 10;
    let mut remote_start = None;
    let loop_start = Instant::now();
    let mut consecutive_errors = 0u32;

    'main: for i in 0.. {
        let tick = Instant::now();

        if shutdown.is_cancelled() {
            debug!("stop audio decoder: cancelled");
            break;
        }

        loop {
            match input_rx.try_recv() {
                Ok(packet) => {
                    let remote_start = *remote_start.get_or_insert(packet.timestamp);

                    // Note: we intentionally do NOT call clock.observe_arrival()
                    // here. The clock base must be anchored by the video playout
                    // buffer, not audio. Audio arrives much earlier (near-zero
                    // pipeline delay) and would set base_wall too early, making
                    // all video playout times arrive "in the past" and defeating
                    // the buffer entirely.

                    if tracing::enabled!(tracing::Level::TRACE) {
                        let loop_elapsed = tick.duration_since(loop_start);
                        let remote_elapsed = packet.timestamp.saturating_sub(remote_start);
                        let diff_ms =
                            (loop_elapsed.as_secs_f32() - remote_elapsed.as_secs_f32()) * 1000.;
                        trace!(payload_bytes = packet.payload.remaining(), ts=?packet.timestamp, ?loop_elapsed, ?remote_elapsed, ?diff_ms, "recv packet");
                    }

                    if !sink.is_paused() {
                        if let Err(err) = decoder.push_packet(packet) {
                            consecutive_errors += 1;
                            warn!(consecutive_errors, "failed to push audio packet: {err:#}");
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
                                sink.push_samples(samples)?;
                            }
                            Ok(None) => {
                                consecutive_errors = 0;
                            }
                            Err(err) => {
                                consecutive_errors += 1;
                                warn!(consecutive_errors, "failed to pop audio samples: {err:#}");
                                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
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
                Err(TryRecvError::Empty) => {
                    trace!("no packet to recv");
                    break;
                }
            }
        }

        let sleep = (INTERVAL * i).saturating_sub(loop_start.elapsed());
        if !sleep.is_zero() {
            thread::sleep(sleep);
        }
    }
    shutdown.cancel();
    Ok(())
}

// ---------------------------------------------------------------------------
// Audio Encoder Pipeline
// ---------------------------------------------------------------------------

/// Standalone audio encoder pipeline.
///
/// Captures samples from an [`AudioSource`], encodes them on an OS thread,
/// and sends encoded packets to any [`PacketSink`]. Works without MoQ
/// networking — e.g., paired with an [`AudioDecoderPipeline`] via
/// [`media_pipe`](crate::transport::media_pipe) for local encode→decode loops.
#[derive(derive_more::Debug)]
pub struct AudioEncoderPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl AudioEncoderPipeline {
    /// Creates a new audio encoder pipeline, using an [`AudioStreamFactory`] to
    /// create an input stream with the format required by the encoder.
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
    ///
    /// Returns an error if the source's format does not match the encoder's config.
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
                info!(config = ?encoder.config(), "encode start");
                // 20ms framing to align with typical Opus config (48kHz → 960 samples/ch)
                const INTERVAL: Duration = Duration::from_millis(20);
                let format = source.format();
                let samples_per_frame = (format.sample_rate / 1000) * INTERVAL.as_millis() as u32;
                let mut buf =
                    vec![0.0f32; samples_per_frame as usize * format.channel_count as usize];
                let start = Instant::now();
                let mut sink_closed = false;
                'encode: for tick in 0.. {
                    trace!("tick");
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_samples(&mut buf) {
                        Ok(Some(_n)) => {
                            // Expect a full frame; if shorter, zero-pad via slice len
                            if let Err(err) = encoder.push_samples(&buf) {
                                error!(buf_len = buf.len(), "encoder push_samples failed: {err:#}");
                                break;
                            }
                            loop {
                                match encoder.pop_packet() {
                                    Ok(Some(pkt)) => {
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
                        Ok(None) => {
                            // keep pacing
                        }
                        Err(err) => {
                            error!("audio source failed: {err:#}");
                            break;
                        }
                    }
                    let expected_time = (tick + 1) * INTERVAL;
                    let elapsed = start.elapsed();
                    if elapsed > expected_time {
                        warn!("audio encoder too slow by {:?}", elapsed - expected_time);
                    }
                    let sleep = expected_time.saturating_sub(elapsed);
                    if sleep > Duration::ZERO {
                        thread::sleep(sleep);
                    }
                }
                if !sink_closed {
                    // drain
                    while let Ok(Some(pkt)) = encoder.pop_packet() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::{H264Encoder, H264VideoDecoder, test_util::make_rgba_frame},
        format::VideoPreset,
        traits::{VideoEncoder, VideoEncoderFactory},
        transport::media_pipe,
        util::encoded_frames_to_media_packets,
    };

    fn encode_h264_packets(
        w: u32,
        h: u32,
        n: usize,
        preset: VideoPreset,
    ) -> (VideoConfig, Vec<MediaPacket>) {
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let mut packets = Vec::new();
        for i in 0..n {
            let frame = make_rgba_frame(w, h, (i * 25) as u8, 128, 64);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        let config = enc.config();
        (config, encoded_frames_to_media_packets(packets))
    }

    #[tokio::test]
    async fn video_decoder_pipeline_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let (config, packets) = encode_h264_packets(w, h, 10, VideoPreset::P180);
        assert!(!packets.is_empty());

        let decode_config = DecodeConfig::default();
        let (sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test".into(),
            source,
            &config,
            &decode_config,
        )
        .unwrap();

        // Feed packets from a blocking thread (send_blocking can't be called in async context)
        tokio::task::spawn_blocking(move || {
            for pkt in packets {
                sink.send_blocking(pkt).unwrap();
            }
            // drop sink to signal EOF
        });

        let mut frames = pipeline.frames;
        let mut count = 0;
        while let Some(frame) = frames.rx.recv().await {
            let img = frame.rgba_image();
            assert_eq!(img.width(), w);
            assert_eq!(img.height(), h);
            count += 1;
        }
        assert!(count >= 5, "expected >= 5 decoded frames, got {count}");
    }

    #[tokio::test]
    async fn video_decoder_pipeline_shutdown_on_drop() {
        let (_config, _packets) = encode_h264_packets(320, 180, 1, VideoPreset::P180);
        let config = _config;
        let decode_config = DecodeConfig::default();
        let (_sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test".into(),
            source,
            &config,
            &decode_config,
        )
        .unwrap();

        // Drop pipeline — should not hang or panic
        drop(pipeline);
    }

    #[tokio::test]
    async fn video_decoder_pipeline_viewport() {
        let (config, packets) = encode_h264_packets(640, 360, 5, VideoPreset::P360);

        let decode_config = DecodeConfig::default();
        let (sink, source) = media_pipe(64);

        let pipeline = VideoDecoderPipeline::new::<H264VideoDecoder>(
            "test".into(),
            source,
            &config,
            &decode_config,
        )
        .unwrap();

        pipeline.handle.set_viewport(320, 180);

        tokio::task::spawn_blocking(move || {
            for pkt in packets {
                sink.send_blocking(pkt).unwrap();
            }
        });

        let mut frames = pipeline.frames;
        while let Some(frame) = frames.rx.recv().await {
            let img = frame.rgba_image();
            assert!(img.width() <= 320, "width {} > 320", img.width());
            assert!(img.height() <= 180, "height {} > 180", img.height());
        }
    }

    #[tokio::test]
    async fn video_decoder_pipeline_with_playout_clock() {
        use crate::playout::{PlayoutClock, PlayoutMode};

        let w = 320u32;
        let h = 180u32;
        let (config, packets) = encode_h264_packets(w, h, 10, VideoPreset::P180);
        assert!(!packets.is_empty());

        let decode_config = DecodeConfig::default();
        let (sink, source) = media_pipe(64);
        let clock = PlayoutClock::new(PlayoutMode::Reliable);

        let pipeline = VideoDecoderPipeline::with_clock::<H264VideoDecoder>(
            "test-clock".into(),
            source,
            &config,
            &decode_config,
            Some(clock.clone()),
        )
        .unwrap();

        tokio::task::spawn_blocking(move || {
            for pkt in packets {
                sink.send_blocking(pkt).unwrap();
            }
        });

        let mut frames = pipeline.frames;
        let mut count = 0;
        while let Some(frame) = frames.rx.recv().await {
            let img = frame.rgba_image();
            assert_eq!(img.width(), w);
            assert_eq!(img.height(), h);
            count += 1;
        }
        assert!(count >= 5, "expected >= 5 decoded frames, got {count}");

        // Jitter should be measurable after processing frames.
        let jitter = clock.jitter();
        // In a local pipeline, jitter is near-zero but non-negative.
        assert!(
            jitter < Duration::from_millis(100),
            "jitter unreasonably high: {jitter:?}"
        );
    }
}
