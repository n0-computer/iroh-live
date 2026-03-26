use anyhow::{Result, bail};

use crate::{
    config::{AudioCodec, AudioConfig, VideoCodec, VideoConfig},
    format::{AudioFormat, DecodeConfig, MediaPacket, VideoFrame},
    traits::{AudioDecoder, Decoders, VideoDecoder},
};

/// Decoder set that dispatches to the appropriate codec at runtime based on
/// the catalog's codec configuration. Always available regardless of which
/// codec features are enabled — `new()` returns an error if the required
/// codec feature is not compiled in.
#[derive(Debug, Clone, Copy)]
pub struct DefaultDecoders;

impl Decoders for DefaultDecoders {
    type Audio = DynamicAudioDecoder;
    type Video = DynamicVideoDecoder;
}

/// Generates forwarding match arms for all `DynamicVideoDecoder` variants.
///
/// Each call produces a match block that delegates `$method` to the inner
/// decoder. The cfg gates are baked in once here so adding a new backend
/// only requires updating this macro and the enum definition.
macro_rules! dispatch_video {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            #[cfg(feature = "h264")]
            Self::H264(d) => d.$method($($arg),*),
            #[cfg(feature = "av1")]
            Self::Av1(d) => d.$method($($arg),*),
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264(d) => d.$method($($arg),*),
            #[cfg(all(target_os = "linux", feature = "v4l2"))]
            Self::V4l2H264(d) => d.$method($($arg),*),
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264(d) => d.$method($($arg),*),
            #[cfg(all(target_os = "android", feature = "android"))]
            Self::AndroidHwH264(d) => d.$method($($arg),*),
            #[cfg(all(target_os = "android", feature = "android"))]
            Self::AndroidH264(d) => d.$method($($arg),*),
        }
    };
}

/// Video decoder that dispatches to the appropriate codec-specific decoder
/// based on the `VideoConfig::codec` field.
///
/// Always defined regardless of codec features. Without any video codec
/// features, the enum is empty and `new()` returns an error.
#[derive(Debug)]
#[non_exhaustive]
pub enum DynamicVideoDecoder {
    #[cfg(feature = "h264")]
    H264(Box<super::H264VideoDecoder>),
    #[cfg(feature = "av1")]
    Av1(Box<super::av1::Av1VideoDecoder>),
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    VaapiH264(Box<super::vaapi::VaapiDecoder>),
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    V4l2H264(Box<super::v4l2::V4l2Decoder>),
    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    VtbH264(Box<super::vtb::VtbDecoder>),
    #[cfg(all(target_os = "android", feature = "android"))]
    AndroidHwH264(Box<super::android::AndroidHwDecoder>),
    #[cfg(all(target_os = "android", feature = "android"))]
    AndroidH264(Box<super::android::AndroidDecoder>),
}

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
                #[cfg(all(target_os = "linux", feature = "v4l2"))]
                if matches!(playback_config.backend, crate::format::DecoderBackend::Auto)
                    && let Ok(dec) = super::v4l2::V4l2Decoder::new(config, playback_config)
                {
                    tracing::info!("using V4L2 hardware H.264 decoder");
                    return Ok(Self::V4l2H264(Box::new(dec)));
                }
                #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
                if matches!(playback_config.backend, crate::format::DecoderBackend::Auto)
                    && let Ok(dec) = super::vtb::VtbDecoder::new(config, playback_config)
                {
                    tracing::info!("using VideoToolbox hardware H.264 decoder");
                    return Ok(Self::VtbH264(Box::new(dec)));
                }
                #[cfg(all(target_os = "android", feature = "android"))]
                if matches!(playback_config.backend, crate::format::DecoderBackend::Auto) {
                    // Prefer zero-copy HW decoder; fall back to ByteBuffer decoder.
                    if let Ok(dec) = super::android::AndroidHwDecoder::new(config, playback_config)
                    {
                        tracing::info!("using Android MediaCodec HW decoder (ImageReader)");
                        return Ok(Self::AndroidHwH264(Box::new(dec)));
                    }
                    if let Ok(dec) = super::android::AndroidDecoder::new(config, playback_config) {
                        tracing::info!("using Android MediaCodec decoder (ByteBuffer)");
                        return Ok(Self::AndroidH264(Box::new(dec)));
                    }
                }
                tracing::info!("using software H.264 decoder");
                Ok(Self::H264(Box::new(super::H264VideoDecoder::new(
                    config,
                    playback_config,
                )?)))
            }
            #[cfg(not(feature = "h264"))]
            VideoCodec::H264(_) => bail!("H.264 support requires the `h264` feature"),
            #[cfg(feature = "av1")]
            VideoCodec::AV1(_) => Ok(Self::Av1(Box::new(super::av1::Av1VideoDecoder::new(
                config,
                playback_config,
            )?))),
            #[cfg(not(feature = "av1"))]
            VideoCodec::AV1(_) => bail!("AV1 support requires the `av1` feature"),
            other => bail!("Unsupported video codec: {other}"),
        }
    }

    fn name(&self) -> &str {
        dispatch_video!(self, name)
    }

    fn push_packet(&mut self, packet: MediaPacket) -> Result<()> {
        dispatch_video!(self, push_packet, packet)
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        dispatch_video!(self, pop_frame)
    }

    fn reset(&mut self) -> Result<()> {
        dispatch_video!(self, reset)
    }

    fn set_viewport(&mut self, w: u32, h: u32) {
        dispatch_video!(self, set_viewport, w, h)
    }

    fn burst_size(&self) -> usize {
        dispatch_video!(self, burst_size)
    }
}

/// Audio decoder that dispatches to the appropriate codec-specific decoder
/// based on the `AudioConfig::codec` field.
///
/// Always defined regardless of codec features. Without any audio codec
/// features, the enum is empty and `new()` returns an error.
#[derive(Debug)]
#[non_exhaustive]
pub enum DynamicAudioDecoder {
    #[cfg(feature = "opus")]
    Opus(super::OpusAudioDecoder),
}

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
        }
    }

    fn pop_samples(&mut self) -> Result<Option<&[f32]>> {
        match self {
            #[cfg(feature = "opus")]
            Self::Opus(d) => d.pop_samples(),
        }
    }
}
