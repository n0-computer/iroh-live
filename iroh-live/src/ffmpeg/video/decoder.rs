use anyhow::{Context, Result};
use ffmpeg_next::{
    self as ffmpeg, codec, codec::Id as CodecId, packet::Packet,
    util::frame::video::Video as FfmpegFrame,
};
use image::Delay;

use crate::{
    av::{self, DecodeConfig, DecodedFrame, VideoDecoder},
    ffmpeg::{
        ext::CodecContextExt,
        util::{ffmpeg_frame_to_image, pixel_to_ffmpeg},
        video::util::{Rescaler, StreamClock},
    },
};

pub struct FfmpegVideoDecoder {
    codec: ffmpeg::decoder::Video,
    rescaler: Rescaler,
    clock: StreamClock,
    decoded: FfmpegFrame,
    viewport_changed: Option<(u32, u32)>,
    last_packet: Option<hang::Frame>,
}

impl VideoDecoder for FfmpegVideoDecoder {
    fn new(config: &hang::catalog::VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        ffmpeg::init()?;

        // Build a decoder context for H.264 and attach extradata (e.g., avcC)
        let codec = match &config.codec {
            hang::catalog::VideoCodec::H264(_meta) => {
                let codec =
                    codec::decoder::find(CodecId::H264).context("H.264 decoder not found")?;
                let mut ctx = codec::context::Context::new_with_codec(codec);
                if let Some(description) = &config.description {
                    ctx.set_extradata(&description)?;
                }
                ctx.decoder().video().unwrap()
            }
            hang::catalog::VideoCodec::AV1(_meta) => {
                let codec = codec::decoder::find(CodecId::AV1).context("AV1 decoder not found")?;
                let mut ctx = codec::context::Context::new_with_codec(codec);
                if let Some(description) = &config.description {
                    ctx.set_extradata(&description)?;
                }
                ctx.decoder().video().unwrap()
            }
            _ => anyhow::bail!(
                "Unsupported codec {} (only h264 and av1 are supported)",
                config.codec
            ),
        };
        let rescaler = Rescaler::new(pixel_to_ffmpeg(playback_config.pixel_format), None)?;
        let clock = StreamClock::default();
        let decoded = FfmpegFrame::empty();
        Ok(Self {
            codec,
            rescaler,
            clock,
            decoded,
            viewport_changed: None,
            last_packet: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, packet: hang::Frame) -> Result<()> {
        {
            let pkt = Packet::borrow(&packet.payload);
            self.codec.send_packet(&pkt)?;
        }
        self.last_packet = Some(packet);
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<av::DecodedFrame>> {
        // Pull all available decoded frames
        match self.codec.receive_frame(&mut self.decoded) {
            Ok(()) => {
                // Apply clamped target size.
                if let Some((max_width, max_height)) = self.viewport_changed.take() {
                    let (width, height) =
                        calculate_resized_size(&self.decoded, max_width, max_height);
                    self.rescaler.target_width_height = Some((width, height));
                }

                let frame = self.rescaler.process(&mut self.decoded)?;
                let image = ffmpeg_frame_to_image(frame);
                // Compute interframe delay from provided timestamps
                let last_packet = self.last_packet.as_ref().context("missing last packet")?;
                let delay = Delay::from_saturating_duration(self.clock.frame_delay(&last_packet));
                let frame = DecodedFrame {
                    frame: image::Frame::from_parts(image, 0, 0, delay),
                    timestamp: last_packet.timestamp,
                };
                Ok(Some(frame))
            }
            Err(ffmpeg::util::error::Error::BufferTooSmall) => Ok(None),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => Ok(None),
            Err(err) => {
                // tracing::warn!("decoder error: {err} {err:?} {err:#?}");
                // Ok(None)
                Err(err.into())
            }
        }
    }
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
    tracing::debug!(
        src_w,
        src_h,
        max_w,
        max_h,
        target_width,
        target_height,
        "scale"
    );
    (target_width, target_height)
}
