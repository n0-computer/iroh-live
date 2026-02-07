use anyhow::Result;
use ffmpeg_next::{
    codec,
    format::{Sample, sample},
    software::resampling,
    util::{channel_layout::ChannelLayout, frame},
};
use hang::catalog::{AudioCodec, AudioConfig};

use crate::{
    av::{AudioDecoder, AudioFormat},
    ffmpeg::ext::{CodecContextExt, PacketExt},
};

#[derive(derive_more::Debug)]
pub struct FfmpegAudioDecoder {
    #[debug(skip)]
    codec: codec::decoder::Audio,
    #[debug(skip)]
    resampler: resampling::Context,
    #[debug(skip)]
    decoded_frame: frame::Audio,
    #[debug(skip)]
    resampled_frame: frame::Audio,
}

impl AudioDecoder for FfmpegAudioDecoder {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized,
    {
        let codec = match config.codec {
            AudioCodec::Opus => {
                let codec_id = codec::Id::OPUS;
                let decoder = codec::decoder::find(codec_id).unwrap();
                let mut ctx = codec::context::Context::new_with_codec(decoder)
                    .decoder()
                    .audio()?;
                if let Some(extradata) = &config.description {
                    ctx.set_extradata(extradata)?;
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
                "Unsupported codec {} (only opus is supported)",
                config.codec
            ),
        };
        let target_channel_layout = match target_format.channel_count {
            1 => ChannelLayout::MONO,
            2 => ChannelLayout::STEREO,
            _ => anyhow::bail!("unsupported target channel count"),
        };
        let target_sample_format = Sample::F32(sample::Type::Packed);
        let resampler = resampling::Context::get(
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
            decoded_frame: frame::Audio::empty(),
            resampled_frame: frame::Audio::empty(),
        })
    }

    fn push_packet(&mut self, packet: hang::Frame) -> Result<()> {
        let packet = packet.payload.to_ffmpeg_packet();
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
                let expected_bytes = frame.samples() * frame.channels() as usize * size_of::<f32>();
                Ok(Some(bytemuck::cast_slice(&frame.data(0)[..expected_bytes])))
            }
        }
    }
}
