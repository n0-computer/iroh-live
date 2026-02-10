use anyhow::{Result, bail};
use hang::{
    Timestamp,
    catalog::{AV1, VideoCodec, VideoConfig},
};
use rav1e::prelude::*;

use crate::{
    av::{self, VideoEncoder, VideoEncoderFactory, VideoPreset},
    codec::video::util::convert::pixel_format_to_yuv420,
};

#[derive(derive_more::Debug)]
pub struct Av1Encoder {
    #[debug(skip)]
    ctx: Context<u8>,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    /// AV1 sequence header, populated after context creation.
    seq_header: Vec<u8>,
    /// Encoded packets ready for collection.
    packet_buf: Vec<hang::Frame>,
}

impl Av1Encoder {
    fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        let pixels = width * height;
        let framerate_factor = 30.0 + (framerate as f32 - 30.) / 2.;
        // AV1 is ~30% more efficient than H.264
        let bitrate = (pixels as f32 * 0.05 * framerate_factor).round() as u64;

        let mut enc_config = EncoderConfig::with_speed_preset(10);
        enc_config.width = width as usize;
        enc_config.height = height as usize;
        enc_config.bit_depth = 8;
        enc_config.chroma_sampling = ChromaSampling::Cs420;
        enc_config.bitrate = bitrate as i32;
        enc_config.min_key_frame_interval = 0;
        enc_config.max_key_frame_interval = framerate as u64;
        enc_config.low_latency = true;
        enc_config.error_resilient = true;
        enc_config.time_base = Rational::new(1, framerate as u64);
        // Use quantizer 0 to rely on bitrate-based rate control
        enc_config.quantizer = 0;
        enc_config.min_quantizer = 1;

        let config = Config::new()
            .with_encoder_config(enc_config)
            .with_threads(0);

        let ctx: Context<u8> = config
            .new_context()
            .map_err(|e| anyhow::anyhow!("rav1e context creation failed: {e}"))?;

        let seq_header = ctx.container_sequence_header();

        Ok(Self {
            ctx,
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            seq_header,
            packet_buf: Vec::new(),
        })
    }

    /// Drain all available packets from the encoder context.
    fn drain_packets(&mut self) -> Result<()> {
        loop {
            match self.ctx.receive_packet() {
                Ok(packet) => {
                    let keyframe = packet.frame_type == FrameType::KEY;
                    let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
                    self.frame_count += 1;

                    self.packet_buf.push(hang::Frame {
                        payload: packet.data.into(),
                        timestamp: Timestamp::from_micros(timestamp_us)?,
                        keyframe,
                    });
                }
                Err(EncoderStatus::Encoded) => {
                    // Frame encoded but no packet ready yet (look-ahead)
                    break;
                }
                Err(EncoderStatus::NeedMoreData) => {
                    // Encoder needs more frames before it can produce output
                    break;
                }
                Err(EncoderStatus::LimitReached) => {
                    break;
                }
                Err(e) => {
                    bail!("rav1e receive_packet error: {e}");
                }
            }
        }
        Ok(())
    }
}

impl VideoEncoderFactory for Av1Encoder {
    const ID: &str = "av1-rav1e";

    fn with_preset(preset: VideoPreset) -> Result<Self> {
        Self::new(preset.width(), preset.height(), preset.fps())
    }
}

