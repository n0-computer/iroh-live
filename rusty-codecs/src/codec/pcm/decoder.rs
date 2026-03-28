use anyhow::{Result, bail};

use crate::{
    config::{AudioCodec, AudioConfig},
    format::{AudioFormat, MediaPacket},
    processing::resample::Resampler,
    traits::AudioDecoder,
};

/// PCM audio decoder: unwraps raw little-endian f32 samples from
/// [`MediaPacket`] payloads. Handles resampling and channel conversion
/// if the source and target formats differ.
#[derive(derive_more::Debug)]
pub struct PcmAudioDecoder {
    /// Channel count of the encoded stream (source).
    channel_count: u32,
    /// Channel count of the output (target sink).
    target_channel_count: u32,
    #[debug(skip)]
    resampler: Resampler,
    /// Decoded + resampled + channel-converted sample buffer.
    samples: Vec<f32>,
    /// Secondary buffer for pop_samples — holds samples while the borrow is live.
    consumed_buf: Vec<f32>,
}

impl AudioDecoder for PcmAudioDecoder {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self> {
        if !matches!(&config.codec, AudioCodec::Pcm) {
            bail!(
                "PcmAudioDecoder: unsupported codec {} (expected pcm)",
                config.codec
            );
        }

        let resampler = Resampler::new(
            config.sample_rate,
            target_format.sample_rate,
            config.channel_count,
        )?;

        Ok(Self {
            channel_count: config.channel_count,
            target_channel_count: target_format.channel_count,
            resampler,
            samples: Vec::new(),
            consumed_buf: Vec::new(),
        })
    }

    fn push_packet(&mut self, mut packet: MediaPacket) -> Result<()> {
        use bytes::Buf;
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());

        if !payload.len().is_multiple_of(4) {
            bail!(
                "PCM packet payload length {} is not a multiple of 4 (f32 samples)",
                payload.len()
            );
        }

        // Deserialize little-endian f32 samples.
        let pcm: Vec<f32> = payload
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        // Resample if needed.
        let resampled = self.resampler.process(&pcm)?;

        // Convert channel count if source and target differ.
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
            std::mem::swap(&mut self.samples, &mut self.consumed_buf);
            self.samples.clear();
            Ok(Some(&self.consumed_buf))
        }
    }
}

/// Convert interleaved audio between channel counts, writing into `out`.
///
/// Reuses the same logic as the Opus decoder's channel conversion:
/// - Same count: copies input directly.
/// - Mono to Stereo: duplicates each sample to both channels.
/// - Stereo to Mono: averages L and R for each frame.
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
            // General: mix down to mono, then upmix.
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
    use super::*;
    use crate::{
        codec::{
            pcm::encoder::PcmEncoder,
            test_util::{encoded_frames_to_media_packets, make_sine},
        },
        format::AudioPreset,
        traits::{AudioEncoder, AudioEncoderFactory},
    };

    fn encode_frames(enc: &mut PcmEncoder, samples: &[f32]) -> Vec<MediaPacket> {
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
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(440.0, 48000, 960);
        let packets = encode_frames(&mut enc, &sine);
        assert_eq!(packets.len(), 1);

        let mut dec = PcmAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let samples = dec.pop_samples().unwrap().unwrap();
        assert_eq!(samples.len(), 960);
    }

    #[test]
    fn lossless_roundtrip() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(440.0, 48000, 960);
        let packets = encode_frames(&mut enc, &sine);

        let mut dec = PcmAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let decoded = dec.pop_samples().unwrap().unwrap();

        // PCM should be perfectly lossless.
        for (i, (&orig, &dec_sample)) in sine.iter().zip(decoded.iter()).enumerate() {
            assert!(
                (orig - dec_sample).abs() < 1e-7,
                "sample {i} mismatch: {orig} vs {dec_sample}"
            );
        }
    }

    #[test]
    fn stereo_roundtrip() {
        let format = AudioFormat::stereo_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let samples = make_sine(440.0, 48000, 960 * 2);
        let packets = encode_frames(&mut enc, &samples);
        assert_eq!(packets.len(), 1);

        let mut dec = PcmAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let decoded = dec.pop_samples().unwrap().unwrap();
        assert_eq!(decoded.len(), 960 * 2);
    }

    #[test]
    fn mono_to_stereo_upmix() {
        let enc_format = AudioFormat::mono_48k();
        let dec_format = AudioFormat::stereo_48k();
        let mut enc = PcmEncoder::with_preset(enc_format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(440.0, 48000, 960);
        let packets = encode_frames(&mut enc, &sine);

        let mut dec = PcmAudioDecoder::new(&config, dec_format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();
        let samples = dec.pop_samples().unwrap().unwrap();
        assert_eq!(samples.len(), 960 * 2);
        for pair in samples.chunks_exact(2) {
            assert_eq!(
                pair[0], pair[1],
                "stereo pair should be equal for mono upmix"
            );
        }
    }

    #[test]
    fn pop_samples_consumed_on_second_call() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();

        let sine = make_sine(440.0, 48000, 960);
        let packets = encode_frames(&mut enc, &sine);

        let mut dec = PcmAudioDecoder::new(&config, format).unwrap();
        dec.push_packet(packets.into_iter().next().unwrap())
            .unwrap();

        let first = dec.pop_samples().unwrap();
        assert!(first.is_some(), "first pop should return samples");

        let second = dec.pop_samples().unwrap();
        assert!(second.is_none(), "second pop should return None (consumed)");
    }

    #[test]
    fn rejects_wrong_codec() {
        let config = AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channel_count: 1,
            bitrate: Some(128000),
            description: None,
        };
        let format = AudioFormat::mono_48k();
        let result = PcmAudioDecoder::new(&config, format);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_malformed_payload() {
        let config = AudioConfig {
            codec: AudioCodec::Pcm,
            sample_rate: 48000,
            channel_count: 1,
            bitrate: None,
            description: None,
        };
        let format = AudioFormat::mono_48k();
        let mut dec = PcmAudioDecoder::new(&config, format).unwrap();
        // 5 bytes is not a multiple of 4.
        let bad_packet = MediaPacket {
            payload: bytes::Bytes::from_static(&[0u8; 5]).into(),
            timestamp: std::time::Duration::ZERO,
            is_keyframe: true,
        };
        assert!(dec.push_packet(bad_packet).is_err());
    }
}
