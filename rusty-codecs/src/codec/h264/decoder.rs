use std::time::Duration;

use anyhow::{Context, Result, bail};
use openh264::{decoder::Decoder, formats::YUVSource};

use super::annexb::{avcc_to_annex_b, length_prefixed_to_annex_b};
use crate::{
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, MediaPacket, NalFormat, PixelFormat, VideoFrame},
    processing::{
        convert::{yuv420_to_bgra_from_slices, yuv420_to_rgba_from_slices},
        scale::{Scaler, fit_within},
    },
    traits::VideoDecoder,
};

#[derive(derive_more::Debug)]
pub struct H264VideoDecoder {
    #[debug(skip)]
    decoder: Decoder,
    /// NAL framing format of incoming packets.
    nal_format: NalFormat,
    pixel_format: PixelFormat,
    scaler: Scaler,
    viewport_changed: Option<(u32, u32)>,
    last_timestamp: Option<Duration>,
    /// Decoded pixel data waiting to be collected via `pop_frame`: `(pixels, w, h)`.
    #[debug(skip)]
    pending_frame: Option<(Vec<u8>, u32, u32)>,
}

impl VideoDecoder for H264VideoDecoder {
    fn name(&self) -> &str {
        "h264-openh264"
    }

    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        let inline = match &config.codec {
            VideoCodec::H264(h264) => h264.inline,
            other => bail!(
                "Unsupported codec {other} (only H.264 is supported, AV1 planned for phase 2)",
            ),
        };

        // Determine NAL format: inline (avc3) → Annex B, non-inline (avc1) → length-prefixed.
        // Also treat missing description as Annex B regardless of inline flag.
        let nal_format = if inline || config.description.is_none() {
            NalFormat::AnnexB
        } else {
            NalFormat::Avcc
        };

        let mut decoder = Decoder::new().context("failed to create openh264 decoder")?;

        // If we have avcC extradata, feed the SPS/PPS to the decoder upfront.
        if let Some(description) = &config.description
            && let Some(annex_b) = avcc_to_annex_b(description)
        {
            let _ = decoder.decode(&annex_b);
        }

        tracing::info!("H.264 software decoder ready (openh264)");

