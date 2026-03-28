use std::{
    collections::VecDeque,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use tokio::sync::mpsc::{self, error::TryRecvError};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, info_span, trace, warn};

use super::{PipelineContext, forward_packets};
use crate::{
    format::{DecodeConfig, VideoFrame},
    stats::{FrameKind, FrameMeta, LagTracker},
    traits::VideoDecoder,
    util::spawn_thread,
};

/// Standalone video decoder pipeline.
#[derive(Debug)]
pub struct VideoDecoderPipeline {
    pub frames: VideoDecoderFrames,
    pub handle: VideoDecoderHandle,
}

/// Receiving end of decoded frames from a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderFrames {
    pub(crate) rx: crate::frame_channel::FrameReceiver<VideoFrame>,
}

/// Control handle for a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderHandle {
    rendition: String,
    decoder_name: String,
    pub(crate) viewport: n0_watcher::Watchable<(u32, u32)>,
    _guard: PipelineGuard,
}

/// Keeps decoder pipeline resources alive; dropping cancels everything.
#[derive(derive_more::Debug)]
struct PipelineGuard {
    #[debug(skip)]
    _shutdown_token_guard: tokio_util::sync::DropGuard,
    #[debug(skip)]
    _task_handle: AbortOnDropHandle<()>,
    #[debug(skip)]
    _thread_handle: std::thread::JoinHandle<()>,
}

impl VideoDecoderFrames {
    pub fn current_frame(&mut self) -> Option<VideoFrame> {
        self.rx.take()
    }

    pub async fn recv(&self) -> Option<VideoFrame> {
        self.rx.recv().await
    }

    pub fn is_closed(&self) -> bool {
        self.rx.is_closed()
    }

    pub fn produced(&self) -> u64 {
        self.rx.produced()
    }
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

impl VideoDecoderPipeline {
    pub fn new<D: VideoDecoder>(
        name: String,
        source: impl crate::transport::PacketSource,
        config: &rusty_codecs::config::VideoConfig,
        decode_config: &DecodeConfig,
        opts: PipelineContext,
    ) -> Result<Self> {
        let (frame_tx, frame_rx) = crate::frame_channel::frame_channel();
        let handle = Self::build::<D>(name, source, config, decode_config, opts, frame_tx)?;
        Ok(Self {
            frames: VideoDecoderFrames { rx: frame_rx },
            handle,
        })
    }

    /// Creates a pipeline that writes decoded frames to an externally
    /// provided [`FrameSender`].
    ///
    /// Used by the adaptation layer to wire a new decoder pipeline into
    /// the same frame channel the consumer already holds. Only the
    /// [`VideoDecoderHandle`] is returned because the receiver stays with
    /// the consumer.
    pub fn with_sender<D: VideoDecoder>(
        name: String,
        source: impl crate::transport::PacketSource,
        config: &rusty_codecs::config::VideoConfig,
        decode_config: &DecodeConfig,
        opts: PipelineContext,
        frame_tx: crate::frame_channel::FrameSender<VideoFrame>,
    ) -> Result<VideoDecoderHandle> {
        Self::build::<D>(name, source, config, decode_config, opts, frame_tx)
    }

    fn build<D: VideoDecoder>(
        name: String,
        source: impl crate::transport::PacketSource,
        config: &rusty_codecs::config::VideoConfig,
        decode_config: &DecodeConfig,
        opts: PipelineContext,
        frame_tx: crate::frame_channel::FrameSender<VideoFrame>,
    ) -> Result<VideoDecoderHandle> {
        let shutdown = CancellationToken::new();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let viewport = n0_watcher::Watchable::new((1u32, 1u32));
        let viewport_watcher = viewport.watch();

        let decoder = D::new(config, decode_config)?;
        let decoder_name = decoder.name().to_string();
        let span = info_span!("videodec", %name, decoder = %decoder_name);

        let thread_name = format!("vdec-{name}");
        let decoder_name_for_handle = decoder_name;
        let framerate = config.framerate.unwrap_or_else(|| {
            warn!("catalog has no framerate, falling back to 30 fps");
            30.0
        });
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                let _guard = span.enter();
                info!("decode pipeline start");
                if let Err(err) = decode_loop(
                    &shutdown,
                    packet_rx,
                    frame_tx,
                    viewport_watcher,
                    decoder,
                    framerate,
                    opts,
                ) {
                    error!("decoder failed: {err:#}");
                }
                info!("decode pipeline stop");
                shutdown.cancel();
            }
        });

        let task = tokio::spawn(forward_packets(source, packet_tx));
        let guard = PipelineGuard {
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
        };

        Ok(VideoDecoderHandle {
            rendition: name,
            decoder_name: decoder_name_for_handle,
            viewport,
            _guard: guard,
        })
    }
}

