use std::{
    thread,
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, warn};

use crate::{
    traits::{PreEncodedVideoSource, VideoEncoder, VideoSource},
    transport::PacketSink,
    util::spawn_thread,
};

/// Standalone video encoder pipeline.
#[derive(derive_more::Debug)]
pub struct VideoEncoderPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl VideoEncoderPipeline {
    /// Creates a new encoder pipeline.
    pub fn new(
        mut source: impl VideoSource,
        mut encoder: impl VideoEncoder,
        mut sink: impl PacketSink,
        opts: crate::stats::EncodeOpts,
    ) -> Self {
        let stats = opts.stats;
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
                info!(
                    capture = %source.name(),
                    encoder = %encoder.name(),
                    src_format = ?format,
                    "encode pipeline: {} -> {}",
                    source.name(),
                    encoder.name(),
                );
                debug!(dst_config = ?enc_config);

                if let Some(ref s) = stats {
                    s.encoder.set(encoder.name());
                    s.capture_path.set(source.name());
                }
                let framerate = enc_config.framerate.unwrap_or_else(|| {
                    warn!("encoder config has no framerate, falling back to 30 fps");
                    30.0
                });
                let interval = Duration::from_secs_f64(1. / framerate);
                let mut sink_closed = false;
                let mut last_frame_time = Instant::now();
                let mut prev_bitrate_bytes: u64 = 0;
                let mut prev_bitrate_time = Instant::now();
                let mut total_bytes: u64 = 0;
                let source_name = source.name().to_string();
                let encoder_name = encoder.name().to_string();
                let mut last_gpu;
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
                    let Some(frame) = frame else {
                        // No frame available yet. Short backoff to avoid spinning
                        // while still picking up frames promptly. A full interval
                        // sleep here would halve the effective FPS.
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    {
                        last_gpu = frame.is_gpu();
                        let encode_start = Instant::now();
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
                                    total_bytes += pkt.payload.len() as u64;
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

                        if let Some(ref s) = stats {
                            s.encode_ms
                                .record(encode_start.elapsed().as_secs_f64() * 1000.0);
                            let gap = start.duration_since(last_frame_time);
                            if !gap.is_zero() {
                                s.fps.record(1.0 / gap.as_secs_f64());
                            }
                            let now_bitrate = Instant::now();
                            let dt = now_bitrate.duration_since(prev_bitrate_time).as_secs_f64();
                            if dt > 0.1 {
                                let delta_bytes = total_bytes - prev_bitrate_bytes;
                                s.bitrate_kbps
                                    .record(delta_bytes as f64 * 8.0 / dt / 1000.0);
                                prev_bitrate_bytes = total_bytes;
                                prev_bitrate_time = now_bitrate;
                            }
                        }
                        last_frame_time = start;

                        if let Some(ref s) = stats {
                            throttled_tracing::debug_every!(
                                Duration::from_secs(5),
                                fps = format_args!("{:.1}", s.fps.current()),
                                encode_ms = format_args!("{:.1}", s.encode_ms.current()),
                                kbps = format_args!("{:.0}", s.bitrate_kbps.current()),
                                gpu = last_gpu,
                                path = format_args!("{}/{}", source_name, encoder_name),
                                "venc stats",
                            );
                        }
                    }
                    // Pace to target framerate after encoding a frame.
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

/// Pipeline for pre-encoded video sources that produce compressed packets directly.
#[derive(derive_more::Debug)]
pub struct PreEncodedVideoPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl PreEncodedVideoPipeline {
    /// Starts the passthrough pipeline.
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
                        Ok(None) => thread::sleep(Duration::from_millis(1)),
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
