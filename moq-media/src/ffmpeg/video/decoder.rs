use std::time::Duration;

use anyhow::{Context, Result};
use ffmpeg_next::{
    self as ffmpeg, codec, codec::Id as CodecId, error::EAGAIN,
    util::frame::video::Video as FfmpegFrame,
};
use hang::catalog::{VideoCodec, VideoConfig};

use crate::{
    av::{self, DecodeConfig, DecodedFrame, VideoDecoder},
    ffmpeg::{
        ext::{CodecContextExt, PacketExt},
        video::util::{Rescaler, StreamClock},
    },
};

#[derive(derive_more::Debug)]
pub struct FfmpegVideoDecoder {
    #[debug(skip)]
    codec: codec::decoder::Video,
    #[debug(skip)]
    rescaler: Rescaler,
    clock: StreamClock,
    #[debug(skip)]
    decoded: FfmpegFrame,
    viewport_changed: Option<(u32, u32)>,
    last_timestamp: Option<hang::Timestamp>,
}

impl VideoDecoder for FfmpegVideoDecoder {
    fn name(&self) -> &str {
        self.codec.id().name()
    }

    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        ffmpeg::init()?;

        // Build a decoder context for H.264 and attach extradata (e.g., avcC)
        let codec = match &config.codec {
            VideoCodec::H264(_meta) => {
                let codec =
                    codec::decoder::find(CodecId::H264).context("H.264 decoder not found")?;
                let mut ctx = codec::context::Context::new_with_codec(codec);
                if let Some(description) = &config.description {
                    ctx.set_extradata(description)?;
                }
                ctx.decoder().video().unwrap()
            }
            VideoCodec::AV1(_meta) => {
                let codec = codec::decoder::find(CodecId::AV1).context("AV1 decoder not found")?;
                let mut ctx = codec::context::Context::new_with_codec(codec);
                if let Some(description) = &config.description {
                    ctx.set_extradata(description)?;
                }
                ctx.decoder().video().unwrap()
            }
            _ => anyhow::bail!(
                "Unsupported codec {} (only h264 and av1 are supported)",
                config.codec
            ),
        };
        let rescaler = Rescaler::new(playback_config.pixel_format.to_ffmpeg(), None)?;
        let clock = StreamClock::default();
        let decoded = FfmpegFrame::empty();
        Ok(Self {
            codec,
            rescaler,
            clock,
            decoded,
            viewport_changed: None,
            last_timestamp: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, packet: hang::Frame) -> Result<()> {
        let ffmpeg_packet = packet.payload.to_ffmpeg_packet();
        self.codec.send_packet(&ffmpeg_packet)?;
        self.last_timestamp = Some(packet.timestamp);
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
                    self.rescaler.set_target_dimensions(width, height);
                }

                let frame = self.rescaler.process(&self.decoded)?;
                let last_timestamp = self
                    .last_timestamp
                    .as_ref()
                    .context("missing last packet")?;
                let frame = DecodedFrame::from_ffmpeg(
                    frame,
                    self.clock.frame_delay(last_timestamp),
                    Duration::from(*last_timestamp),
                );
                Ok(Some(frame))
            }
            Err(ffmpeg::Error::BufferTooSmall) => Ok(None),
            Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => Ok(None),
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
    let scale = scale_w.min(scale_h).clamp(0.0, 1.0);
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
