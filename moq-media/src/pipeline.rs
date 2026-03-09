use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use hang::catalog::VideoConfig;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use crate::format::{DecodeConfig, DecodedVideoFrame, FrameBuffer, MediaPacket, PixelFormat};
use crate::processing::scale::{Scaler, fit_within};
use crate::traits::{VideoDecoder, VideoEncoder, VideoSource};
use crate::transport::{PacketSource, PipeSink};
use crate::util::spawn_thread;

/// Forward packets from any async `PacketSource` into a sync mpsc channel.
async fn forward_packets(mut source: impl PacketSource, sender: mpsc::Sender<MediaPacket>) {
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

/// A standalone video decoder pipeline.
///
/// Reads encoded packets from any [`PacketSource`], decodes on an OS thread,
/// and outputs [`DecodedVideoFrame`]s via an mpsc channel.
///
/// This can be used without MoQ networking — e.g., with a [`PipeSource`](crate::transport::PipeSource)
/// for local encode→decode pipelines.
#[derive(Debug)]
pub struct VideoDecoderPipeline {
    pub frames: VideoDecoderFrames,
    pub handle: VideoDecoderHandle,
}

/// Receiving end of decoded frames from a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderFrames {
    rx: mpsc::Receiver<DecodedVideoFrame>,
}

impl VideoDecoderFrames {
    /// Get the most recent decoded frame, draining any older buffered frames.
    pub fn current_frame(&mut self) -> Option<DecodedVideoFrame> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        latest
    }

    /// Blocking receive of the next decoded frame.
    pub fn recv_blocking(&mut self) -> Option<DecodedVideoFrame> {
        self.rx.blocking_recv()
    }

    /// Consume this and return the underlying receiver.
    pub fn into_rx(self) -> mpsc::Receiver<DecodedVideoFrame> {
        self.rx
    }
}

