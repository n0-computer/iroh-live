use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use bytes::Buf;
use dav1d::PlanarImageComponent;
use hang::catalog::{VideoCodec, VideoConfig};
use image::{Delay, Frame, RgbaImage};

use crate::av::{self, DecodeConfig, DecodedFrame, VideoDecoder};

use super::util::{
    StreamClock,
    convert::{YuvData, yuv420_to_rgba_data},
    scale::{Scaler, fit_within},
};

#[derive(derive_more::Debug)]
pub struct Av1VideoDecoder {
    #[debug(skip)]
    decoder: dav1d::Decoder,
    scaler: Scaler,
    clock: StreamClock,
    viewport_changed: Option<(u32, u32)>,
    last_timestamp: Option<hang::Timestamp>,
    /// Decoded frame waiting to be collected via `pop_frame`.
    #[debug(skip)]
    pending_frame: Option<(RgbaImage, u32, u32)>,
}

impl VideoDecoder for Av1VideoDecoder {
    fn name(&self) -> &str {
        "av1-dav1d"
    }

    fn new(config: &VideoConfig, _playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        if !matches!(&config.codec, VideoCodec::AV1(_)) {
            bail!("Av1VideoDecoder only supports AV1, got {}", config.codec);
        }

        let mut settings = dav1d::Settings::new();
        settings.set_n_threads(0);
        settings.set_max_frame_delay(1);

        let mut decoder =
            dav1d::Decoder::with_settings(&settings).context("failed to create dav1d decoder")?;

        // If we have a sequence header, feed it to prime the decoder.
        if let Some(description) = &config.description {
            let data: Vec<u8> = description.to_vec();
            let _ = decoder.send_data(data, None, None, None);
            // Drain any picture produced by the sequence header.
            let _ = decoder.get_picture();
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

        self.decoder
            .send_data(payload.to_vec(), None, None, None)
            .map_err(|e| anyhow::anyhow!("dav1d send_data error: {e}"))?;

        match self.decoder.get_picture() {
            Ok(picture) => {
                let w = picture.width();
                let h = picture.height();

                let y_plane = picture.plane(PlanarImageComponent::Y);
                let u_plane = picture.plane(PlanarImageComponent::U);
                let v_plane = picture.plane(PlanarImageComponent::V);

                let y_stride = picture.stride(PlanarImageComponent::Y);
                let u_stride = picture.stride(PlanarImageComponent::U);
                let v_stride = picture.stride(PlanarImageComponent::V);

                let yuv = YuvData {
                    y: y_plane.as_ref().to_vec(),
                    u: u_plane.as_ref().to_vec(),
                    v: v_plane.as_ref().to_vec(),
                    y_stride,
                    u_stride,
                    v_stride,
                    width: w,
                    height: h,
                };

                let rgba = yuv420_to_rgba_data(&yuv)?;

                let img = RgbaImage::from_raw(w, h, rgba)
                    .context("failed to create RgbaImage from RGBA")?;
                self.pending_frame = Some((img, w, h));
            }
            Err(e) if e.is_again() => {
                // No picture available yet — decoder needs more data.
            }
            Err(e) => {
                bail!("dav1d get_picture error: {e}");
            }
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
