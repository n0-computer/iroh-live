use anyhow::Result;
use ffmpeg_next::{self as ffmpeg, util::channel_layout::ChannelLayout};
use hang::catalog::AudioConfig;

use crate::{
    av::{AudioDecoder, AudioFormat},
    ffmpeg_ext::CodecContextExt,
};

pub struct FfmpegAudioDecoder {
    codec: ffmpeg::decoder::Audio,
    resampler: ffmpeg::software::resampling::Context,
    decoded_frame: ffmpeg::util::frame::Audio,
    resampled_frame: ffmpeg::util::frame::Audio,
}

impl AudioDecoder for FfmpegAudioDecoder {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized,
    {
        let codec = match config.codec {
            hang::catalog::AudioCodec::Opus => {
                let codec_id = ffmpeg::codec::Id::OPUS;
                let codec = ffmpeg::decoder::find(codec_id).unwrap();
                let mut ctx = ffmpeg::codec::Context::new_with_codec(codec)
                    .decoder()
                    .audio()?;
                if let Some(extradata) = &config.description {
                    ctx.set_extradata(&extradata)?;
                }
                ctx.set_channel_layout(if config.channel_count == 1 {
                    ChannelLayout::MONO
                } else {
                    ChannelLayout::STEREO
                });
                unsafe {
                    let ctx_mut = ctx.as_mut_ptr();
                    (*ctx_mut).sample_rate = config.sample_rate as i32;
                }
                ctx
            }
            _ => anyhow::bail!(
                "Unsupported codec {} (only aac and opus are supported)",
                config.codec
            ),
        };
        let target_channel_layout = match target_format.channel_count {
            1 => ChannelLayout::MONO,
            2 => ChannelLayout::STEREO,
            _ => anyhow::bail!("unsupported target channel count"),
        };
        let target_sample_format = ffmpeg_next::util::format::sample::Sample::F32(
            ffmpeg_next::util::format::sample::Type::Packed,
        );
        let resampler = ffmpeg::software::resampling::Context::get(
            codec.format(),
            codec.channel_layout(),
            codec.rate(),
            target_sample_format,
            target_channel_layout,
            target_format.sample_rate,
        )?;
        Ok(Self {
            codec,
            resampler,
            decoded_frame: ffmpeg::util::frame::Audio::empty(),
            resampled_frame: ffmpeg::util::frame::Audio::empty(),
        })
    }

    fn push_packet(&mut self, packet: hang::Frame) -> Result<()> {
        let packet = ffmpeg::Packet::borrow(&packet.payload);
        self.codec.send_packet(&packet)?;
        Ok(())
    }

    fn pop_samples(&mut self) -> Result<Option<&[f32]>> {
        match self.codec.receive_frame(&mut self.decoded_frame) {
            Err(err) => Err(err.into()),
            Ok(()) => {
                // Create an empty frame to hold the resampled audio data.
                self.resampler
                    .run(&self.decoded_frame, &mut self.resampled_frame)
                    .unwrap();
                let frame = &self.resampled_frame;
                let expected_bytes =
                    frame.samples() * frame.channels() as usize * core::mem::size_of::<f32>();
                Ok(Some(bytemuck::cast_slice(&frame.data(0)[..expected_bytes])))
            }
        }
    }
}
