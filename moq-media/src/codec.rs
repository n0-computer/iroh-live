use crate::av::Decoders;

pub use self::{audio::*, video::*};

pub(crate) mod audio;
mod resample;
pub(crate) mod video;

#[derive(Debug, Clone, Copy)]
pub struct DefaultDecoders;

impl Decoders for DefaultDecoders {
    type Audio = OpusAudioDecoder;
    type Video = DynamicVideoDecoder;
}

/// No-op replacement for `ffmpeg_log_init`. Nothing to initialize.
pub fn codec_init() {}

#[cfg(test)]
mod tests {
    use crate::av::{
        AudioDecoder, AudioEncoder, AudioEncoderInner, AudioFormat, AudioPreset, DecodeConfig,
        DecodedFrame, Decoders, PixelFormat, VideoDecoder, VideoEncoder, VideoEncoderInner,
        VideoFormat, VideoFrame, VideoPreset,
    };
    use hang::catalog::{AudioConfig, VideoConfig};
    use std::f32::consts::PI;

    use super::*;

    #[test]
    fn default_decoders_types() {
        fn assert_decoders<D: Decoders>() {}
        assert_decoders::<DefaultDecoders>();
    }

    // --- Video roundtrip helpers ---

    fn make_solid_frame(w: u32, h: u32, r: u8, g: u8, b: u8) -> VideoFrame {
        let raw: Vec<u8> = [r, g, b, 255].repeat((w * h) as usize);
        VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [w, h],
            },
            raw: raw.into(),
        }
    }

    /// Encode `n` solid-color frames, return packets.
    fn video_encode(
        enc: &mut H264Encoder,
        w: u32,
        h: u32,
        r: u8,
        g: u8,
        b: u8,
        n: usize,
    ) -> Vec<hang::Frame> {
        let mut packets = Vec::new();
        for _ in 0..n {
            enc.push_frame(make_solid_frame(w, h, r, g, b)).unwrap();
            while let Some(pkt) = enc.pop_packet().unwrap() {
                packets.push(pkt);
            }
        }
        packets
    }

    /// Decode all packets, return decoded frames.
    fn video_decode(config: &VideoConfig, packets: Vec<hang::Frame>) -> Vec<DecodedFrame> {
        let decode_config = DecodeConfig::default();
        let mut dec = H264VideoDecoder::new(config, &decode_config).unwrap();
        let mut frames = Vec::new();
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(frame) = dec.pop_frame().unwrap() {
                frames.push(frame);
            }
        }
        frames
    }

    /// Assert decoded frames have correct dimensions and the center pixel
    /// approximately matches the expected color (within `tolerance`).
    #[allow(
        clippy::too_many_arguments,
        reason = "test helper with clear parameters"
    )]
    fn assert_video_roundtrip(
        frames: &[DecodedFrame],
        w: u32,
        h: u32,
        expected_r: u8,
        expected_g: u8,
        expected_b: u8,
        tolerance: u8,
        min_frames: usize,
    ) {
        assert!(
            frames.len() >= min_frames,
            "expected >= {min_frames} frames, got {}",
            frames.len()
        );
        // Check the last frame (encoder has stabilized by then)
        let last = frames.last().unwrap();
        let img = last.img();
        assert_eq!(img.width(), w);
        assert_eq!(img.height(), h);
        let pixel = img.get_pixel(w / 2, h / 2);
        assert!(
            pixel[0].abs_diff(expected_r) <= tolerance,
            "R: expected ~{expected_r}, got {} (tolerance {tolerance})",
            pixel[0]
        );
        assert!(
            pixel[1].abs_diff(expected_g) <= tolerance,
            "G: expected ~{expected_g}, got {} (tolerance {tolerance})",
            pixel[1]
        );
        assert!(
            pixel[2].abs_diff(expected_b) <= tolerance,
            "B: expected ~{expected_b}, got {} (tolerance {tolerance})",
            pixel[2]
        );
        assert_eq!(pixel[3], 255, "alpha should be 255");
    }

    // --- Audio roundtrip helpers ---

    fn make_sine(num_samples: usize, freq: f32, sample_rate: f32) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    /// Encode samples, return packets.
    fn audio_encode(enc: &mut OpusEncoder, samples: &[f32]) -> Vec<hang::Frame> {
        enc.push_samples(samples).unwrap();
        let mut packets = Vec::new();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
        packets
    }

    /// Decode all packets, return concatenated samples.
    fn audio_decode(
        config: &AudioConfig,
        format: AudioFormat,
        packets: Vec<hang::Frame>,
    ) -> Vec<f32> {
        let mut dec = OpusAudioDecoder::new(config, format).unwrap();
        let mut all_samples = Vec::new();
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(samples) = dec.pop_samples().unwrap() {
                all_samples.extend_from_slice(samples);
            }
        }
        all_samples
    }

    /// Compute RMS energy of a signal.
    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    // --- Video roundtrip tests for every preset ---

    #[test]
    fn video_roundtrip_p180_red() {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = video_encode(&mut enc, w, h, 255, 0, 0, 10);
        let config = enc.config();
        let frames = video_decode(&config, packets);
        assert_video_roundtrip(&frames, w, h, 255, 0, 0, 80, 5);
    }

    #[test]
    fn video_roundtrip_p360_green() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = video_encode(&mut enc, w, h, 0, 255, 0, 10);
        let config = enc.config();
        let frames = video_decode(&config, packets);
        assert_video_roundtrip(&frames, w, h, 0, 255, 0, 80, 5);
    }

    #[test]
    fn video_roundtrip_p720_blue() {
        let preset = VideoPreset::P720;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = video_encode(&mut enc, w, h, 0, 0, 255, 10);
        let config = enc.config();
        let frames = video_decode(&config, packets);
        assert_video_roundtrip(&frames, w, h, 0, 0, 255, 80, 5);
    }

    #[test]
    fn video_roundtrip_p1080_white() {
        let preset = VideoPreset::P1080;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = video_encode(&mut enc, w, h, 255, 255, 255, 10);
        let config = enc.config();
        let frames = video_decode(&config, packets);
        assert_video_roundtrip(&frames, w, h, 255, 255, 255, 40, 5);
    }

    // --- Audio roundtrip tests for every format × preset combination ---

    #[test]
    fn audio_roundtrip_mono_hq() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        let sine = make_sine(48000, 440.0, 48000.0); // 1 second
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 48000);
        let input_rms = rms(&sine);
        let output_rms = rms(&decoded);
        // Decoded sine should have significant energy (not silence)
        assert!(output_rms > 0.1, "mono HQ decoded RMS {output_rms} too low");
        // Output energy should be in the same ballpark as input (within 50%)
        let ratio = output_rms / input_rms;
        assert!(
            (0.5..2.0).contains(&ratio),
            "energy ratio {ratio} out of range (input RMS={input_rms}, output RMS={output_rms})"
        );
    }

    #[test]
    fn audio_roundtrip_mono_lq() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Lq).unwrap();
        let config = enc.config();
        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 48000);
        let energy = rms(&decoded);
        assert!(energy > 0.1, "mono LQ decoded RMS {energy} too low");
    }

    #[test]
    fn audio_roundtrip_stereo_hq() {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        // 1 second stereo: 48000 frames × 2 channels = 96000 samples
        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 96000);
        let energy = rms(&decoded);
        assert!(energy > 0.1, "stereo HQ decoded RMS {energy} too low");
    }

    #[test]
    fn audio_roundtrip_stereo_lq() {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Lq).unwrap();
        let config = enc.config();
        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 96000);
        let energy = rms(&decoded);
        assert!(energy > 0.1, "stereo LQ decoded RMS {energy} too low");
    }
}