/// The core video decode loop, running on an OS thread.
///
/// ## Sync integration
///
/// Ported from `moq/js` commit `53fe78d8`, file
/// `js/watch/src/video/decoder.ts`.
///
/// When `opts.sync` is `Some`:
///
/// 1. **Receive path** — `sync.received(packet.timestamp)` is called for
///    every packet popped from `input_rx`, recording the arrival time
///    so the shared clock can compute the tightest reference offset.
///
/// 2. **Render path** — `sync.wait(frame.timestamp)` gates each decoded
///    frame before it is sent to `output_tx`, sleeping until the
///    playout time (reference + pts + latency) arrives. This replaces
///    the `FramePacer`.
///
/// When `opts.sync` is `None`, the legacy [`FramePacer`] is used (no
/// shared clock, PTS-cadence sleep only).
///
/// ### Differences from the JS pipeline
///
/// - JS WebCodecs decode is async, so multiple `wait()` calls run
///   concurrently. Here decode is synchronous on one OS thread, so
///   `wait()` calls are sequential. Same algorithm, different
///   concurrency model.
///
/// - JS immediately displays the first decoded frame before `wait()`
///   returns ("preview while buffering"). We skip this because our
///   frame channel already exposes the latest frame, and the wait is
///   only the jitter buffer duration.
fn decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<crate::format::MediaPacket>,
    output_tx: crate::frame_channel::FrameSender<VideoFrame>,
    mut viewport_watcher: n0_watcher::Direct<(u32, u32)>,
    mut decoder: impl VideoDecoder,
    framerate: f64,
    opts: PipelineContext,
) -> Result<()> {
    let stats = &opts.stats;
    let sync = opts.sync.as_ref();
    let mut buffer = VecDeque::new();
    let mut waiting_for_keyframe = false;
    // FramePacer is only used when sync is None (legacy mode).
    let mut pacer = FramePacer::new(framerate);
    let mut lag = LagTracker::new();
    let mut last_render_wall: Option<Instant> = None;

    let burst_size = decoder.burst_size();
    let decoder_name = decoder.name().to_string();

    // When sync is present, use a shorter drain timeout. The sync clock
    // handles pacing, so we just need to avoid starving the decode buffer.
    let drain_timeout = if sync.is_some() {
        Duration::from_millis(2)
    } else {
        pacer.frame_time
    };

    'main: loop {
        if shutdown.is_cancelled() {
            info!("decode loop: shutdown cancelled");
            break;
        }

        if viewport_watcher.update() {
            let (w, h) = viewport_watcher.peek();
            decoder.set_viewport(*w, *h);
        }

        // Drain all available packets into the decoder, collect frames.
        let drain_start = Instant::now();
        'drain: loop {
            if !buffer.is_empty() && drain_start.elapsed() > drain_timeout {
                break;
            }
            let packet = match input_rx.try_recv() {
                Ok(packet) => packet,
                Err(TryRecvError::Empty) => {
                    if buffer.len() <= burst_size {
                        let wait_start = Instant::now();
                        match input_rx.blocking_recv() {
                            Some(packet) => {
                                let wait = wait_start.elapsed();
                                if wait > Duration::from_secs(1) {
                                    tracing::debug!(
                                        wait_ms = wait.as_millis(),
                                        waiting_for_keyframe,
                                        "decode input stall"
                                    );
                                }
                                packet
                            }
                            None => {
                                info!("decode loop: input channel closed");
                                break 'main;
                            }
                        }
                    } else {
                        break 'drain;
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    info!("decode loop: input channel disconnected");
                    break 'main;
                }
            };

            if waiting_for_keyframe {
                if !packet.is_keyframe {
                    trace!("skipping non-keyframe packet while waiting for recovery");
                    continue;
                }
                info!("received keyframe, resuming decode");
                waiting_for_keyframe = false;
            }

            // Record the packet arrival for the shared playout clock.
            // The ordered consumer already applied group-level latency
            // skipping, so this is the first point we see the packet.
            if let Some(sync) = sync {
                sync.received(packet.timestamp);
            }

            let received = Instant::now();
            let meta = FrameMeta {
                kind: FrameKind::Video,
                pts: packet.timestamp,
                is_keyframe: packet.is_keyframe,
                received,
                decoded: None,
                rendered: received,
            };

            let decode_start = Instant::now();
            decoder.push_packet(packet)?;

            'pop: loop {
                match decoder.pop_frame() {
                    Ok(Some(frame)) => {
                        let decoded = Instant::now();
                        stats.render.decode_ms.record_ms(decoded - decode_start);
                        let mut meta = meta.clone();
                        meta.decoded = Some(decoded);
                        meta.pts = frame.timestamp;
                        buffer.push_back((frame, meta));
                    }
                    Ok(None) => break 'pop,
                    Err(err) => {
                        warn!("decode error: {err:#}");
                        decoder.reset()?;
                        waiting_for_keyframe = true;
                        break 'pop;
                    }
                }
            }
        }

        // Record buffer depth.
        stats.timing.video_buf.record(buffer.len() as f64);

        // Pace and render.
        while let Some((frame, mut meta)) = buffer.pop_front() {
            let frame_pts = frame.timestamp;

            if let Some(sync) = sync {
                // Block until the playout clock says it is time to
                // render this frame. Returns false if the Sync was
                // closed (pipeline teardown).
                if !sync.wait(frame_pts) {
                    break 'main;
                }
            } else {
                let sleep = pacer.pace(frame_pts);
                if !sleep.is_zero() {
                    thread::sleep(sleep);
                }
                pacer.note_render(frame_pts);
            }

            let now = Instant::now();
            meta.rendered = now;
            stats.timeline.push(meta);
            output_tx.send(frame);

            if let Some(prev) = last_render_wall {
                stats.render.fps.record_fps_gap(now - prev);
            }
            last_render_wall = Some(now);

            let vid_lag_ms = lag.record_ms(now, frame_pts);
            stats.timing.video_lag_ms.record(vid_lag_ms);

            // A/V delta: difference between video and audio lag.
            // Positive = video is behind audio.
            if stats.timing.audio_lag_ms.has_samples() {
                let aud_lag_ms = stats.timing.audio_lag_ms.current();
                stats.timing.av_delta_ms.record(vid_lag_ms - aud_lag_ms);
            }
        }

        {
            let t = &stats.timing;
            let av = if t.av_delta_ms.has_samples() {
                format!("{:.1}", t.av_delta_ms.current())
            } else {
                "-".into()
            };
            throttled_tracing::debug_every!(Duration::from_secs(5),
                fps = format_args!("{:.1}", stats.render.fps.current()),
                decode_ms = format_args!("{:.1}", stats.render.decode_ms.current()),
                lag_ms = format_args!("{:.1}", t.video_lag_ms.current()),
                av_delta = %av,
                buf = buffer.len(),
                decoder = %decoder_name,
                "vdec stats",
            );
        }
    }

    // Do NOT close the shared Sync here. The Sync belongs to the
    // RemoteBroadcast and is shared across rendition switches. If the
    // old pipeline closes it, the replacement pipeline's sync.wait()
    // returns false immediately, killing the new decode loop.

    Ok(())
}

/// PTS-cadence frame pacer. Sleeps between frames based on the PTS delta
/// from the previous frame, clamped to avoid long stalls after network gaps.
#[derive(Debug)]
struct FramePacer {
    frame_time: Duration,
    last_render_wall: Option<Instant>,
    last_render_pts: Option<Duration>,
    frame_count: u64,
}

impl FramePacer {
    fn new(fps: f64) -> Self {
        Self {
            frame_time: Duration::from_secs_f64(1.0 / fps.max(1.0)),
            last_render_wall: None,
            last_render_pts: None,
            frame_count: 0,
        }
    }

    fn pace(&mut self, pts: Duration) -> Duration {
        self.frame_count += 1;
        let (Some(prev_wall), Some(prev_pts)) = (self.last_render_wall, self.last_render_pts)
        else {
            return Duration::ZERO;
        };
        let pts_delta = pts.saturating_sub(prev_pts);
        let target_sleep = pts_delta.min(self.frame_time * 2);
        target_sleep.saturating_sub(prev_wall.elapsed())
    }

    fn note_render(&mut self, pts: Duration) {
        self.last_render_wall = Some(Instant::now());
        self.last_render_pts = Some(pts);
    }
}
