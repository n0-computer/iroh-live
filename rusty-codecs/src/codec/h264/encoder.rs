use anyhow::Result;
use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Encoder as OpenH264Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
        RateControlMode, UsageType,
    },
    formats::YUVSource,
};

use super::annexb::{annex_b_to_length_prefixed, build_avcc, extract_sps_pps, parse_annex_b};
use crate::{
    config::VideoConfig,
    format::{EncodedFrame, NalFormat, ScaleMode, VideoEncoderConfig, VideoFrame},
    processing::{
        convert::{YuvData, pixel_format_to_yuv420},
        scale::Scaler,
    },
    traits::{VideoEncoder, VideoEncoderFactory},
};

#[derive(derive_more::Debug)]
pub struct H264Encoder {
    #[debug(skip)]
    encoder: OpenH264Encoder,
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    nal_format: NalFormat,
    scale_mode: ScaleMode,
    #[debug(skip)]
    scaler: Scaler,
    /// avcC description, populated after first successful encode (avcC mode only).
    avcc: Option<Vec<u8>>,
    /// Encoded packets ready for collection.
    packet_buf: std::collections::VecDeque<EncodedFrame>,
}

/// Borrowed view of I420 planes — avoids copying when the frame already has
/// the right layout. Used for the zero-copy I420 encode path.
struct YuvSlices<'a> {
    y: &'a [u8],
    u: &'a [u8],
    v: &'a [u8],
    width: u32,
    height: u32,
    y_stride: u32,
    u_stride: u32,
    v_stride: u32,
}

impl YUVSource for YuvSlices<'_> {
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
        self.y
    }

    fn u(&self) -> &[u8] {
        self.u
    }

    fn v(&self) -> &[u8] {
        self.v
    }
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

/// Bits-per-pixel factor for H.264 default bitrate calculation.
const H264_BPP: f32 = 0.07;

impl H264Encoder {
    fn new(config: VideoEncoderConfig) -> Result<Self> {
        let width = config.width;
        let height = config.height;
        let framerate = config.framerate;
        let bitrate = config.bitrate_or_default(H264_BPP);
        let nal_format = config.nal_format;

        let keyframe_interval = config.keyframe_interval_or_default();

        let enc_config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate as u32))
            .max_frame_rate(FrameRate::from_hz(framerate as f32))
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .intra_frame_period(IntraFramePeriod::from_num_frames(keyframe_interval));

        let api = OpenH264API::from_source();
        let mut encoder = OpenH264Encoder::with_api_config(api, enc_config)?;

        let avcc = if nal_format == NalFormat::Avcc {
            // Encode a black frame to extract SPS/PPS for the avcC description.
            // openh264 only emits parameter sets in its first encoded IDR frame.
            let black = YuvData::black(width, height);
            let bitstream = encoder.encode(&black)?;
            let annex_b = bitstream.to_vec();
            let nals = parse_annex_b(&annex_b);
            let avcc = extract_sps_pps(&nals).map(|(sps, pps)| build_avcc(sps, pps));
            // Force the next real frame to be an IDR since we consumed the first one.
            encoder.force_intra_frame();
            avcc
        } else {
            // Annex B mode: SPS/PPS are inline in keyframes, no priming needed.
            None
        };

        tracing::info!(
            width,
            height,
            framerate,
            bitrate,
            "H.264 software encoder ready (openh264)"
        );

        Ok(Self {
            encoder,
            width,
            height,
            framerate,
            bitrate,
            nal_format,
            scale_mode: config.scale_mode,
            scaler: Scaler::new(Some((width, height))),
            avcc,
            packet_buf: std::collections::VecDeque::new(),
        })
    }
}

impl H264Encoder {
    fn scale_if_needed(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        crate::processing::scale::scale_frame_if_needed(
            &mut self.scaler,
            self.scale_mode,
            self.width,
            self.height,
            frame,
        )
    }
}

impl VideoEncoderFactory for H264Encoder {
    const ID: &str = "h264-openh264";

    fn with_config(config: VideoEncoderConfig) -> Result<Self> {
        Self::new(config)
    }

    fn config_for(config: &VideoEncoderConfig) -> VideoConfig {
        let bitrate = config.bitrate_or_default(H264_BPP);
        let inline = config.nal_format == NalFormat::AnnexB;
        super::h264_video_config(
            config.width,
            config.height,
            bitrate,
            config.framerate,
            inline,
            None,
        )
    }
}

