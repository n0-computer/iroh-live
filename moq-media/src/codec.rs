#[cfg(feature = "av1")]
pub(crate) mod av1;
#[cfg(any_codec)]
pub(crate) mod dynamic;
#[cfg(feature = "h264")]
pub(crate) mod h264;
#[cfg(feature = "opus")]
pub(crate) mod opus;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub(crate) mod vaapi;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub(crate) mod vtb;

#[cfg(feature = "h264")]
pub use self::h264::*;
#[cfg(feature = "opus")]
pub use self::opus::*;
#[cfg(feature = "av1")]
pub use av1::*;
#[cfg(any_codec)]
pub use dynamic::*;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub use vaapi::*;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub use vtb::*;

#[cfg(test)]
pub(crate) mod test_util;

/// Available audio encoder implementations.
#[cfg(any_audio_codec)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString, strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum AudioCodec {
    #[cfg(feature = "opus")]
    Opus,
}

/// Available video encoder implementations.
#[cfg(any_video_codec)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString, strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum VideoCodec {
    /// Software H.264 via openh264.
    #[cfg(feature = "h264")]
    H264,
    /// Software AV1 via rav1e.
    #[cfg(feature = "av1")]
    Av1,
    /// Hardware H.264 via macOS VideoToolbox.
    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    #[strum(serialize = "h264-vtb")]
    VtbH264,
    /// Hardware H.264 via Linux VAAPI.
    #[strum(serialize = "h264-vaapi")]
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    VaapiH264,
}

#[cfg(any_video_codec)]
impl VideoCodec {
    /// Returns all encoder kinds that are compiled in.
    pub fn available() -> Vec<Self> {
        vec![
            #[cfg(feature = "h264")]
            Self::H264,
            #[cfg(feature = "av1")]
            Self::Av1,
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264,
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264,
        ]
    }

    /// Returns the best available encoder: hardware if available, otherwise software H.264.
    #[allow(unreachable_code)]
    pub fn best_available() -> Self {
        #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
        {
            return Self::VtbH264;
        }
        #[cfg(all(target_os = "linux", feature = "vaapi"))]
        {
            return Self::VaapiH264;
        }
        #[cfg(feature = "h264")]
        {
            return Self::H264;
        }
        panic!("no video codec available: enable the h264, av1, videotoolbox, or vaapi feature")
    }

    /// Whether this is a hardware-accelerated encoder.
    pub fn is_hardware(self) -> bool {
        match self {
            #[cfg(feature = "h264")]
            Self::H264 => false,
            #[cfg(feature = "av1")]
            Self::Av1 => false,
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => true,
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => true,
        }
    }

    /// Creates an encoder for this codec with a full [`VideoEncoderConfig`](crate::format::VideoEncoderConfig).
    pub fn create_encoder(
        self,
        config: crate::format::VideoEncoderConfig,
    ) -> anyhow::Result<Box<dyn crate::traits::VideoEncoder>> {
        use crate::traits::VideoEncoderFactory as _;
        match self {
            #[cfg(feature = "h264")]
            Self::H264 => Ok(Box::new(H264Encoder::with_config(config)?)),
            #[cfg(feature = "av1")]
            Self::Av1 => Ok(Box::new(Av1Encoder::with_config(config)?)),
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => Ok(Box::new(VtbEncoder::with_config(config)?)),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => Ok(Box::new(VaapiEncoder::with_config(config)?)),
        }
    }

    /// Creates an encoder for this codec with a [`VideoPreset`](crate::format::VideoPreset).
    pub fn create_encoder_from_preset(
        self,
        preset: crate::format::VideoPreset,
    ) -> anyhow::Result<Box<dyn crate::traits::VideoEncoder>> {
        self.create_encoder(crate::format::VideoEncoderConfig::from_preset(preset))
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            #[cfg(feature = "h264")]
            Self::H264 => "H.264 (Software)",
            #[cfg(feature = "av1")]
            Self::Av1 => "AV1 (Software)",
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => "H.264 (VideoToolbox)",
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => "H.264 (VAAPI)",
        }
    }
}

#[cfg(all(test, feature = "opus"))]
mod tests {
    use crate::format::{
        AudioFormat, AudioPreset, DecodeConfig, EncodedFrame, MediaPacket, VideoPreset,
    };
    use crate::traits::{
        AudioDecoder, AudioEncoder, AudioEncoderFactory, Decoders, VideoDecoder, VideoEncoder,
        VideoEncoderFactory,
    };
    use crate::util::encoded_frames_to_media_packets;
    use hang::catalog::AudioConfig;
    use std::f32::consts::PI;

