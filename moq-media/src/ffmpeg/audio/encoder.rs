use anyhow::{Context, Result};
use ffmpeg_next::{self as ffmpeg, Rational};
use hang::{Timestamp, catalog::AudioConfig};
use tracing::trace;

use crate::{
    av::{AudioEncoder, AudioEncoderInner, AudioFormat, AudioPreset},
    ffmpeg::ext::CodecContextExt,
};

const SAMPLE_RATE: u32 = 48_000;
const BITRATE: u64 = 128_000; // 128 kbps

pub struct OpusEncoder {
    encoder: ffmpeg::encoder::Audio,
    frame_count: u64,
    sample_rate: u32,
    bitrate: u64,
    channel_count: u32,
    extradata: Vec<u8>,
}

impl OpusEncoder {
    pub fn stereo() -> Result<Self> {
        Self::new(SAMPLE_RATE, 2, BITRATE)
    }

    pub fn mono() -> Result<Self> {
        Self::new(SAMPLE_RATE, 1, BITRATE)
    }

    pub fn new(sample_rate: u32, channel_count: u32, bitrate: u64) -> Result<Self> {
        tracing::info!(
            "Initializing Opus encoder: {}Hz, {} channels",
            sample_rate,
            channel_count
        );
        ffmpeg::init()?;

        let codec =
            ffmpeg::encoder::find(ffmpeg::codec::Id::OPUS).context("Opus encoder not found")?;
        tracing::debug!("Found Opus codec: {:?}", codec.name());
        let mut ctx = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()?;

        let sample_rate = sample_rate as i32;
        ctx.set_rate(sample_rate);
        ctx.set_bit_rate(bitrate as usize);
        ctx.set_format(ffmpeg::format::Sample::F32(
            ffmpeg_next::format::sample::Type::Packed,
        ));
        ctx.set_time_base(Rational::new(1, sample_rate));
        ctx.set_channel_layout(if channel_count == 1 {
            ffmpeg::util::channel_layout::ChannelLayout::MONO
        } else {
            ffmpeg::util::channel_layout::ChannelLayout::STEREO
        });

        let encoder = ctx.open()?;

        let extradata = encoder.extradata().unwrap_or(&[]).to_vec();

        tracing::info!("Opus encoder initialized successfully");
        Ok(Self {
            encoder,
            frame_count: 0,
            sample_rate: sample_rate as u32,
            channel_count,
            extradata,
            bitrate,
        })
    }
}
impl AudioEncoder for OpusEncoder {
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self>
    where
        Self: Sized,
    {
        let channel_count = format.channel_count;
        let bitrate = match preset {
            AudioPreset::Hq => BITRATE,
            AudioPreset::Lq => 32_000,
        };
        Self::new(SAMPLE_RATE, channel_count, bitrate)
    }
}

impl AudioEncoderInner for OpusEncoder {
    fn name(&self) -> &str {
        self.encoder.id().name()
    }

    fn config(&self) -> AudioConfig {
        hang::catalog::AudioConfig {
            codec: hang::catalog::AudioCodec::Opus,
            sample_rate: self.sample_rate,
            channel_count: self.channel_count,
            bitrate: Some(self.bitrate),
            description: Some(self.extradata.clone().into()),
        }
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }

        let samples_per_channel = samples.len() / self.channel_count as usize;
        debug_assert_eq!(samples_per_channel as u32, self.encoder.frame_size());

        let mut audio_frame = ffmpeg::util::frame::Audio::new(
            ffmpeg::util::format::sample::Sample::F32(ffmpeg::util::format::sample::Type::Packed),
            samples_per_channel,
            ffmpeg::util::channel_layout::ChannelLayout::default(self.channel_count as i32),
        );

        // Copy interleaved samples directly since we're using packed format
        let frame_data = audio_frame.data_mut(0);
        let frame_samples: &mut [f32] = bytemuck::cast_slice_mut(frame_data);

        let copy_len = samples.len().min(frame_samples.len());
        frame_samples[..copy_len].copy_from_slice(&samples[..copy_len]);

        audio_frame.set_pts(Some(self.frame_count as i64));
        self.frame_count += samples_per_channel as u64;

        trace!("push samples {}", audio_frame.samples());
        self.encoder.send_frame(&audio_frame)?;
        Ok(())
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        let mut packet = ffmpeg::packet::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let payload = packet.data().unwrap_or(&[]).to_vec();
                let hang_frame = hang::Frame {
                    payload: payload.into(),
                    timestamp: Timestamp::from_micros(
                        (self.frame_count * 1_000_000) / self.sample_rate as u64,
                    )?,
                    keyframe: true, // Audio frames are generally independent
                };
                trace!("poll frame {}", hang_frame.payload.num_bytes());
                Ok(Some(hang_frame))
            }
            Err(ffmpeg::Error::Eof) => Ok(None),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
