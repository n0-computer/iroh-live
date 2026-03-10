use anyhow::{Result, bail};
use hang::catalog::{AudioCodec, AudioConfig};
use unsafe_libopus::{self as opus, OPUS_OK, OpusDecoder as RawOpusDecoder};

use crate::format::{AudioFormat, MediaPacket};
use crate::processing::resample::Resampler;
use crate::traits::AudioDecoder;

/// Maximum Opus frame size: 120ms at 48kHz = 5760 samples per channel.
const MAX_FRAME_SIZE: usize = 5760;

#[derive(derive_more::Debug)]
pub struct OpusAudioDecoder {
    #[debug(skip)]
    decoder: *mut RawOpusDecoder,
    /// Channel count of the encoded stream (source).
    channel_count: u32,
    /// Channel count of the output (target sink).
    target_channel_count: u32,
    #[debug(skip)]
    resampler: Resampler,
    /// Decoded + resampled + channel-converted sample buffer.
    samples: Vec<f32>,
    /// Reusable PCM decode buffer.
    pcm_buf: Vec<f32>,
}

// Safety: The OpusDecoder pointer is only used from a single thread.
unsafe impl Send for OpusAudioDecoder {}

impl Drop for OpusAudioDecoder {
    fn drop(&mut self) {
        unsafe {
            opus::opus_decoder_destroy(self.decoder);
        }
    }
}

impl AudioDecoder for OpusAudioDecoder {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self> {
        if !matches!(&config.codec, AudioCodec::Opus) {
            bail!(
                "Unsupported codec {} (only opus is supported)",
                config.codec
            );
        }

        let channel_count = config.channel_count;
        let mut error: i32 = 0;
        let decoder = unsafe {
            opus::opus_decoder_create(config.sample_rate as i32, channel_count as i32, &mut error)
        };
        if error != OPUS_OK || decoder.is_null() {
            bail!("opus_decoder_create failed with error {error}");
        }

        let resampler =
            Resampler::new(config.sample_rate, target_format.sample_rate, channel_count)?;

        let max_samples = MAX_FRAME_SIZE * channel_count as usize;
        Ok(Self {
            decoder,
            channel_count,
            target_channel_count: target_format.channel_count,
            resampler,
            samples: Vec::new(),
            pcm_buf: vec![0f32; max_samples],
        })
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        let decoded_samples = unsafe {
            opus::opus_decode_float(
                self.decoder,
                payload.as_ptr(),
                payload.len() as i32,
                self.pcm_buf.as_mut_ptr(),
                MAX_FRAME_SIZE as i32,
                0, // no FEC
            )
        };
        if decoded_samples < 0 {
            bail!("opus_decode_float failed: {decoded_samples}");
        }

        let total_samples = decoded_samples as usize * self.channel_count as usize;
        let pcm = &self.pcm_buf[..total_samples];

        // Resample if needed (e.g., 48kHz → target rate)
        let resampled = self.resampler.process(pcm)?;

        // Convert channel count if source and target differ
        self.samples.clear();
        if self.channel_count == self.target_channel_count {
            self.samples.extend_from_slice(&resampled);
        } else {
            convert_channels_into(
                &resampled,
                self.channel_count,
                self.target_channel_count,
                &mut self.samples,
            );
        }

        Ok(())
    }

    fn pop_samples(&mut self) -> Result<Option<&[f32]>> {
        if self.samples.is_empty() {
            Ok(None)
        } else {
            Ok(Some(&self.samples))
        }
    }
}

