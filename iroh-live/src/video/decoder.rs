use std::time::Duration;

use anyhow::{Context, Result};
use ffmpeg_next::{
    self as ffmpeg, codec,
    codec::Id as CodecId,
    packet::Packet,
    util::{format::pixel::Pixel, frame::video::Video as FfmpegFrame},
};
use image::{Delay, RgbaImage};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::{PacketSender, video::Rescaler, video::StreamClock, ffmpeg_ext::CodecContextExt};

pub use crate::av::PixelFormat;

pub type FrameReceiver = mpsc::Receiver<DecodedFrame>;
pub type ResizeSender = mpsc::UnboundedSender<(u32, u32)>;

pub struct DecodedFrame {
    pub frame: image::Frame,
    pub timestamp: Duration,
}

impl DecodedFrame {
    pub fn img(&self) -> &RgbaImage {
        self.frame.buffer()
    }
}

impl From<PixelFormat> for Pixel {
    fn from(value: PixelFormat) -> Self {
        match value {
            PixelFormat::Rgba => Pixel::RGBA,
            PixelFormat::Bgra => Pixel::BGRA,
        }
    }
}

pub struct DecoderContext {
    target_pixel_format: PixelFormat,
    packet_rx: mpsc::Receiver<hang::Frame>,
    frame_tx: mpsc::Sender<DecodedFrame>,
    resize_rx: mpsc::UnboundedReceiver<(u32, u32)>,
    shutdown: CancellationToken,
}

impl DecoderContext {
    pub fn new(
        shutdown: CancellationToken,
        target_pixel_format: PixelFormat,
    ) -> (Self, FrameReceiver, ResizeSender, PacketSender) {
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let (resize_tx, resize_rx) = mpsc::unbounded_channel();
        let ctx = DecoderContext {
            target_pixel_format,
            packet_rx,
            frame_tx,
            resize_rx,
            shutdown,
        };
        (ctx, frame_rx, resize_tx, packet_tx)
    }
}

pub struct Decoder {
    finished: oneshot::Receiver<(DecoderContext, anyhow::Result<()>)>,
}

impl Decoder {
    pub fn new(config: &hang::catalog::VideoConfig, mut ctx: DecoderContext) -> Result<Self> {
        ffmpeg::init()?;

        // let (packet_tx, packet_rx) = mpsc::channel(32);
        // let (frame_tx, frame_rx) = mpsc::channel(32);
        // let (resize_tx, resize_rx) = mpsc::unbounded_channel();

        // Build a decoder context for H.264 and attach extradata (e.g., avcC)
        let mut codec = match &config.codec {
            hang::catalog::VideoCodec::H264(_meta) => {
                let codec =
                    codec::decoder::find(CodecId::H264).context("H.264 decoder not found")?;
                let mut ctx = codec::context::Context::new_with_codec(codec);

                // Attach extradata if present (via your ext trait)
                if let Some(description) = &config.description {
                    ctx.set_extradata(&description)?;
                }

                // Open the decoder and get a typed Video decoder
                ctx.decoder().video().unwrap()
            }
            hang::catalog::VideoCodec::AV1(_meta) => {
                let codec = codec::decoder::find(CodecId::AV1).context("AV1 decoder not found")?;
                let mut ctx = codec::context::Context::new_with_codec(codec);
                if let Some(description) = &config.description { ctx.set_extradata(&description)?; }
                ctx.decoder().video().unwrap()
            }
            _ => anyhow::bail!(
                "Unsupported codec {} (only h264 and av1 are supported)",
                config.codec
            ),
        };

        let (finished_tx, finished) = oneshot::channel();
        std::thread::spawn(move || {
            let res = decode_loop(&mut codec, &mut ctx);
            let res = res.inspect_err(|err| error!("Decoder failed: {err:#}"));
            finished_tx.send((ctx, res)).ok();
        });

        let this = Decoder { finished };

        Ok(this)
    }

    pub async fn closed(self) -> Result<DecoderContext> {
        let (ctx, _res) = self.finished.await?;
        Ok(ctx)
    }
}

