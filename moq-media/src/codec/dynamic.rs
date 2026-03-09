use anyhow::{Result, bail};
use hang::catalog::{AudioCodec, AudioConfig, VideoCodec, VideoConfig};

use crate::{
    format::{AudioFormat, DecodeConfig, DecodedVideoFrame, MediaPacket},
    traits::{AudioDecoder, Decoders, VideoDecoder},
};

#[derive(Debug, Clone, Copy)]
pub struct DefaultDecoders;

impl Decoders for DefaultDecoders {
    type Audio = DynamicAudioDecoder;
    type Video = DynamicVideoDecoder;
}

/// A video decoder that dispatches to the appropriate codec-specific decoder
/// based on the `VideoConfig::codec` field.
#[derive(Debug)]
#[non_exhaustive]
#[cfg(any_video_codec)]
pub enum DynamicVideoDecoder {
    #[cfg(feature = "h264")]
    H264(super::H264VideoDecoder),
    #[cfg(feature = "av1")]
    Av1(super::av1::Av1VideoDecoder),
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    VaapiH264(Box<super::vaapi::VaapiDecoder>),
}

#[cfg(any_video_codec)]
impl VideoDecoder for DynamicVideoDecoder {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized,
    {
        match &config.codec {
            #[cfg(feature = "h264")]
            VideoCodec::H264(_) => {
                #[cfg(all(target_os = "linux", feature = "vaapi"))]
                if matches!(playback_config.backend, crate::format::DecoderBackend::Auto)
                    && let Ok(dec) = super::vaapi::VaapiDecoder::new(config, playback_config)
                {
                    tracing::info!("using VAAPI hardware H.264 decoder");
                    return Ok(Self::VaapiH264(Box::new(dec)));
                }
                tracing::info!("using software H.264 decoder");
                Ok(Self::H264(super::H264VideoDecoder::new(
                    config,
                    playback_config,
                )?))
            }
            #[cfg(not(feature = "h264"))]
            VideoCodec::H264(_) => bail!("H.264 support requires the `h264` feature"),
            #[cfg(feature = "av1")]
            VideoCodec::AV1(_) => Ok(Self::Av1(super::av1::Av1VideoDecoder::new(
                config,
                playback_config,
            )?)),
            #[cfg(not(feature = "av1"))]
            VideoCodec::AV1(_) => bail!("AV1 support requires the `av1` feature"),
            other => bail!("Unsupported video codec: {other}"),
        }
    }

    fn name(&self) -> &str {
        match self {
            #[cfg(feature = "h264")]
            Self::H264(d) => d.name(),
            #[cfg(feature = "av1")]
            Self::Av1(d) => d.name(),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264(d) => d.name(),
            #[cfg(not(any(feature = "h264", feature = "av1")))]
            _ => unreachable!(),
        }
    }

    fn push_packet(&mut self, packet: MediaPacket) -> Result<()> {
        match self {
            #[cfg(feature = "h264")]
            Self::H264(d) => d.push_packet(packet),
            #[cfg(feature = "av1")]
            Self::Av1(d) => d.push_packet(packet),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264(d) => d.push_packet(packet),
            #[cfg(not(any(feature = "h264", feature = "av1")))]
            _ => unreachable!(),
        }
    }

    fn pop_frame(&mut self) -> Result<Option<DecodedVideoFrame>> {
        match self {
            #[cfg(feature = "h264")]
            Self::H264(d) => d.pop_frame(),
            #[cfg(feature = "av1")]
            Self::Av1(d) => d.pop_frame(),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264(d) => d.pop_frame(),
            #[cfg(not(any(feature = "h264", feature = "av1")))]
            _ => unreachable!(),
        }
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        match self {
            #[cfg(feature = "h264")]
            Self::H264(d) => d.set_viewport(w, h),
            #[cfg(feature = "av1")]
            Self::Av1(d) => d.set_viewport(w, h),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264(d) => d.set_viewport(w, h),
            #[cfg(not(any(feature = "h264", feature = "av1")))]
            _ => unreachable!(),
        }
    }
}

/// An audio decoder that dispatches to the appropriate codec-specific decoder
/// based on the `AudioConfig::codec` field.
#[cfg(any_audio_codec)]
#[derive(Debug)]
#[non_exhaustive]
pub enum DynamicAudioDecoder {
    #[cfg(feature = "opus")]
    Opus(super::OpusAudioDecoder),
}

#[cfg(any_audio_codec)]
impl AudioDecoder for DynamicAudioDecoder {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized,
    {
        match &config.codec {
            #[cfg(feature = "opus")]
            AudioCodec::Opus => Ok(Self::Opus(super::OpusAudioDecoder::new(
                config,
                target_format,
            )?)),
            #[cfg(not(feature = "opus"))]
            AudioCodec::Opus => bail!("Opus support requires the `opus` feature"),
            other => bail!("Unsupported audio codec: {other}"),
        }
    }

    fn push_packet(&mut self, packet: MediaPacket) -> Result<()> {
        match self {
            #[cfg(feature = "opus")]
            Self::Opus(d) => d.push_packet(packet),
            #[cfg(not(feature = "opus"))]
            _ => unreachable!(),
        }
    }

    fn pop_samples(&mut self) -> Result<Option<&[f32]>> {
        match self {
            #[cfg(feature = "opus")]
            Self::Opus(d) => d.pop_samples(),
            #[cfg(not(feature = "opus"))]
            _ => unreachable!(),
        }
    }
}