impl VideoEncoder for Av1Encoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        VideoConfig {
            codec: VideoCodec::AV1(AV1 {
                profile: 0, // Main
                level: 0,
                tier: 'M',
                bitdepth: 8,
                mono_chrome: false,
                chroma_subsampling_x: true,
                chroma_subsampling_y: true,
                chroma_sample_position: 0,
                color_primaries: 1, // BT.709
                transfer_characteristics: 1,
                matrix_coefficients: 1,
                full_range: false,
            }),
            description: Some(self.seq_header.clone().into()),
            coded_width: Some(self.width),
            coded_height: Some(self.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(self.bitrate),
            framerate: Some(self.framerate as f64),
            optimize_for_latency: Some(true),
        }
    }

    fn push_frame(&mut self, frame: av::VideoFrame) -> Result<()> {
        let [w, h] = frame.format.dimensions;
        let yuv = pixel_format_to_yuv420(&frame.raw, w, h, frame.format.pixel_format)?;

        let mut rav1e_frame = self.ctx.new_frame();

        // Copy YUV planes into the rav1e frame using copy_from_raw_u8,
        // which correctly handles the plane's internal stride/padding.
        rav1e_frame.planes[0].copy_from_raw_u8(&yuv.y, yuv.y_stride as usize, 1);
        rav1e_frame.planes[1].copy_from_raw_u8(&yuv.u, yuv.u_stride as usize, 1);
        rav1e_frame.planes[2].copy_from_raw_u8(&yuv.v, yuv.v_stride as usize, 1);

        self.ctx
            .send_frame(rav1e_frame)
            .map_err(|e| anyhow::anyhow!("rav1e send_frame error: {e}"))?;

        self.drain_packets()?;

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        Ok(if self.packet_buf.is_empty() {
            None
        } else {
            Some(self.packet_buf.remove(0))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av::{PixelFormat, VideoEncoder, VideoFormat, VideoFrame, VideoPreset};

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

    /// rav1e buffers frames for look-ahead even in low-latency mode.
    /// Helper to send frames and collect all resulting packets.
    fn encode_n_frames(enc: &mut Av1Encoder, n: usize) -> Vec<hang::Frame> {
        let mut packets = Vec::new();
        for _ in 0..n {
            let frame = make_rgba_frame(320, 180, 255, 0, 0);
            enc.push_frame(frame).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        packets
    }

    #[test]
    fn encode_produces_packets() {
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();
        // rav1e buffers frames for look-ahead; send enough to produce output
        let packets = encode_n_frames(&mut enc, 30);
        assert!(!packets.is_empty(), "should produce at least one packet");
        assert!(packets[0].keyframe, "first packet should be keyframe");
    }

    #[test]
    fn sequence_header_after_creation() {
        let enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();
        let desc = enc.config().description;
        assert!(
            desc.is_some(),
            "sequence header should be populated after creation"
        );
        assert!(
            !desc.unwrap().is_empty(),
            "sequence header should not be empty"
        );
    }

    #[test]
    fn encode_sequence_has_keyframes() {
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();
        let mut keyframe_count = 0;
        for _ in 0..60 {
            let frame = make_rgba_frame(320, 180, 128, 128, 128);
            enc.push_frame(frame).unwrap();
            if let Some(pkt) = enc.pop_packet().unwrap()
                && pkt.keyframe
            {
                keyframe_count += 1;
            }
        }
        assert!(
            keyframe_count >= 2,
            "expected >= 2 keyframes, got {keyframe_count}"
        );
    }

    #[test]
    fn timestamps_increase() {
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();
        let mut prev_ts = None;
        for _ in 0..10 {
            let frame = make_rgba_frame(320, 180, 64, 64, 64);
            enc.push_frame(frame).unwrap();
            if let Some(pkt) = enc.pop_packet().unwrap() {
                if let Some(prev) = prev_ts {
                    assert!(pkt.timestamp > prev, "timestamps should increase");
                }
                prev_ts = Some(pkt.timestamp);
            }
        }
    }

    #[test]
    fn multiple_presets() {
        for preset in VideoPreset::all() {
            let (w, h) = preset.dimensions();
            let mut enc = Av1Encoder::with_preset(preset).unwrap();
            let mut got_packet = false;
            for _ in 0..30 {
                let frame = make_rgba_frame(w, h, 200, 100, 50);
                enc.push_frame(frame).unwrap();
                if enc.pop_packet().unwrap().is_some() {
                    got_packet = true;
                    break;
                }
            }
            assert!(got_packet, "should encode at {w}x{h}");
        }
    }

    #[test]
    fn config_fields() {
        let enc = Av1Encoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(matches!(config.codec, VideoCodec::AV1(_)));
        assert_eq!(config.coded_width, Some(640));
        assert_eq!(config.coded_height, Some(360));
        assert_eq!(config.framerate, Some(30.0));
        assert_eq!(config.optimize_for_latency, Some(true));
    }
}
