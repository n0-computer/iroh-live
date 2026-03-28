use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use super::rav1d_safe::{Decoder, PlanarImageComponent, Settings};
use crate::{
    config::{VideoCodec, VideoConfig},
    format::{DecodeConfig, MediaPacket, PixelFormat, VideoFrame},
    processing::{
        convert::{yuv420_to_bgra_into, yuv420_to_rgba_into},
        scale::{Scaler, fit_within},
    },
    traits::VideoDecoder,
};

#[derive(derive_more::Debug)]
pub struct Av1VideoDecoder {
    #[debug(skip)]
    decoder: Decoder,
    scaler: Scaler,
    pixel_format: PixelFormat,
    viewport_changed: Option<(u32, u32)>,
    last_timestamp: Option<Duration>,
    /// Decoded pixel data waiting to be collected via `pop_frame`: `(pixels, w, h)`.
    #[debug(skip)]
    pending_frame: Option<(Vec<u8>, u32, u32)>,
    /// Reusable pixel buffer for YUV→RGBA/BGRA conversion. Avoids per-frame
    /// allocation (~2.6 MB at 1080p).
    #[debug(skip)]
    pixel_buf: Option<(u32, u32, Vec<u8>)>,
}

impl VideoDecoder for Av1VideoDecoder {
    fn name(&self) -> &str {
        "av1-rav1d"
    }

    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        if !matches!(&config.codec, VideoCodec::AV1(_)) {
            bail!("Av1VideoDecoder only supports AV1, got {}", config.codec);
        }

        let mut settings = Settings::new();
        settings.set_n_threads(0);
        settings.set_max_frame_delay(1);

        let decoder =
            Decoder::with_settings(&settings).context("failed to create rav1d decoder")?;

        // Note: config.description contains the av1C container configuration box
        // (from rav1e's container_sequence_header()), which is ISOBMFF/Matroska
        // metadata — NOT raw OBU data. The actual sequence header OBU is embedded
        // in the first keyframe packet, so dav1d will parse it automatically.

        tracing::info!("AV1 software decoder ready (dav1d)");

