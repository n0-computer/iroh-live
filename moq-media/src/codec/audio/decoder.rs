use anyhow::{Result, bail};
use bytes::Buf;
use hang::catalog::{AudioCodec, AudioConfig};
use unsafe_libopus::{self as opus, OPUS_OK, OpusDecoder as RawOpusDecoder};

use crate::av::{AudioDecoder, AudioFormat};
use crate::codec::resample::Resampler;

/// Maximum Opus frame size: 120ms at 48kHz = 5760 samples per channel.
const MAX_FRAME_SIZE: usize = 5760;

#[derive(derive_more::Debug)]
pub struct OpusAudioDecoder {
    #[debug(skip)]
    decoder: *mut RawOpusDecoder,
    channel_count: u32,
    #[debug(skip)]
    resampler: Resampler,
    /// Decoded + resampled sample buffer.
    samples: Vec<f32>,
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

        Ok(Self {
            decoder,
            channel_count,
            resampler,
            samples: Vec::new(),
        })
    }

    fn push_packet(&mut self, mut packet: hang::Frame) -> Result<()> {
        let payload = packet.payload.copy_to_bytes(packet.payload.remaining());
        let max_samples = MAX_FRAME_SIZE * self.channel_count as usize;
        let mut pcm = vec![0f32; max_samples];

        let decoded_samples = unsafe {
            opus::opus_decode_float(
                self.decoder,
                payload.as_ptr(),
                payload.len() as i32,
                pcm.as_mut_ptr(),
                MAX_FRAME_SIZE as i32,
                0, // no FEC
            )
        };
        if decoded_samples < 0 {
            bail!("opus_decode_float failed: {decoded_samples}");
        }

        let total_samples = decoded_samples as usize * self.channel_count as usize;
        pcm.truncate(total_samples);

        // Resample if needed (e.g., 48kHz → target rate)
        let resampled = self.resampler.process(&pcm)?;
        self.samples = resampled;

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

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use hang::catalog::AudioCodec;

    use super::*;
    use crate::av::{AudioEncoder, AudioEncoderInner, AudioPreset};
    use crate::codec::audio::encoder::OpusEncoder;

    fn make_sine(num_samples: usize, freq: f32, sample_rate: f32) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    fn encode_frames(enc: &mut OpusEncoder, samples: &[f32]) -> Vec<hang::Frame> {
        enc.push_samples(samples).unwrap();
        let mut packets = Vec::new();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
        packets
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
        };
        let format = AudioFormat::mono_48k();
        let mut dec = OpusAudioDecoder::new(&config, format).unwrap();
        let bad_packet = hang::Frame {
            payload: bytes::Bytes::from_static(&[0xFF, 0xFF, 0xFF, 0xFF]).into(),
            timestamp: hang::Timestamp::ZERO,
            keyframe: true,
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
}