impl VideoEncoder for H264Encoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> VideoConfig {
        let inline = self.nal_format == NalFormat::AnnexB;
        super::h264_video_config(
            self.width,
            self.height,
            self.bitrate,
            self.framerate,
            inline,
            self.avcc.clone().map(Into::into),
        )
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        // Propagate the input frame's timestamp to the encoded output.
        let input_timestamp = frame.timestamp;
        // Scale frame to encoder dimensions if needed.
        let frame = self.scale_if_needed(frame)?;
        let [w, h] = frame.dimensions;
        // Fast path: if the frame is already I420 with matching strides,
        // borrow the planes directly without copying (PF2).
        let (annex_b, frame_type) = if let crate::format::FrameData::I420 { y, u, v } = &frame.data
        {
            let slices = YuvSlices {
                y,
                u,
                v,
                width: w,
                height: h,
                y_stride: w,
                u_stride: w / 2,
                v_stride: w / 2,
            };
            let bs = self.encoder.encode(&slices)?;
            (bs.to_vec(), bs.frame_type())
        } else {
            let yuv = match &frame.data {
                crate::format::FrameData::Packed { pixel_format, data } => {
                    pixel_format_to_yuv420(data, w, h, *pixel_format)?
                }
                _ => {
                    let img = frame.rgba_image();
                    pixel_format_to_yuv420(img.as_raw(), w, h, crate::format::PixelFormat::Rgba)?
                }
            };
            let bs = self.encoder.encode(&yuv)?;
            (bs.to_vec(), bs.frame_type())
        };

        self.finish_encode(annex_b, frame_type, input_timestamp)
    }

    fn pop_packet(&mut self) -> Result<Option<EncodedFrame>> {
        Ok(self.packet_buf.pop_front())
    }
}

impl H264Encoder {
    /// Converts encoded Annex B data into an `EncodedFrame` and pushes it
    /// to the output buffer.
    fn finish_encode(
        &mut self,
        annex_b: Vec<u8>,
        frame_type: FrameType,
        timestamp: std::time::Duration,
    ) -> Result<()> {
        if matches!(frame_type, FrameType::Skip | FrameType::Invalid) {
            return Ok(());
        }

        if self.nal_format == NalFormat::Avcc && self.avcc.is_none() {
            let nals = parse_annex_b(&annex_b);
            if let Some((sps, pps)) = extract_sps_pps(&nals) {
                self.avcc = Some(build_avcc(sps, pps));
            }
        }

        let payload: bytes::Bytes = match self.nal_format {
            NalFormat::AnnexB => annex_b.into(),
            NalFormat::Avcc => annex_b_to_length_prefixed(&annex_b).into(),
        };

        let is_keyframe = matches!(frame_type, FrameType::IDR | FrameType::I);

        self.packet_buf.push_back(EncodedFrame {
            is_keyframe,
            timestamp,
            payload,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::test_util::make_rgba_frame,
        config::VideoCodec,
        format::VideoPreset,
        traits::{VideoEncoder, VideoEncoderFactory},
    };

    #[test]
    fn encode_single_frame() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let frame = make_rgba_frame(320, 180, 255, 0, 0);
        enc.push_frame(frame).unwrap();
        let packet = enc.pop_packet().unwrap();
        assert!(packet.is_some(), "should produce a packet");
        let pkt = packet.unwrap();
        assert!(pkt.is_keyframe, "first frame should be keyframe");
    }

    #[test]
    fn annex_b_default_no_description() {
        let enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let config = enc.config();
        assert!(
            config.description.is_none(),
            "Annex B mode should have no avcC description"
        );
        let VideoCodec::H264(h264) = &config.codec else {
            panic!("expected H264 codec");
        };
        assert!(h264.inline, "Annex B mode should set inline=true");
    }

    #[test]
    fn encode_sequence_has_keyframes() {
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
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
    fn timestamps_propagate() {
        use std::time::Duration;
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let mut prev_ts = None;
        for i in 0..10 {
            let mut frame = make_rgba_frame(320, 180, 64, 64, 64);
            frame.timestamp = Duration::from_millis(i * 33); // ~30fps
            enc.push_frame(frame).unwrap();
            if let Some(pkt) = enc.pop_packet().unwrap() {
                if let Some(prev) = prev_ts {
                    assert!(pkt.timestamp > prev, "timestamps should increase");
                }
                prev_ts = Some(pkt.timestamp);
            }
        }
        // Verify timestamps match input, not synthetic counter.
        assert!(
            prev_ts.unwrap().as_millis() > 200,
            "timestamps should come from input"
        );
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

    #[test]
    fn i420_zero_copy_path() {
        // Verify the I420 fast path (YuvSlices borrow) produces valid output
        // without copying the plane data (PF2).
        use crate::processing::convert::YuvData;
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();
        let yuv = YuvData::black(320, 180);
        let frame = VideoFrame::new_i420(
            yuv.y.into(),
            yuv.u.into(),
            yuv.v.into(),
            320,
            180,
            std::time::Duration::from_millis(42),
        );
        enc.push_frame(frame).unwrap();
        let pkt = enc.pop_packet().unwrap().expect("should encode I420 frame");
        assert!(pkt.is_keyframe, "first frame should be IDR");
        assert_eq!(
            pkt.timestamp,
            std::time::Duration::from_millis(42),
            "timestamp should propagate from input"
        );
    }
}