        Ok(Self {
            decoder,
            scaler: Scaler::new(None),
            pixel_format: playback_config.pixel_format,
            viewport_changed: None,
            last_timestamp: None,
            pending_frame: None,
            pixel_buf: None,
        })
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport_changed = Some((w, h));
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());
        self.decoder
            .send_data(&payload)
            .map_err(|e| anyhow::anyhow!("rav1d send_data error: {e}"))?;

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

                anyhow::ensure!(
                    y_stride >= w && u_stride >= w.div_ceil(2) && v_stride >= w.div_ceil(2),
                    "invalid AV1 plane strides: Y={y_stride} U={u_stride} V={v_stride} for {w}x{h}"
                );

                // Reuse the pixel buffer when dimensions are stable, avoiding a
                // ~2.6 MB allocation per frame at 1080p.
                let mut pixels = match self.pixel_buf.take() {
                    Some((bw, bh, buf)) if bw == w && bh == h => buf,
                    _ => Vec::new(),
                };
                match self.pixel_format {
                    PixelFormat::Bgra => yuv420_to_bgra_into(
                        y_plane,
                        y_stride,
                        u_plane,
                        u_stride,
                        v_plane,
                        v_stride,
                        w,
                        h,
                        &mut pixels,
                    )?,
                    PixelFormat::Rgba => yuv420_to_rgba_into(
                        y_plane,
                        y_stride,
                        u_plane,
                        u_stride,
                        v_plane,
                        v_stride,
                        w,
                        h,
                        &mut pixels,
                    )?,
                };

                self.pending_frame = Some((pixels, w, h));
            }
            Err(e) if e.is_again() => {
                // No picture available yet — decoder needs more data.
            }
            Err(e) => {
                bail!("rav1d get_picture error: {e}");
            }
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
        if let Some((max_w, max_h)) = self.viewport_changed.take() {
            let (tw, th) = fit_within(src_w, src_h, max_w, max_h);
            self.scaler.set_target_dimensions(tw, th);
        }

        let (data, w, h) =
            if let Some((scaled, sw, sh)) = self.scaler.scale_rgba(&pixels, src_w, src_h)? {
                // Scaling produced a new buffer; recycle the original for the
                // next decode cycle's YUV→RGB conversion.
                self.pixel_buf = Some((src_w, src_h, pixels));
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
            av1::encoder::Av1Encoder,
            test_util::{encoded_frames_to_media_packets, make_rgba_frame},
        },
        config::H264,
        format::{EncodedFrame, VideoFrame, VideoPreset},
        traits::{VideoDecoder, VideoEncoder, VideoEncoderFactory},
    };

    fn encode_frames(enc: &mut Av1Encoder, frames: &[VideoFrame]) -> Vec<EncodedFrame> {
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
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();

        // low_latency rav1e emits packets after a few frames; 15 is plenty.
        let frames: Vec<VideoFrame> = (0..15)
            .map(|i| make_rgba_frame(w, h, (i * 4) as u8, 128, 64))
            .collect();
        let packets = encode_frames(&mut enc, &frames);
        assert!(!packets.is_empty());

        let config = enc.config();
        assert!(config.description.is_some());

        let decode_config = DecodeConfig::default();
        let mut dec = Av1VideoDecoder::new(&config, &decode_config).unwrap();

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
            decoded_count >= 2,
            "expected >= 2 decoded frames, got {decoded_count}"
        );
    }

    #[test]
    fn solid_red_visual_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();

        let frames: Vec<VideoFrame> = (0..15).map(|_| make_rgba_frame(w, h, 255, 0, 0)).collect();
        let packets = encode_frames(&mut enc, &frames);

        let config = enc.config();
        let decode_config = DecodeConfig::default();
        let mut dec = Av1VideoDecoder::new(&config, &decode_config).unwrap();

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
    fn viewport_scaling() {
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();

        let frames: Vec<VideoFrame> = (0..15)
            .map(|_| make_rgba_frame(320, 180, 100, 100, 100))
            .collect();
        let packets = encode_frames(&mut enc, &frames);

        let config = enc.config();
        let decode_config = DecodeConfig::default();
        let mut dec = Av1VideoDecoder::new(&config, &decode_config).unwrap();

        dec.set_viewport(160, 90);

        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                let img = frame.rgba_image();
                assert!(img.width() <= 160, "width {} > 160", img.width());
                assert!(img.height() <= 90, "height {} > 90", img.height());
            }
        }
    }

    #[test]
    fn bgra_output_roundtrip() {
        let w = 320u32;
        let h = 180u32;
        let mut enc = Av1Encoder::with_preset(VideoPreset::P180).unwrap();

        let frames: Vec<VideoFrame> = (0..15).map(|_| make_rgba_frame(w, h, 255, 0, 0)).collect();
        let packets = encode_frames(&mut enc, &frames);

        let config = enc.config();
        let decode_config = DecodeConfig {
            pixel_format: crate::format::PixelFormat::Bgra,
            ..Default::default()
        };
        let mut dec = Av1VideoDecoder::new(&config, &decode_config).unwrap();

        let mut last_frame = None;
        let packets = encoded_frames_to_media_packets(packets);
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                last_frame = Some(frame);
            }
        }

        let frame = last_frame.expect("should have decoded at least one frame");
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
            // BGRA red pixel: B=low, G=low, R=high (at offset 2), A=high
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
            codec: VideoCodec::H264(H264 {
                profile: 0x42,
                constraints: 0xE0,
                level: 0x1E,
                inline: false,
            }),
            coded_width: Some(320),
            coded_height: Some(180),
            description: None,
            display_ratio_width: None,
            display_ratio_height: None,
            bitrate: None,
            framerate: None,
            optimize_for_latency: None,
        };
        let decode_config = DecodeConfig::default();
        let result = Av1VideoDecoder::new(&config, &decode_config);
        assert!(result.is_err());
    }
}
