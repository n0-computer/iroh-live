use std::{collections::HashMap, time::Duration};

use anyhow::{Result, bail};
use rav1e::prelude::*;

use crate::{
    config::{AV1, VideoCodec, VideoConfig},
    format::{EncodedFrame, ScaleMode, VideoEncoderConfig, VideoFrame},
    processing::{convert::pixel_format_to_yuv420, scale::Scaler},
    traits::{VideoEncoder, VideoEncoderFactory},
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
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    /// AV1 sequence header, populated after context creation.
    seq_header: Vec<u8>,
    /// Maps rav1e input_frameno to the original frame timestamp, so we can
    /// recover timestamps after rav1e's lookahead reordering.
    timestamp_map: HashMap<u64, Duration>,
    /// Encoded packets ready for collection.
    packet_buf: std::collections::VecDeque<EncodedFrame>,
}

/// Bits-per-pixel factor for AV1 default bitrate calculation (~30% more efficient than H.264).
const AV1_BPP: f32 = 0.05;

impl Av1Encoder {
    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(AV1_BPP);
        let keyframe_interval = config.keyframe_interval_or_default();
        let scale_mode = config.scale_mode;

        let mut enc_config = EncoderConfig::with_speed_preset(10);
        enc_config.width = width as usize;
        enc_config.height = height as usize;
        enc_config.bit_depth = 8;
        enc_config.chroma_sampling = ChromaSampling::Cs420;
        enc_config.bitrate = bitrate as i32;
        enc_config.min_key_frame_interval = 0;
        enc_config.max_key_frame_interval = keyframe_interval as u64;
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

        tracing::info!(
            width,
            height,
            framerate,
            bitrate,
            "AV1 software encoder ready (rav1e)"
        );

        Ok(Self {
            ctx,
            width,
            height,
            framerate,
            bitrate,
            frame_count: 0,
            scale_mode,
            scaler: Scaler::new(Some((width, height))),
            seq_header,
            timestamp_map: HashMap::new(),
            packet_buf: std::collections::VecDeque::new(),
        })
    }

    /// Scales the frame to encoder dimensions if needed.
    fn scale_if_needed(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        let [fw, fh] = frame.dimensions;
        if fw == self.width && fh == self.height {
            return Ok(frame);
        }
        let (tw, th) = self.scale_mode.resolve((fw, fh), (self.width, self.height));
        if tw == fw && th == fh {
            return Ok(frame);
        }
        self.scaler.set_target_dimensions(tw, th);
        let img = frame.rgba_image();
        let scaled = if self.scale_mode == ScaleMode::Cover {
            self.scaler.scale_cover_rgba(img.as_raw(), fw, fh)?
        } else {
            self.scaler.scale_rgba(img.as_raw(), fw, fh)?
        };
        match scaled {
            Some((data, w, h)) => Ok(VideoFrame::new_rgba(data.into(), w, h, frame.timestamp)),
            None => Ok(frame),
        }
    }

    /// Drain all available packets from the encoder context.
    fn drain_packets(&mut self) -> Result<()> {
        loop {
            match self.ctx.receive_packet() {
                Ok(packet) => {
                    let is_keyframe = packet.frame_type == FrameType::KEY;
                    let timestamp = self
                        .timestamp_map
                        .remove(&packet.input_frameno)
                        .unwrap_or_default();

                    self.packet_buf.push_back(EncodedFrame {
                        is_keyframe,
                        timestamp,
                        payload: packet.data.into(),
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

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(AV1_BPP);
        VideoConfig {
            codec: VideoCodec::AV1(AV1 {
                profile: 0,
                level: 0,
                tier: 'M',
                bitdepth: 8,
                mono_chrome: false,
                chroma_subsampling_x: true,
                chroma_subsampling_y: true,
                chroma_sample_position: 0,
                // Match the actual conversion pipeline:
                // - Primaries: BT.709 (same gamut as sRGB, correct for screen/camera)
                // - Transfer: BT.709 (close to sRGB gamma)
                // - Matrix: BT.601 (matches YuvStandardMatrix::Bt601 in convert.rs)
                color_primaries: 1,          // BT.709
                transfer_characteristics: 1, // BT.709
                matrix_coefficients: 6,      // BT.601 (SMPTE 170M)
                full_range: false,
            }),
            description: None,
            coded_width: Some(config.width),
            coded_height: Some(config.height),
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: Some(bitrate),
            framerate: Some(config.framerate as f64),
            optimize_for_latency: Some(true),
        }
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
                // Match the actual conversion pipeline:
                // - Primaries: BT.709 (same gamut as sRGB, correct for screen/camera)
                // - Transfer: BT.709 (close to sRGB gamma)
                // - Matrix: BT.601 (matches YuvStandardMatrix::Bt601 in convert.rs)
                color_primaries: 1,          // BT.709
                transfer_characteristics: 1, // BT.709
                matrix_coefficients: 6,      // BT.601 (SMPTE 170M)
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

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        let frame = self.scale_if_needed(frame)?;
        let [w, h] = frame.dimensions;
        let yuv = match &frame.data {
            crate::format::FrameData::Packed { pixel_format, data } => {
                pixel_format_to_yuv420(data, w, h, *pixel_format)?
            }
            _ => {
                // GPU or NV12 frames: fall back through RGBA.
                let img = frame.rgba_image();
                pixel_format_to_yuv420(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
            }
        };

        let mut rav1e_frame = self.ctx.new_frame();

        // Copy YUV planes into the rav1e frame using copy_from_raw_u8,
        // which correctly handles the plane's internal stride/padding.
        rav1e_frame.planes[0].copy_from_raw_u8(&yuv.y, yuv.y_stride as usize, 1);
        rav1e_frame.planes[1].copy_from_raw_u8(&yuv.u, yuv.u_stride as usize, 1);
        rav1e_frame.planes[2].copy_from_raw_u8(&yuv.v, yuv.v_stride as usize, 1);

        // Record the input timestamp so drain_packets can recover it after
        // rav1e's lookahead reordering.
        self.timestamp_map.insert(self.frame_count, frame.timestamp);
        self.frame_count += 1;

        self.ctx
            .send_frame(rav1e_frame)
            .map_err(|e| anyhow::anyhow!("rav1e send_frame error: {e}"))?;

        self.drain_packets()?;

        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        Ok(self.packet_buf.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::test_util::make_rgba_frame,
        format::VideoPreset,
        traits::{VideoEncoder, VideoEncoderFactory},
    };

    /// rav1e buffers frames for look-ahead even in low-latency mode.
    /// Helper to send frames and collect all resulting packets.
    fn encode_n_frames(enc: &mut Av1Encoder, n: usize) -> Vec<EncodedFrame> {
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
        assert!(packets[0].is_keyframe, "first packet should be keyframe");
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
                && pkt.is_keyframe
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
