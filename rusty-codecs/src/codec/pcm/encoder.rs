use std::{collections::VecDeque, time::Duration};

use anyhow::Result;

use crate::{
    config::{AudioCodec, AudioConfig},
    format::{AudioEncoderConfig, EncodedFrame},
    traits::{AudioEncoder, AudioEncoderFactory},
};

/// 20ms frame duration — matches Opus framing for consistent pipeline behavior.
const FRAME_DURATION_MS: u64 = 20;

/// PCM audio encoder: wraps raw f32 samples into [`EncodedFrame`] packets
/// with no compression. Each packet contains exactly one frame (20ms) of
/// interleaved little-endian f32 samples.
#[derive(Debug)]
pub struct PcmEncoder {
    sample_rate: u32,
    channel_count: u32,
    /// Samples per 20ms frame (per channel).
    frame_size: usize,
    /// Internal buffer accumulating samples until a full frame is ready.
    sample_buf: Vec<f32>,
    /// Number of complete frames emitted so far (for timestamp calculation).
    frame_count: u64,
    /// Encoded packets ready for collection.
    packet_buf: VecDeque<EncodedFrame>,
}

impl PcmEncoder {
    fn new(sample_rate: u32, channel_count: u32) -> Self {
        let frame_size = (sample_rate as u64 * FRAME_DURATION_MS / 1000) as usize;
        Self {
            sample_rate,
            channel_count,
            frame_size,
            sample_buf: Vec::new(),
            frame_count: 0,
            packet_buf: VecDeque::new(),
        }
    }

    /// Packs accumulated samples into 20ms frames.
    fn pack_pending(&mut self) {
        let samples_per_frame = self.frame_size * self.channel_count as usize;
        while self.sample_buf.len() >= samples_per_frame {
            // Serialize f32 samples as little-endian bytes.
            let mut payload = Vec::with_capacity(samples_per_frame * 4);
            for &sample in &self.sample_buf[..samples_per_frame] {
                payload.extend_from_slice(&sample.to_le_bytes());
            }

            let timestamp_us =
                self.frame_count * self.frame_size as u64 * 1_000_000 / self.sample_rate as u64;
            self.packet_buf.push_back(EncodedFrame {
                is_keyframe: true,
                timestamp: Duration::from_micros(timestamp_us),
                payload: payload.into(),
            });
            self.frame_count += 1;
            self.sample_buf.drain(..samples_per_frame);
        }
    }
}

impl AudioEncoderFactory for PcmEncoder {
    const ID: &str = "pcm";

    fn with_config(config: AudioEncoderConfig) -> Result<Self> {
        Ok(Self::new(config.sample_rate, config.channel_count))
    }

    fn config_for(config: &AudioEncoderConfig) -> AudioConfig {
        let bitrate = config.sample_rate as u64 * config.channel_count as u64 * 32;
        AudioConfig {
            codec: AudioCodec::Pcm,
            sample_rate: config.sample_rate,
            channel_count: config.channel_count,
            bitrate: Some(bitrate),
            description: None,
        }
    }
}

impl AudioEncoder for PcmEncoder {
    fn name(&self) -> &str {
        Self::ID
    }

    fn config(&self) -> AudioConfig {
        let bitrate = self.sample_rate as u64 * self.channel_count as u64 * 32;
        AudioConfig {
            codec: AudioCodec::Pcm,
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            bitrate: Some(bitrate),
            description: None,
        }
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        self.sample_buf.extend_from_slice(samples);
        self.pack_pending();
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
        codec::test_util::make_sine,
        format::{AudioFormat, AudioPreset},
        traits::AudioEncoderFactory,
    };

    #[test]
    fn encode_silence_mono() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        // 960 samples = 20ms at 48kHz mono
        let silence = vec![0.0f32; 960];
        enc.push_samples(&silence).unwrap();
        let packet = enc.pop_packet().unwrap();
        assert!(
            packet.is_some(),
            "should produce a packet for one full frame"
        );
        let pkt = packet.unwrap();
        assert!(pkt.is_keyframe);
        // 960 samples * 4 bytes each = 3840 bytes
        assert_eq!(pkt.payload.len(), 960 * 4);
    }

    #[test]
    fn encode_sine_mono() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let sine = make_sine(440.0, 48000, 960);
        enc.push_samples(&sine).unwrap();
        let packet = enc.pop_packet().unwrap().unwrap();
        assert_eq!(packet.payload.len(), 960 * 4);
    }

    #[test]
    fn sample_buffer_accumulation() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let half = vec![0.1f32; 480];
        enc.push_samples(&half).unwrap();
        assert!(enc.pop_packet().unwrap().is_none());
        enc.push_samples(&half).unwrap();
        assert!(enc.pop_packet().unwrap().is_some());
    }

    #[test]
    fn multiple_frames() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let samples = make_sine(440.0, 48000, 960 * 3);
        enc.push_samples(&samples).unwrap();
        for i in 0..3 {
            assert!(enc.pop_packet().unwrap().is_some(), "missing packet {i}");
        }
        assert!(enc.pop_packet().unwrap().is_none());
    }

    #[test]
    fn stereo_encoding() {
        let format = AudioFormat::stereo_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let samples = vec![0.05f32; 960 * 2];
        enc.push_samples(&samples).unwrap();
        let pkt = enc.pop_packet().unwrap().unwrap();
        // 960 frames * 2 channels * 4 bytes = 7680
        assert_eq!(pkt.payload.len(), 960 * 2 * 4);
    }

    #[test]
    fn timestamps_increase() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let samples = make_sine(440.0, 48000, 960 * 3);
        enc.push_samples(&samples).unwrap();
        let mut prev_ts = None;
        for _ in 0..3 {
            let pkt = enc.pop_packet().unwrap().unwrap();
            if let Some(prev) = prev_ts {
                assert!(pkt.timestamp > prev, "timestamps should increase");
            }
            prev_ts = Some(pkt.timestamp);
        }
    }

    #[test]
    fn bitrate_reflects_raw_samples() {
        let format = AudioFormat::stereo_48k();
        let enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let config = enc.config();
        // 48000 Hz * 2 ch * 32 bits = 3,072,000 bps
        assert_eq!(config.bitrate, Some(48_000 * 2 * 32));
    }

    #[test]
    fn payload_roundtrips_samples() {
        let format = AudioFormat::mono_48k();
        let mut enc = PcmEncoder::with_preset(format, AudioPreset::Hq).unwrap();
        let sine = make_sine(440.0, 48000, 960);
        enc.push_samples(&sine).unwrap();
        let pkt = enc.pop_packet().unwrap().unwrap();

        // Deserialize and compare to original.
        let decoded: Vec<f32> = pkt
            .payload
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(decoded.len(), sine.len());
        for (i, (&orig, &dec)) in sine.iter().zip(decoded.iter()).enumerate() {
            assert!(
                (orig - dec).abs() < 1e-7,
                "sample {i} mismatch: {orig} vs {dec}"
            );
        }
    }
}
