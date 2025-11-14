use std::{collections::VecDeque, time::Duration};

use crate::av::AudioPreset;
use anyhow::Result;
use hang::catalog::{AudioCodec, AudioConfig};

use crate::av as lav;

const DEFAULT_BITRATE: u32 = 128_000; // 128 kbps

pub struct OpusEncoder {
    enc: opus::Encoder,
    sample_rate: u32,
    channel_count: u32,
    bitrate: u32,
    queued: VecDeque<hang::Frame>,
    frame_count: u64,
}

impl OpusEncoder {
    pub fn stereo() -> Result<Self> {
        Self::new(48_000, 2)
    }
    pub fn mono() -> Result<Self> {
        Self::new(48_000, 1)
    }

    pub fn new(sample_rate: u32, channel_count: u32) -> Result<Self> {
        let channels = match channel_count {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            n => anyhow::bail!("unsupported opus channel count: {n}"),
        };
        let mut enc = opus::Encoder::new(sample_rate, channels, opus::Application::Audio)?;
        enc.set_bitrate(opus::Bitrate::Bits(DEFAULT_BITRATE as i32))?;
        Ok(Self {
            enc,
            sample_rate,
            channel_count,
            bitrate: DEFAULT_BITRATE,
            queued: VecDeque::new(),
            frame_count: 0,
        })
    }
}

impl lav::AudioEncoder for OpusEncoder {
    fn with_preset(preset: AudioPreset) -> Result<Self>
    where
        Self: Sized,
    {
        let channels = match preset {
            AudioPreset::Hq => 2,
            AudioPreset::Lq => 1,
        };
        let mut enc = Self::new(48_000, channels)?;
        enc.bitrate = match preset {
            AudioPreset::Hq => 160_000,
            AudioPreset::Lq => 32_000,
        };
        enc.enc
            .set_bitrate(opus::Bitrate::Bits(enc.bitrate as i32))?;
        Ok(enc)
    }

    fn config(&self) -> AudioConfig {
        AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            bitrate: Some(self.bitrate as u64),
            description: None,
        }
    }

    fn push_samples(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }

        // frame_size = samples per channel
        let frame_size = (samples.len() / self.channel_count as usize) as i32;
        // Allocate output buffer; worst-case a couple of KB is enough for opus
        let mut out = vec![0u8; 4096];
        let n = self.enc.encode_float(samples, &mut out)?;
        out.truncate(n);

        let ts = Duration::from_nanos(
            (self.frame_count as u64) * 1_000_000_000u64 / (self.sample_rate as u64),
        );
        self.frame_count += frame_size as u64;

        self.queued.push_back(hang::Frame {
            payload: out.into(),
            timestamp: ts,
            keyframe: true,
        });
        Ok(())
    }

    fn pop_packet(&mut self) -> anyhow::Result<Option<hang::Frame>> {
        Ok(self.queued.pop_front())
    }
}
