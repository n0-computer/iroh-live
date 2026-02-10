use anyhow::Result;
use hang::{
    Timestamp,
    catalog::{H264, VideoCodec, VideoConfig},
};
use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Encoder as OpenH264Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
        RateControlMode, UsageType,
    },
    formats::YUVSource,
};

use crate::{
    av::{self, VideoEncoder, VideoEncoderFactory, VideoPreset},
    codec::video::util::{
        annexb::{annex_b_to_length_prefixed, build_avcc, extract_sps_pps, parse_annex_b},
        convert::pixel_format_to_yuv420,
    },
};

use super::util::convert::YuvData;

#[derive(derive_more::Debug)]
pub struct H264Encoder {
    #[debug(skip)]
    encoder: OpenH264Encoder,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    /// avcC description, populated after first successful encode.
    avcc: Option<Vec<u8>>,
    /// Encoded packets ready for collection.
    packet_buf: Vec<hang::Frame>,
}

impl YUVSource for YuvData {
    fn dimensions(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (
            self.y_stride as usize,
            self.u_stride as usize,
            self.v_stride as usize,
        )
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

impl H264Encoder {
    fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        let pixels = width * height;
        let framerate_factor = 30.0 + (framerate as f32 - 30.) / 2.;
        let bitrate = (pixels as f32 * 0.07 * framerate_factor).round() as u64;

        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate as u32))
            .max_frame_rate(FrameRate::from_hz(framerate as f32))
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .intra_frame_period(IntraFramePeriod::from_num_frames(framerate));

        let api = OpenH264API::from_source();
        let mut encoder = OpenH264Encoder::with_api_config(api, config)?;

        // Encode a black frame to extract SPS/PPS for the avcC description.
        // openh264 only emits parameter sets in its first encoded IDR frame.
        let black = YuvData::black(width, height);
        let bitstream = encoder.encode(&black)?;
        let annex_b = bitstream.to_vec();
        let nals = parse_annex_b(&annex_b);
        let avcc = extract_sps_pps(&nals).map(|(sps, pps)| build_avcc(&sps, &pps));

        // Force the next real frame to be an IDR since we consumed the first one.
        encoder.force_intra_frame();

        Ok(Self {
            encoder,
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            avcc,
            packet_buf: Vec::new(),
        })
    }
}

impl VideoEncoderFactory for H264Encoder {
    const ID: &str = "h264-openh264";

    fn with_preset(preset: VideoPreset) -> Result<Self> {
        Self::new(preset.width(), preset.height(), preset.fps())
    }
}

impl VideoEncoder for H264Encoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                profile: 0x42, // Baseline
                constraints: 0xE0,
                level: 0x1E, // Level 3.0
                inline: false,
            }),
            description: self.avcc.clone().map(Into::into),
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

        let bitstream = self.encoder.encode(&yuv)?;
        let frame_type = bitstream.frame_type();

        if matches!(frame_type, FrameType::Skip | FrameType::Invalid) {
            return Ok(());
        }

        // openh264 outputs Annex B format
        let annex_b = bitstream.to_vec();

        // On first encode (or first IDR), extract SPS/PPS and build avcC
        if self.avcc.is_none() {
            let nals = parse_annex_b(&annex_b);
            if let Some((sps, pps)) = extract_sps_pps(&nals) {
                self.avcc = Some(build_avcc(&sps, &pps));
            }
        }

        // Convert Annex B → length-prefixed NALs for transport
        let payload = annex_b_to_length_prefixed(&annex_b);

        let keyframe = matches!(frame_type, FrameType::IDR | FrameType::I);
        let timestamp_us = (self.frame_count * 1_000_000) / self.framerate as u64;
        self.frame_count += 1;

        self.packet_buf.push(hang::Frame {
            payload: payload.into(),
            timestamp: Timestamp::from_micros(timestamp_us)?,
            keyframe,
        });

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

    #[test]
    fn encode_single_frame() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let frame = make_rgba_frame(320, 180, 255, 0, 0);
        enc.push_frame(frame).unwrap();
        let packet = enc.pop_packet().unwrap();
        assert!(packet.is_some(), "should produce a packet");
        let pkt = packet.unwrap();
        assert!(pkt.keyframe, "first frame should be keyframe");
    }

    #[test]
    fn avcc_extradata_available_at_construction() {
        let enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let desc = enc.config().description;
        assert!(
            desc.is_some(),
            "avcC should be populated at construction time"
        );
        let avcc = desc.unwrap();
        assert_eq!(avcc[0], 1, "avcC should start with version 1");
    }

    #[test]
    fn encode_sequence_has_keyframes() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
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
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
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
            let mut enc = H264Encoder::with_preset(preset).unwrap();
            let frame = make_rgba_frame(w, h, 200, 100, 50);
            enc.push_frame(frame).unwrap();
            assert!(
                enc.pop_packet().unwrap().is_some(),
                "should encode at {w}x{h}"
            );
        }
    }

    #[test]
    fn config_fields() {
        let enc = H264Encoder::with_preset(VideoPreset::P360).unwrap();
        let config = enc.config();
        assert!(matches!(config.codec, VideoCodec::H264(_)));
        assert_eq!(config.coded_width, Some(640));
        assert_eq!(config.coded_height, Some(360));
        assert_eq!(config.framerate, Some(30.0));
        assert_eq!(config.optimize_for_latency, Some(true));
    }
}