/// Control handle for a [`VideoDecoderPipeline`].
#[derive(Debug)]
pub struct VideoDecoderHandle {
    pub rendition: String,
    pub decoder_name: String,
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

/// Holds resources that keep the pipeline alive; dropping cancels everything.
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
    /// Create a new decoder pipeline from any packet source.
    pub fn new<D: VideoDecoder>(
        name: String,
        source: impl PacketSource,
        config: &VideoConfig,
        decode_config: &DecodeConfig,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let viewport = n0_watcher::Watchable::new((1u32, 1u32));
        let viewport_watcher = viewport.watch();

        let decoder = D::new(config, decode_config)?;
        let decoder_name = decoder.name().to_string();
        let target_pixel_format = decode_config.pixel_format;

        let thread_name = format!("vdec-{name}");
        let decoder_name_for_handle = decoder_name.clone();
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                info!(decoder = %decoder_name, "pipeline decoder thread start");
                if let Err(err) = decode_loop(
                    &shutdown,
                    packet_rx,
                    frame_tx,
                    viewport_watcher,
                    decoder,
                    target_pixel_format,
                ) {
                    error!("pipeline decoder failed: {err:#}");
                }
                info!("pipeline decoder thread stop");
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

/// A standalone video encoder pipeline.
///
/// Captures frames from a [`VideoSource`], encodes them on an OS thread,
/// and sends [`MediaPacket`]s to a [`PipeSink`].
///
/// This can be used without MoQ networking — e.g., paired with a
/// [`VideoDecoderPipeline`] via [`media_pipe`](crate::transport::media_pipe)
/// for local encode→decode loops.
#[derive(derive_more::Debug)]
pub struct VideoEncoderPipeline {
    shutdown: CancellationToken,
    #[debug(skip)]
    _thread_handle: thread::JoinHandle<()>,
}

impl VideoEncoderPipeline {
    /// Create a new encoder pipeline.
    ///
    /// Spawns an OS thread that captures frames from `source`, encodes them
    /// with `encoder`, and writes the resulting packets to `sink`.
    pub fn new(
        mut source: impl VideoSource,
        mut encoder: impl VideoEncoder,
        sink: PipeSink,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let enc_config = encoder.config();
        let format = source.format();

        let scaler_dims = match (enc_config.coded_width, enc_config.coded_height) {
            (Some(w), Some(h)) => {
                Some(fit_within(format.dimensions[0], format.dimensions[1], w, h))
            }
            _ => None,
        };
        let framerate = enc_config.framerate.unwrap_or(30.0);
        let interval = Duration::from_secs_f64(1.0 / framerate);

        let thread_name = format!("venc-{}", source.name());
        let thread = spawn_thread(thread_name, {
            let shutdown = shutdown.clone();
            move || {
                if let Err(err) = source.start() {
                    error!("encoder pipeline: video source failed to start: {err:#}");
                    return;
                }
                info!(encoder = encoder.name(), "encoder pipeline thread start");
                let scaler = Scaler::new(scaler_dims);
                let mut first = true;
                loop {
                    let tick = Instant::now();
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match source.pop_frame() {
                        Ok(Some(frame)) => {
                            let frame = match scaler.scale_rgba(
                                &frame.raw,
                                frame.format.dimensions[0],
                                frame.format.dimensions[1],
                            ) {
                                Ok(Some((data, w, h))) => crate::format::VideoFrame {
                                    format: crate::format::VideoFormat {
                                        pixel_format: frame.format.pixel_format,
                                        dimensions: [w, h],
                                    },
                                    raw: data.into(),
                                },
                                Ok(None) => frame,
                                Err(err) => {
                                    error!("encoder pipeline: scaling failed: {err:#}");
                                    break;
                                }
                            };
                            if let Err(err) = encoder.push_frame(frame) {
                                error!("encoder pipeline: push_frame failed: {err:#}");
                                break;
                            }
                            loop {
                                match encoder.pop_packet() {
                                    Ok(Some(pkt)) => {
                                        if first && !pkt.is_keyframe {
                                            continue;
                                        }
                                        first = false;
                                        let media_pkt = MediaPacket {
                                            timestamp: pkt.timestamp,
                                            payload: pkt.payload.into(),
                                            is_keyframe: pkt.is_keyframe,
                                        };
                                        if let Err(err) = sink.send_blocking(media_pkt) {
                                            debug!("encoder pipeline: sink closed: {err:#}");
                                            shutdown.cancel();
                                            break;
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(err) => {
                                        error!("encoder pipeline: pop_packet failed: {err:#}");
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            error!("encoder pipeline: source failed: {err:#}");
                            break;
                        }
                    }
                    thread::sleep(interval.saturating_sub(tick.elapsed()));
                }
                if let Err(err) = source.stop() {
                    warn!("encoder pipeline: source failed to stop: {err:#}");
                }
                info!("encoder pipeline thread stop");
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
// Video Decoder Pipeline – decode loop
// ---------------------------------------------------------------------------

/// The core decode loop, running on an OS thread.
///
/// Reads `MediaPacket`s from the channel, feeds them to the decoder,
/// and sends decoded frames to the output channel.
fn decode_loop(
    shutdown: &CancellationToken,
    mut input_rx: mpsc::Receiver<MediaPacket>,
    output_tx: mpsc::Sender<DecodedVideoFrame>,
    mut viewport_watcher: n0_watcher::Direct<(u32, u32)>,
    mut decoder: impl VideoDecoder,
    target_pixel_format: PixelFormat,
) -> Result<()> {
    let mut waiting_for_keyframe = false;

    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let Some(packet) = input_rx.blocking_recv() else {
            break;
        };

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
        trace!(t=?t.elapsed(), "pipeline: push_packet");

        match decoder.pop_frame() {
            Ok(Some(mut frame)) => {
                trace!(t=?t.elapsed(), "pipeline: pop frame");
                if target_pixel_format == PixelFormat::Bgra
                    && let FrameBuffer::Cpu(ref mut cpu) = frame.buffer
                {
                    cpu.pixel_format = PixelFormat::Bgra;
                    for pixel in cpu.image.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }
                }
                if output_tx.blocking_send(frame).is_err() {
                    debug!("pipeline: frame receiver dropped");
                    break;
                }
            }
            Ok(None) => {}
            Err(err) => {
                warn!("failed to pop video frame, waiting for next keyframe: {err:#}");
                waiting_for_keyframe = true;
            }
        }
    }
    Ok(())
}