/// Convert interleaved audio between channel counts, writing into `out`.
///
/// - Same count: copies input directly.
/// - Mono→Stereo: duplicates each sample to both channels.
/// - Stereo→Mono: averages L and R for each frame.
/// - Other combinations: mixes down to mono, then upmixes by duplication.
fn convert_channels_into(samples: &[f32], from_ch: u32, to_ch: u32, out: &mut Vec<f32>) {
    if from_ch == to_ch {
        out.extend_from_slice(samples);
        return;
    }

    let from = from_ch as usize;
    let to = to_ch as usize;
    let frames = samples.len() / from;

    out.reserve(frames * to);
    match (from, to) {
        (1, 2) => {
            for &s in samples {
                out.push(s);
                out.push(s);
            }
        }
        (2, 1) => {
            for pair in samples.chunks_exact(2) {
                out.push((pair[0] + pair[1]) * 0.5);
            }
        }
        (_, 1) => {
            for frame in samples.chunks_exact(from) {
                let sum: f32 = frame.iter().sum();
                out.push(sum / from as f32);
            }
        }
        (1, _) => {
            for &s in samples {
                for _ in 0..to {
                    out.push(s);
                }
            }
        }
        _ => {
            // General: mix down to mono, then upmix
            // Use a temp buffer for the mono intermediate.
            let mono_len = frames;
            let start = out.len();
            out.reserve(mono_len);
            for frame in samples.chunks_exact(from) {
                let sum: f32 = frame.iter().sum();
                out.push(sum / from as f32);
            }
            let mono: Vec<f32> = out.drain(start..).collect();
            convert_channels_into(&mono, 1, to_ch, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use hang::catalog::AudioCodec;

    use super::*;
    use crate::{
        codec::opus::encoder::OpusEncoder,
        format::AudioPreset,
        traits::{AudioEncoder, AudioEncoderFactory},
        util::encoded_frames_to_media_packets,
    };

    fn make_sine(num_samples: usize, freq: f32, sample_rate: f32) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    fn encode_frames(enc: &mut OpusEncoder, samples: &[f32]) -> Vec<MediaPacket> {
        enc.push_samples(samples).unwrap();
        let mut packets = Vec::new();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
        encoded_frames_to_media_packets(packets)
    }

    #[test]
    fn decode_single_packet() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(960, 440.0, 48000.0);
        let packets = encode_frames(&mut enc, &sine);
        assert_eq!(packets.len(), 1);

        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let samples = dec.pop_samples().unwrap().unwrap();
        // Should get 960 mono samples back
        assert_eq!(samples.len(), 960);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        // Encode 5 frames
        let sine = make_sine(960 * 5, 440.0, 48000.0);
        let packets = encode_frames(&mut enc, &sine);
        assert_eq!(packets.len(), 5);

        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        let mut total_decoded = 0;
        for pkt in packets {
            dec.push_packet(pkt).unwrap();
            if let Some(samples) = dec.pop_samples().unwrap() {
                total_decoded += samples.len();
            }
        }
        assert_eq!(total_decoded, 960 * 5);
    }

    #[test]
    fn stereo_roundtrip() {
        let format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        // Stereo: 960 frames * 2 channels
        let samples = make_sine(960 * 2, 440.0, 48000.0);
        let packets = encode_frames(&mut enc, &samples);
        assert_eq!(packets.len(), 1);

        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let decoded = dec.pop_samples().unwrap().unwrap();
        // 960 frames * 2 channels = 1920 samples
        assert_eq!(decoded.len(), 960 * 2);
    }

    #[test]
    fn pop_samples_empty_before_push() {
        let config = AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channel_count: 1,
            bitrate: Some(128000),
            description: None,
            container: hang::catalog::Container::Legacy,
            jitter: None,
        };
        let format = AudioFormat::mono_48k();
        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        assert!(dec.pop_samples().unwrap().is_none());
    }

    #[test]
    fn corrupt_packet() {
        let config = AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channel_count: 1,
            bitrate: Some(128000),
            description: None,
            container: hang::catalog::Container::Legacy,
            jitter: None,
        };
        let format = AudioFormat::mono_48k();
        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        let bad_packet = MediaPacket {
            payload: bytes::Bytes::from_static(&[0xFF, 0xFF, 0xFF, 0xFF]).into(),
            timestamp: std::time::Duration::ZERO,
            is_keyframe: false,
        };
        // Corrupt packet should error, not panic
        let result = dec.push_packet(bad_packet);
        assert!(result.is_err());
    }

    #[test]
    fn decoded_output_non_silent() {
        let format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(960, 440.0, 48000.0);
        let packets = encode_frames(&mut enc, &sine);

        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let samples = dec.pop_samples().unwrap().unwrap();
        // Decoded 440Hz sine should not be all zeros
        let energy: f32 = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
        assert!(energy > 0.01, "decoded signal energy {energy} is too low");
    }

    #[test]
    fn mono_to_stereo_upmix() {
        // Encode mono, decode to stereo target (the typical real-world path)
        let enc_format = AudioFormat::mono_48k();
        let dec_format = AudioFormat::stereo_48k();
        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(960, 440.0, 48000.0);
        let packets = encode_frames(&mut enc, &sine);
        assert_eq!(packets.len(), 1);

        let mut dec = OpusAudioDecoder::new(&config, dec_format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let samples = dec.pop_samples().unwrap().unwrap();
        // 960 frames * 2 channels = 1920 interleaved samples
        assert_eq!(samples.len(), 960 * 2);
        // Each stereo pair should be identical (mono duplicated)
        for pair in samples.chunks_exact(2) {
            assert_eq!(
                pair[0], pair[1],
                "stereo pair should be equal for mono upmix"
            );
        }
    }

    #[test]
    fn stereo_to_mono_downmix() {
        let enc_format = AudioFormat::stereo_48k();
        let dec_format = AudioFormat::mono_48k();
        let mut enc = OpusEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        // 960 frames * 2 channels
        let samples = make_sine(960 * 2, 440.0, 48000.0);
        let packets = encode_frames(&mut enc, &samples);
        assert_eq!(packets.len(), 1);

        let mut dec = OpusAudioDecoder::new(&config, dec_format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let decoded = dec.pop_samples().unwrap().unwrap();
        // Should be 960 mono samples
        assert_eq!(decoded.len(), 960);
    }

    #[test]
    fn convert_channels_identity() {
        let input = vec![1.0, 2.0, 3.0];
        let mut output = Vec::new();
        super::convert_channels_into(&input, 1, 1, &mut output);
        assert_eq!(output, input);
    }

    #[test]
    fn convert_channels_mono_to_stereo() {
        let input = vec![1.0, 2.0, 3.0];
        let mut output = Vec::new();
        super::convert_channels_into(&input, 1, 2, &mut output);
        assert_eq!(output, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn convert_channels_stereo_to_mono() {
        let input = vec![1.0, 3.0, 2.0, 4.0];
        let mut output = Vec::new();
        super::convert_channels_into(&input, 2, 1, &mut output);
        assert_eq!(output, vec![2.0, 3.0]);
    }

    #[test]
    fn convert_channels_mono_to_quad() {
        let input = vec![1.0, 2.0];
        let mut output = Vec::new();
        super::convert_channels_into(&input, 1, 4, &mut output);
        assert_eq!(output, vec![1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn convert_channels_quad_to_mono() {
        // 2 frames of 4 channels: average each frame
        let input = vec![1.0, 2.0, 3.0, 4.0, 8.0, 8.0, 8.0, 8.0];
        let mut output = Vec::new();
        super::convert_channels_into(&input, 4, 1, &mut output);
        assert_eq!(output, vec![2.5, 8.0]);
    }

    #[test]
    fn convert_channels_general_n_to_m() {
        // 3ch → 2ch: goes through mono intermediate, then upmix
        let input = vec![3.0, 6.0, 9.0]; // 1 frame of 3 channels → mono avg = 6.0
        let mut output = Vec::new();
        super::convert_channels_into(&input, 3, 2, &mut output);
        // Mono 6.0 → stereo [6.0, 6.0]
        assert_eq!(output, vec![6.0, 6.0]);
    }

    #[test]
    fn convert_channels_reuses_output_capacity() {
        let mut output = Vec::with_capacity(100);
        let input = vec![1.0, 2.0, 3.0];
        super::convert_channels_into(&input, 1, 2, &mut output);
        assert_eq!(output, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        // Capacity should be at least what we reserved
        assert!(output.capacity() >= 100);
    }
}
