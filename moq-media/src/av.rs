use std::time::Duration;

use anyhow::Result;
use image::RgbaImage;
use strum::{Display, EnumString, VariantNames};

#[derive(Copy, Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channel_count: u32,
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
    fn smoothed_peak_normalized(&self) -> Option<f32> {
        None
    }
}

pub trait AudioEncoder: AudioEncoderInner {
    fn with_preset(preset: AudioPreset) -> Result<Self>
    where
        Self: Sized;
}
pub trait AudioEncoderInner: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> hang::catalog::AudioConfig;
    fn push_samples(&mut self, samples: &[f32]) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

impl AudioEncoderInner for Box<dyn AudioEncoder> {
    fn name(&self) -> &str {
        (&**self).name()
    }

    fn config(&self) -> hang::catalog::AudioConfig {
        (&**self).config()
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        (&mut **self).push_samples(samples)
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        (&mut **self).pop_packet()
    }
}

pub trait AudioDecoder: Send + 'static {
    fn new(config: &hang::catalog::AudioConfig, target_format: AudioFormat) -> Result<Self>
    where
        Self: Sized;
    fn push_packet(&mut self, packet: hang::Frame) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PixelFormat {
    Rgba,
    Bgra,
}

impl Default for PixelFormat {
    fn default() -> Self {
        PixelFormat::Rgba
    }
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

pub trait VideoEncoder: VideoEncoderInner {
    fn with_preset(preset: VideoPreset) -> Result<Self>
    where
        Self: Sized;
}

pub trait VideoEncoderInner: Send + 'static {
    fn name(&self) -> &str;
    fn config(&self) -> hang::catalog::VideoConfig;
    fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;
    fn pop_packet(&mut self) -> Result<Option<hang::Frame>>;
}

impl VideoEncoderInner for Box<dyn VideoEncoder> {
    fn name(&self) -> &str {
        (&**self).name()
    }

    fn config(&self) -> hang::catalog::VideoConfig {
        (&**self).config()
    }

    fn push_frame(&mut self, frame: VideoFrame) -> Result<()> {
        (&mut **self).push_frame(frame)
    }

    fn pop_packet(&mut self) -> Result<Option<hang::Frame>> {
        (&mut **self).pop_packet()
    }
}

pub trait VideoDecoder: Send + 'static {
    fn new(config: &hang::catalog::VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where
        Self: Sized;
    fn name(&self) -> &str;
    fn pop_frame(&mut self) -> Result<Option<DecodedFrame>>;
    fn push_packet(&mut self, packet: hang::Frame) -> Result<()>;
    fn set_viewport(&mut self, w: u32, h: u32);
}

pub struct DecodedFrame {
    pub frame: image::Frame,
    pub timestamp: Duration,
}

impl DecodedFrame {
    pub fn img(&self) -> &RgbaImage {
        self.frame.buffer()
    }
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum TrackKind {
    Audio,
    Video,
}

impl TrackKind {
    pub fn from_name(name: &str) -> Option<Self> {
        if name.starts_with("audio-") {
            Some(Self::Audio)
        } else if name.starts_with("video-") {
            Some(Self::Video)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum AudioCodec {
    Opus,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames)]
#[strum(serialize_all = "lowercase")]
pub enum VideoCodec {
    H264,
    Av1,
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
    pub fn all() -> [VideoPreset; 4] {
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

#[derive(Clone, Default)]
pub struct DecodeConfig {
    pub pixel_format: PixelFormat,
}

#[derive(Clone, Default)]
pub struct PlaybackConfig {
    pub playback: DecodeConfig,
    pub quality: Quality,
}