        Ok(Self {
            decoder,
            nal_format,
            pixel_format: playback_config.pixel_format,
            scaler: Scaler::new(None),
            viewport_changed: None,
            last_timestamp: None,
            pending_frame: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        // Convert to Annex B if needed — openh264 expects start-code framing.
        let annex_b_buf;
        let annex_b: &[u8] = match self.nal_format {
            NalFormat::AnnexB => &payload,
            NalFormat::Avcc => {
                annex_b_buf = length_prefixed_to_annex_b(&payload);
                &annex_b_buf
            }
        };

        let maybe_yuv = self
            .decoder
            .decode(annex_b)
            .map_err(|e| anyhow::anyhow!("openh264 decode error: {e}"))?;

        if let Some(yuv) = maybe_yuv {
            let (w, h) = yuv.dimensions();
            let w = w as u32;
            let h = h as u32;
            let pixels = match self.pixel_format {
                PixelFormat::Bgra => {
                    // Use yuvutils-rs for direct YUV→BGRA (avoids RGBA + swap).
                    let (y_stride, u_stride, v_stride) = yuv.strides();
                    yuv420_to_bgra_from_slices(
                        yuv.y(),
                        y_stride as u32,
                        yuv.u(),
                        u_stride as u32,
                        yuv.v(),
                        v_stride as u32,
                        w,
                        h,
                    )?
                }
                PixelFormat::Rgba => {
                    // Use yuvutils-rs for consistent BT.601 limited-range conversion
                    // across all backends (software, VAAPI, VideoToolbox).
                    let (y_stride, u_stride, v_stride) = yuv.strides();
                    yuv420_to_rgba_from_slices(
                        yuv.y(),
                        y_stride as u32,
                        yuv.u(),
                        u_stride as u32,
                        yuv.v(),
                        v_stride as u32,
                        w,
                        h,
                    )?
                }
            };

            self.pending_frame = Some((pixels, w, h));
        }

        self.last_timestamp = Some(packet.timestamp);
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let Some((pixels, src_w, src_h)) = self.pending_frame.take() else {
            return Ok(None);
        };

        let timestamp = self
            .last_timestamp
            .context("missing last packet timestamp")?;
        // Apply viewport scaling AFTER decode.
        if let Some((max_w, max_h)) = self.viewport_changed.take() {
            let (tw, th) = fit_within(src_w, src_h, max_w, max_h);
            self.scaler.set_target_dimensions(tw, th);
        }

        let (data, w, h) =
            if let Some((scaled, sw, sh)) = self.scaler.scale_rgba(&pixels, src_w, src_h)? {
                (scaled, sw, sh)
            } else {
                (pixels, src_w, src_h)
            };

        Ok(Some(VideoFrame::new_cpu_with_format(
            data,
            w,
            h,
            timestamp,
            self.pixel_format,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::{
            h264::encoder::H264Encoder,
            test_util::{encoded_frames_to_media_packets, make_rgba_frame},
        },
        config::{AV1, H264},
        format::{EncodedFrame, VideoFrame, VideoPreset},
        traits::{VideoDecoder, VideoEncoder, VideoEncoderFactory},
    };

    fn encode_frames(enc: &mut H264Encoder, frames: &[VideoFrame]) -> Vec<EncodedFrame> {
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

        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();

        let mut decoded_count = 0;
        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                let img = frame.rgba_image();
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

        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                let img = frame.rgba_image();
                assert!(img.width() <= 320, "width {} > 320", img.width());
                assert!(img.height() <= 180, "height {} > 180", img.height());
            }
        }
    }

    #[test]
    fn pop_frame_empty_before_push() {
        let config = VideoConfig {
            codec: VideoCodec::H264(H264 {
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
        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                last_frame = Some(frame);
            }
        }

        let frame = last_frame.expect("should have decoded at least one frame");
        let img = frame.rgba_image();
        let pixel = img.get_pixel(w / 2, h / 2);
        assert!(pixel[0] > 150, "R={} should be high", pixel[0]);
        assert!(pixel[1] < 100, "G={} should be low", pixel[1]);
        assert!(pixel[2] < 100, "B={} should be low", pixel[2]);
    }

    #[test]
    fn bgra_output_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let mut enc = H264Encoder::with_preset(VideoPreset::P180).unwrap();

        let frames: Vec<VideoFrame> = (0..5).map(|_| make_rgba_frame(w, h, 255, 0, 0)).collect();
        let packets = encode_frames(&mut enc, &frames);

        let config = enc.config();
        let decode_config = DecodeConfig {
            pixel_format: crate::format::PixelFormat::Bgra,
            ..Default::default()
        };
        let mut dec = H264VideoDecoder::new(&config, &decode_config).unwrap();

        let mut last_frame = None;
        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                last_frame = Some(frame);
            }
        }

        let frame = last_frame.expect("should have decoded at least one frame");
        // Frame should report BGRA pixel format
        if let crate::format::FrameData::Packed {
            pixel_format,
            ref data,
        } = frame.data
        {
            assert_eq!(
                pixel_format,
                crate::format::PixelFormat::Bgra,
                "should be BGRA"
            );
            // In BGRA order, red pixel = [B=low, G=low, R=high, A=255]
            let mid = ((h / 2 * w + w / 2) * 4) as usize;
            assert!(
                data[mid + 2] > 150,
                "BGRA R={} should be high",
                data[mid + 2]
            );
            assert!(data[mid] < 100, "BGRA B={} should be low", data[mid]);
        } else {
            panic!("expected Packed frame");
        }
    }

    #[test]
    fn unsupported_codec_errors() {
        let config = VideoConfig {
            codec: VideoCodec::AV1(AV1::default()),
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
