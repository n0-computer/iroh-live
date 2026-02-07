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
pub struct H264VideoDecoder {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av::{
        PixelFormat, VideoEncoder, VideoEncoderInner, VideoFormat, VideoFrame, VideoPreset,
    };
    use crate::codec::video::encoder::H264Encoder;

    fn make_rgba_frame(w: u32, h: u32, r: u8, g: u8, b: u8) -> VideoFrame {
        let pixel = [r, g, b, 255u8];
        let raw: Vec<u8> = pixel.repeat((w * h) as usize);
        VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [w, h],
            },
            raw: raw.into(),
        }
    }

    fn encode_frames(enc: &mut H264Encoder, frames: &[VideoFrame]) -> Vec<hang::Frame> {
        let mut packets = Vec::new();
        for f in frames {
            enc.push_frame(f.clone()).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        packets
    }

    #[test]
    fn encode_decode_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();

        let frames: Vec<VideoFrame> = (0..10)
            .map(|i| make_rgba_frame(w, h, (i * 25) as u8, 128, 64))
            .collect();
        let packets = encode_frames(&mut enc, &frames);
        assert!(!packets.is_empty());

        let config = enc.config();
        assert!(config.description.is_some());

        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();

        let mut decoded_count = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                let img = frame.img();
                assert_eq!(img.width(), w);
                assert_eq!(img.height(), h);
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    }

    #[test]
    fn viewport_scaling() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P360).unwrap();

        let frames: Vec<VideoFrame> = (0..5)
            .map(|_| make_rgba_frame(640, 360, 100, 100, 100))
            .collect();
        let packets = encode_frames(&mut enc, &frames);

        let config = enc.config();
        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();

        dec.set_viewport(320, 180);

        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                let img = frame.img();
                assert!(img.width() <= 320, "width {} > 320", img.width());
                assert!(img.height() <= 180, "height {} > 180", img.height());
            }
        }
    }

    #[test]
    fn pop_frame_empty_before_push() {
        let config = VideoConfig {
            codec: VideoCodec::H264(hang::catalog::H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline: false,
            }),
            description: None,
            coded_width: Some(320),
            coded_height: Some(180),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: None,
            framerate: None,
            optimize_for_latency: None,
        };
        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();
        assert!(dec.pop_frame().unwrap().is_none());
    }

    #[test]
    fn solid_red_visual_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();

        let frames: Vec<VideoFrame> = (0..5).map(|_| make_rgba_frame(w, h, 255, 0, 0)).collect();
        let packets = encode_frames(&mut enc, &frames);

        let config = enc.config();
        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();

        let mut last_frame = None;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                last_frame = Some(frame);
            }
        }

        let frame = last_frame.expect("should have decoded at least one frame");
        let img = frame.img();
        let pixel = img.get_pixel(w / 2, h / 2);
        assert!(pixel[0] > 150, "R={} should be high", pixel[0]);
        assert!(pixel[1] < 100, "G={} should be low", pixel[1]);
        assert!(pixel[2] < 100, "B={} should be low", pixel[2]);
    }

    #[test]
    fn unsupported_codec_errors() {
        let config = VideoConfig {
            codec: VideoCodec::AV1(hang::catalog::AV1::default()),
            description: None,
            coded_width: Some(320),
            coded_height: Some(180),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: None,
            framerate: None,
            optimize_for_latency: None,
        };
        let decode_config = DecodeConfig::default();
        let result = H264VideoDecoder::new(&config, &decode_config);
        assert!(result.is_err());
    }
}
