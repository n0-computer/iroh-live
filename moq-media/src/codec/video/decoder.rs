use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Buf;
use hang::catalog::{VideoCodec, VideoConfig};
use image::{Delay, Frame, RgbaImage};
use openh264::{decoder::Decoder, formats::YUVSource};

use crate::av::{self, DecodeConfig, DecodedFrame, VideoDecoder};

use super::util::{
    StreamClock,
    annexb::{avcc_to_annex_b, length_prefixed_to_annex_b},
    scale::{Scaler, fit_within},
};

#[derive(derive_more::Debug)]
pub(crate) struct H264VideoDecoder {
    #[debug(skip)]
    decoder: Decoder,
    scaler: Scaler,
    clock: StreamClock,
    viewport_changed: Option<(u32, u32)>,
    last_timestamp: Option<hang::Timestamp>,
    /// Decoded frame waiting to be collected via `pop_frame`.
    #[debug(skip)]
    pending_frame: Option<(RgbaImage, u32, u32)>,
}

impl VideoDecoder for H264VideoDecoder {
    fn name(&self) -> &str {
        "h264-openh264"
    }

    fn new(config: &VideoConfig, _playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        if !matches!(&config.codec, VideoCodec::H264(_)) {
            bail!(
                "Unsupported codec {} (only H.264 is supported, AV1 planned for phase 2)",
                config.codec
            );
        }

        let mut decoder = Decoder::new().context("failed to create openh264 decoder")?;

        // If we have avcC extradata, feed the SPS/PPS to the decoder upfront.
        if let Some(description) = &config.description
            && let Some(annex_b) = avcc_to_annex_b(description)
        {
            let _ = decoder.decode(&annex_b);
        }

        Ok(Self {
            decoder,
            scaler: Scaler::new(None),
            clock: StreamClock::default(),
            viewport_changed: None,
            last_timestamp: None,
            pending_frame: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, mut packet: hang::Frame) -> Result<()> {
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        // Transport uses length-prefixed NALs (avcC style), openh264 expects Annex B.
        let annex_b = length_prefixed_to_annex_b(&payload);

        let maybe_yuv = self
            .decoder
            .decode(&annex_b)
            .map_err(|e| anyhow::anyhow!("openh264 decode error: {e}"))?;

        if let Some(yuv) = maybe_yuv {
            let (w, h) = yuv.dimensions();
            let w = w as u32;
            let h = h as u32;

            // Convert YUV → RGBA using openh264's built-in converter.
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            yuv.write_rgba8(&mut rgba);

            let img =
                RgbaImage::from_raw(w, h, rgba).context("failed to create RgbaImage from RGBA")?;
            self.pending_frame = Some((img, w, h));
        }

        self.last_timestamp = Some(packet.timestamp);
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<av::DecodedFrame>> {
        let Some((img, src_w, src_h)) = self.pending_frame.take() else {
            return Ok(None);
        };

        let last_timestamp = self
            .last_timestamp
            .as_ref()
            .context("missing last packet timestamp")?;
        let delay = self.clock.frame_delay(last_timestamp);
        let timestamp = Duration::from(*last_timestamp);

        // Apply viewport scaling AFTER decode (fix: was consuming viewport_pending before decode
        // in a previous implementation, leading to a bug).
        if let Some((max_w, max_h)) = self.viewport_changed.take() {
            let (tw, th) = fit_within(src_w, src_h, max_w, max_h);
            self.scaler.set_target_dimensions(tw, th);
        }

        let final_img =
            if let Some((scaled, sw, sh)) = self.scaler.scale_rgba(img.as_raw(), src_w, src_h)? {
                RgbaImage::from_raw(sw, sh, scaled).context("failed to create scaled RgbaImage")?
            } else {
                img
            };

        let frame_delay = Delay::from_saturating_duration(delay);

        Ok(Some(DecodedFrame {
            frame: Frame::from_parts(final_img, 0, 0, frame_delay),
            timestamp,
        }))
    }
}
