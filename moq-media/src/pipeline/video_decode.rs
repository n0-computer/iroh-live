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

use super::{DecodeOpts, forward_packets};
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
        opts: DecodeOpts,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = crate::frame_channel::frame_channel();
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
                info!("decode start");
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
                info!("decode stop");
                shutdown.cancel();
            }
        });

        let task = tokio::spawn(forward_packets(source, packet_tx));
        let guard = PipelineGuard {
            _shutdown_token_guard: shutdown.drop_guard(),
            _task_handle: AbortOnDropHandle::new(task),
            _thread_handle: thread,
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

/// The core video decode loop, running on an OS thread.
fn decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<crate::format::MediaPacket>,
    output_tx: crate::frame_channel::FrameSender<VideoFrame>,
    mut viewport_watcher: n0_watcher::Direct<(u32, u32)>,
    mut decoder: impl VideoDecoder,
    framerate: f64,
    opts: DecodeOpts,
) -> Result<()> {
    let stats = &opts.stats;
    let mut buffer = VecDeque::new();
    let mut waiting_for_keyframe = false;
    let mut pacer = FramePacer::new(framerate);
    let mut lag = LagTracker::new();
    let mut last_render_wall: Option<Instant> = None;

    let burst_size = decoder.burst_size();

    'main: loop {
        if shutdown.is_cancelled() {
            break;
        }

        if viewport_watcher.update() {
            let (w, h) = viewport_watcher.peek();
            decoder.set_viewport(*w, *h);
        }

        // Drain all available packets into the decoder, collect frames.
        let drain_start = Instant::now();
        'drain: loop {
            if !buffer.is_empty() && drain_start.elapsed() > pacer.frame_time {
                break;
            }
            let packet = match input_rx.try_recv() {
                Ok(packet) => packet,
                Err(TryRecvError::Empty) => {
                    if buffer.len() <= burst_size {
                        match input_rx.blocking_recv() {
                            Some(packet) => packet,
                            None => break 'main,
                        }
                    } else {
                        break 'drain;
                    }
                }
                Err(TryRecvError::Disconnected) => break 'main,
            };

            if waiting_for_keyframe {
                if !packet.is_keyframe {
                    trace!("skipping non-keyframe packet while waiting for recovery");
                    continue;
                }
                info!("received keyframe, resuming decode");
                waiting_for_keyframe = false;
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
            let sleep = pacer.pace(frame_pts);
            if !sleep.is_zero() {
                thread::sleep(sleep);
            }
            pacer.note_render(frame_pts);

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
    }
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