fn decode_loop(codec: &mut ffmpeg::decoder::Video, ctx: &mut DecoderContext) -> Result<()> {
    let DecoderContext {
        packet_rx,
        frame_tx,
        resize_rx,
        shutdown,
        target_pixel_format,
    } = ctx;
    let mut rescaler = Rescaler::new(Pixel::from(*target_pixel_format), None)?;
    let mut clock = StreamClock::default();
    let mut decoded = FfmpegFrame::empty();
    let mut requested_target: Option<(u32, u32)> = None;

    let mut i = 0;
    while let Some(encoded_frame) = packet_rx.blocking_recv() {
        if shutdown.is_cancelled() {
            break;
        }

        // Apply any pending resize requests (drain queue keeping last)
        while let Ok((w, h)) = resize_rx.try_recv() {
            if w > 0 && h > 0 {
                requested_target = Some((w, h));
            }
        }
        // Wrap incoming payload as an FFmpeg packet and send to decoder
        let pkt = Packet::borrow(&encoded_frame.payload);
        if let Err(err) = codec.send_packet(&pkt) {
            error!("Failed to import packet: {err}");
            continue;
        }

        // Pull all available decoded frames
        while codec.receive_frame(&mut decoded).is_ok() {
            i += 1;
            // Apply clamped target size and convert to BGRA
            if let Some((max_width, max_height)) = requested_target {
                let (width, height) = calculate_resized_size(&decoded, max_width, max_height);
                rescaler.target_width_height = Some((width, height));
            } else {
                rescaler.target_width_height = None;
            }
            // Convert to BGRA
            // Note: This is specific to GPUI: It takes an image::RgbaImage but actually
            // expects it to be in BGRA format.
            // (Yes, I did find out about this because everything looked wrong initially).
            let frame = rescaler.process(&decoded)?;
            if i % 20 == 0 {
                tracing::trace!(
                    src_w = decoded.width(),
                    src_h = decoded.height(),
                    w = frame.width(),
                    h = frame.height(),
                    "decoded frame"
                );
            }

            // Allocate into a vec.
            let image = into_image_frame(frame);

            // Compute interframe delay from provided timestamps
            let delay = Delay::from_saturating_duration(clock.frame_delay(&encoded_frame));
            let frame = DecodedFrame {
                frame: image::Frame::from_parts(image, 0, 0, delay),
                timestamp: encoded_frame.timestamp,
            };

            // Send out.
            frame_tx
                .blocking_send(frame)
                .context("Receiver for decoded frames dropped")?;
        }
    }

    // Flush decoder
    if let Err(err) = codec.send_eof() {
        error!("Failed to send EOF: {err}");
    }

    Ok(())
}

/// Calculates the target frame size to fit into the requested bounds while preserving aspect ratio.
fn calculate_resized_size(decoded: &FfmpegFrame, max_width: u32, max_height: u32) -> (u32, u32) {
    let src_w = decoded.width().max(1);
    let src_h = decoded.height().max(1);
    let max_w = max_width.max(1);
    let max_h = max_height.max(1);

    // Fit within requested bounds, preserve aspect ratio, never upscale
    let scale_w = (max_w as f32) / (src_w as f32);
    let scale_h = (max_h as f32) / (src_h as f32);
    let scale = scale_w.min(scale_h).min(1.0).max(0.0);
    let target_width = ((src_w as f32) * scale).floor().max(1.0) as u32;
    let target_height = ((src_h as f32) * scale).floor().max(1.0) as u32;
    (target_width, target_height)
}

/// Convert the ffmpeg frame into an [image] frame.
///
/// Note: This does not do any color conversion. Make sure the frame is in the correct color format before.
///
/// This allocates the full frame into a vec, which we need anyway to cross the thread boundary.
fn into_image_frame(frame: &ffmpeg_next::util::frame::Video) -> image::RgbaImage {
    let width = frame.width();
    let height = frame.height();
    let bytes_per_pixel = 4usize; // BGRA
    let src = frame.data(0);
    // ffmpeg frames may have padding at end of each line; copy row-by-row.
    let stride = frame.stride(0) as usize;
    let row_bytes = (width as usize) * bytes_per_pixel;
    let mut out = vec![0u8; row_bytes * (height as usize)];
    for y in 0..(height as usize) {
        let src_off = y * stride;
        let dst_off = y * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    RgbaImage::from_raw(width, height, out).expect("valid image buffer")
}
