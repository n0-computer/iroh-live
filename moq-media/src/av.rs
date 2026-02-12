use std::time::Duration;

use anyhow::Result;
use hang::catalog::{AudioConfig, VideoConfig};
use image::RgbaImage;
use strum::{Display, EnumString, VariantNames};

#[derive(Copy, Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channel_count: u32,
}

impl AudioFormat {
    pub fn mono_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 1,
        }
    }

    pub fn stereo_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 2,
        }
    }

    pub fn from_hang_config(config: &AudioConfig) -> Self {
        Self {
            channel_count: config.channel_count,
            sample_rate: config.sample_rate,
        }
    }
}

pub trait Decoders {
    type Audio: AudioDecoder;
    type Video: VideoDecoder;
}

pub trait AudioSource: Send + 'static {
    fn cloned_boxed(&self) -> Box<dyn AudioSource>;
    fn format(&self) -> AudioFormat;
    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>>;
}

pub trait AudioSink: AudioSinkHandle {
    fn format(&self) -> Result<AudioFormat>;
    fn push_samples(&mut self, buf: &[f32]) -> Result<()>;
    fn handle(&self) -> Box<dyn AudioSinkHandle>;
}

pub trait AudioSinkHandle: Send + 'static {
    fn pause(&self);
    fn resume(&self);
    fn is_paused(&self) -> bool;
    fn toggle_pause(&self);
    /// Smoothed peak, normalized to 0..1.
    // TODO: document how smoothing and normalization are expected
    fn smoothed_peak_normalized(&self) -> Option<f32> {
        None
    }
}

pub trait AudioEncoderFactory: AudioEncoder {
    const ID: &str;
    fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self>
    where
        Self: Sized;
}

pub trait AudioEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

impl AudioEncoder for Box<dyn AudioEncoder> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn config(&self) -> AudioConfig {
        (**self).config()
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        (**self).push_samples(samples)
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        (**self).pop_packet()
    }
}

pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized;
    fn push_packet(&mut self, packet: hang::Frame) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub enum PixelFormat {
    #[default]
    Rgba,
    Bgra,
}

#[derive(Clone, Debug)]
pub struct VideoFormat {
    pub pixel_format: PixelFormat,
    pub dimensions: [u32; 2],
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub format: VideoFormat,
    pub raw: bytes::Bytes,
}

pub trait VideoSource: Send + 'static {
    fn name(&self) -> &str;
    fn format(&self) -> VideoFormat;
    fn pop_frame(&mut self) -> Result<Option<VideoFrame>>;
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

pub trait VideoEncoderFactory: VideoEncoder {
    const ID: &str;

    fn with_preset(preset: VideoPreset) -> Result<Self>
    where
        Self: Sized;
}

pub trait VideoEncoder: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> VideoConfig;
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

impl VideoEncoder for Box<dyn VideoEncoder> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn config(&self) -> VideoConfig {
        (**self).config()
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        (**self).push_frame(frame)
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        (**self).pop_packet()
    }
}

pub trait VideoDecoder: Send + 'static {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized;
    fn name(&self) -> &str;
    fn pop_frame(&mut self) -> Result<Option<DecodedFrame>>;
    fn push_packet(&mut self, packet: hang::Frame) -> Result<()>;
    fn set_viewport(&mut self, w: u32, h: u32);
}

#[derive(derive_more::Debug)]
pub struct DecodedFrame {
    #[debug(skip)]
    pub frame: image::Frame,
    pub timestamp: Duration,
}

impl DecodedFrame {
    pub fn img(&self) -> &RgbaImage {
        self.frame.buffer()
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum AudioCodec {
    Opus,
}

/// Available video encoder implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, EnumString, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum VideoCodec {
    /// Software H.264 via openh264 (always available).
    H264,
    /// Software AV1 via rav1e.
    #[cfg(feature = "av1")]
    Av1,
    /// Hardware H.264 via macOS VideoToolbox.
    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    #[strum(serialize = "h264-vtb")]
    VtbH264,
    /// Hardware H.264 via Linux VAAPI.
    #[strum(serialize = "h264-vaapi")]
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    VaapiH264,
}

impl VideoCodec {
    /// Returns all encoder kinds that are compiled in.
    pub fn available() -> Vec<Self> {
        vec![
            Self::H264,
            #[cfg(feature = "av1")]
            Self::Av1,
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264,
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264,
        ]
    }

    /// Returns the best available encoder: hardware if available, otherwise software H.264.
    pub fn best_available() -> Self {
        #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
        {
            return Self::VtbH264;
        }
        #[cfg(all(target_os = "linux", feature = "vaapi"))]
        {
            return Self::VaapiH264;
        }
        #[allow(
            unreachable_code,
            reason = "fallback when no HW encoder is compiled in"
        )]
        Self::H264
    }

    /// Whether this is a hardware-accelerated encoder.
    pub fn is_hardware(self) -> bool {
        match self {
            Self::H264 => false,
            #[cfg(feature = "av1")]
            Self::Av1 => false,
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => true,
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => true,
        }
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::H264 => "H.264 (Software)",
            #[cfg(feature = "av1")]
            Self::Av1 => "AV1 (Software)",
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            Self::VtbH264 => "H.264 (VideoToolbox)",
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            Self::VaapiH264 => "H.264 (VAAPI)",
        }
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Ord, PartialOrd)]
pub enum VideoPreset {
    #[strum(serialize = "180p")]
    P180,
    #[strum(serialize = "360p")]
    P360,
    #[strum(serialize = "720p")]
    P720,
    #[strum(serialize = "1080p")]
    P1080,
}

impl VideoPreset {
    pub fn all() -> [Self; 4] {
        [Self::P180, Self::P360, Self::P720, Self::P1080]
    }

    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::P180 => (320, 180),
            Self::P360 => (640, 360),
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
        }
    }

    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    pub fn fps(&self) -> u32 {
        30
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq)]
#[strum(serialize_all = "lowercase")]
pub enum AudioPreset {
    Hq,
    Lq,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Default)]
#[strum(serialize_all = "lowercase")]
pub enum Quality {
    Highest,
    #[default]
    High,
    Mid,
    Low,
}

#[derive(Clone, Debug, Default)]
pub struct DecodeConfig {
    pub pixel_format: PixelFormat,
}

#[derive(Clone, Debug, Default)]
pub struct PlaybackConfig {
    pub decode_config: DecodeConfig,
    pub quality: Quality,
}