    use super::*;

    #[test]
    fn default_decoders_types() {
        fn assert_decoders<D: Decoders>() {}
        assert_decoders::<DefaultDecoders>();
    }

    // --- Audio roundtrip helpers ---

    fn make_sine(num_samples: usize, freq: f32, sample_rate: f32) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    /// Encode samples, return packets.
    fn audio_encode(enc: &mut OpusEncoder, samples: &[f32]) -> Vec<EncodedFrame> {
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
        packets: Vec<MediaPacket>,
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

    /// Assert that a decoded audio signal has preserved energy relative to the input.
    fn assert_energy_preserved(input: &[f32], output: &[f32]) {
        let input_rms = rms(input);
        let output_rms = rms(output);
        assert!(
            output_rms > 0.1,
            "decoded RMS {output_rms} too low (signal is near-silent)"
        );
        let ratio = output_rms / input_rms;
        assert!(
            (0.3..3.0).contains(&ratio),
            "energy ratio {ratio} out of range (input RMS={input_rms}, output RMS={output_rms})"
        );
    }

    /// Extract one channel from interleaved samples.
    fn extract_channel(interleaved: &[f32], channel: usize, num_channels: usize) -> Vec<f32> {
        interleaved
            .iter()
            .skip(channel)
            .step_by(num_channels)
            .copied()
            .collect()
    }

    // --- Video roundtrip tests for every preset ---

    #[cfg(feature = "h264")]
    #[test]
    fn video_roundtrip_p180_red() {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 255, 0, 0, 10);
        let config = enc.config();
        let frames = test_util::video_decode::<H264VideoDecoder>(&config, packets);
        test_util::assert_video_roundtrip(&frames, w, h, 255, 0, 0, 80, 5);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn video_roundtrip_p360_green() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 0, 255, 0, 10);
        let config = enc.config();
        let frames = test_util::video_decode::<H264VideoDecoder>(&config, packets);
        test_util::assert_video_roundtrip(&frames, w, h, 0, 255, 0, 80, 5);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn video_roundtrip_p720_blue() {
        let preset = VideoPreset::P720;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 0, 0, 255, 10);
        let config = enc.config();
        let frames = test_util::video_decode::<H264VideoDecoder>(&config, packets);
        test_util::assert_video_roundtrip(&frames, w, h, 0, 0, 255, 80, 5);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn video_roundtrip_p1080_white() {
        let preset = VideoPreset::P1080;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 255, 255, 255, 10);
        let config = enc.config();
        let frames = test_util::video_decode::<H264VideoDecoder>(&config, packets);
        test_util::assert_video_roundtrip(&frames, w, h, 255, 255, 255, 40, 5);
    }

    // --- AV1 video roundtrip tests ---

    #[cfg(feature = "av1")]
    #[test]
    fn av1_roundtrip_p180_red() {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = Av1Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 255, 0, 0, 60);
        let config = enc.config();
        let frames = test_util::video_decode::<Av1VideoDecoder>(&config, packets);
        test_util::assert_video_roundtrip(&frames, w, h, 255, 0, 0, 80, 5);
    }

    #[cfg(feature = "av1")]
    #[test]
    fn av1_roundtrip_p360_green() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let mut enc = Av1Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 0, 255, 0, 60);
        let config = enc.config();
        let frames = test_util::video_decode::<Av1VideoDecoder>(&config, packets);
        test_util::assert_video_roundtrip(&frames, w, h, 0, 255, 0, 80, 5);
    }

    // --- DynamicVideoDecoder routing tests ---

    #[cfg(feature = "h264")]
    #[test]
    fn dynamic_routes_h264() {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = H264Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 200, 100, 50, 10);
        let config = enc.config();

        let decode_config = DecodeConfig::default();
        let mut dec = DynamicVideoDecoder::new(&config, &decode_config).unwrap();
        assert_eq!(dec.name(), "h264-openh264");

        let mut decoded_count = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if dec.pop_frame().unwrap().is_some() {
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    }

    #[cfg(all(feature = "h264", feature = "av1"))]
    #[test]
    fn dynamic_routes_av1() {
        let preset = VideoPreset::P180;
        let (w, h) = preset.dimensions();
        let mut enc = Av1Encoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode(&mut enc, w, h, 200, 100, 50, 60);
        let config = enc.config();

        let decode_config = DecodeConfig::default();
        let mut dec = DynamicVideoDecoder::new(&config, &decode_config).unwrap();
        assert_eq!(dec.name(), "av1-rav1d");

        let mut decoded_count = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if dec.pop_frame().unwrap().is_some() {
                decoded_count += 1;
            }
        }
        assert!(
            decoded_count >= 5,
            "expected >= 5 decoded frames, got {decoded_count}"
        );
    }

    // --- Hardware encoder cross-codec roundtrip tests ---

    #[cfg(all(target_os = "macos", feature = "videotoolbox", feature = "h264"))]
    #[test]
    #[ignore]
    fn vtb_roundtrip_p360_red() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let mut enc = VtbEncoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode_pattern(&mut enc, w, h, 30);
        let config = enc.config();
        let frames = test_util::video_decode::<H264VideoDecoder>(&config, packets);
        test_util::assert_video_not_black(&frames, w, h, 5);
    }

    #[cfg(all(target_os = "linux", feature = "vaapi", feature = "h264"))]
    #[test]
    #[ignore = "requires VAAPI hardware"]
    fn vaapi_roundtrip_p360_red() {
        let preset = VideoPreset::P360;
        let (w, h) = preset.dimensions();
        let mut enc = VaapiEncoder::with_preset(preset).unwrap();
        let packets = test_util::video_encode_pattern(&mut enc, w, h, 30);
        let config = enc.config();
        let frames = test_util::video_decode::<H264VideoDecoder>(&config, packets);
        test_util::assert_video_not_black(&frames, w, h, 5);
    }

    // --- Audio roundtrip tests ---

    #[test]
    fn audio_roundtrip_mono_hq() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 48000);
        assert_energy_preserved(&sine, &decoded);
    }

    #[test]
    fn audio_roundtrip_mono_lq() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Lq).unwrap();
        let config = enc.config();
        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 48000);
        assert_energy_preserved(&sine, &decoded);
    }

    #[test]
    fn audio_roundtrip_stereo_hq() {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 96000);
        assert_energy_preserved(&sine, &decoded);
    }

    #[test]
    fn audio_roundtrip_stereo_lq() {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Lq).unwrap();
        let config = enc.config();
        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, format, packets);
        assert_eq!(decoded.len(), 96000);
        assert_energy_preserved(&sine, &decoded);
    }

    // --- Cross-channel audio pipeline tests ---

    #[test]
    fn audio_pipeline_mono_encode_stereo_decode() {
        let enc_format = AudioFormat::mono_48k();
        let dec_format = AudioFormat::stereo_48k();

        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        assert_eq!(config.channel_count, 1);

        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);

        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, dec_format, packets);

        assert_eq!(decoded.len(), 96000);

        for (i, pair) in decoded.chunks_exact(2).enumerate() {
            assert_eq!(
                pair[0], pair[1],
                "stereo pair at frame {i} should be equal: L={}, R={}",
                pair[0], pair[1]
            );
        }

        let left = extract_channel(&decoded, 0, 2);
        assert_energy_preserved(&sine, &left);
    }

    #[test]
    fn audio_pipeline_stereo_encode_mono_decode() {
        let enc_format = AudioFormat::stereo_48k();
        let dec_format = AudioFormat::mono_48k();

        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        assert_eq!(config.channel_count, 2);

        let sine = make_sine(96000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        assert_eq!(packets.len(), 50);

        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, dec_format, packets);

        assert_eq!(decoded.len(), 48000);

        let input_left = extract_channel(&sine, 0, 2);
        assert_energy_preserved(&input_left, &decoded);
    }

    #[test]
    fn audio_pipeline_mono_encode_stereo_decode_lq() {
        let enc_format = AudioFormat::mono_48k();
        let dec_format = AudioFormat::stereo_48k();

        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Lq).unwrap();
        let config = enc.config();

        let sine = make_sine(48000, 440.0, 48000.0);
        let packets = audio_encode(&mut enc, &sine);
        let packets = encoded_frames_to_media_packets(packets);
        let decoded = audio_decode(&config, dec_format, packets);

        assert_eq!(decoded.len(), 96000);
        let left = extract_channel(&decoded, 0, 2);
        assert_energy_preserved(&sine, &left);
    }
}
